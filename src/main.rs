#![feature(async_await)]

use futures::core_reexport::pin::Pin;
use futures::future::Future;
use futures::task::Context;
use futures::task::Poll;

use nix::sys::wait::waitpid;
use nix::sys::wait::WaitPidFlag;
use nix::sys::wait::WaitStatus;
use nix::unistd::fork;
use nix::unistd::ForkResult;
use nix::unistd::Pid;
use nix::errno::Errno;
use nix::Error::Sys;

use futures::future::FutureObj;
use futures::task::Waker;
use std::sync::Arc;

use std::collections::HashMap;
use std::collections::BTreeMap;
use futures::task::ArcWake;
use std::cell::RefCell;

// Currently libc::siginfo_t does not have the si_pid field.
// So we go to C to fetch the pid_t that was set by waitid().
extern "C" {
    fn getPid(infop: *mut libc::siginfo_t ) -> libc::pid_t;
}

type TaskId = u32;

// The future will add an entry here mapping it's Pid to the Waker we passed it
// as part of it's context. When a waitid() returns a Pid, the executor will:
// let waker = PID_WAKER[pid];
// waker.wake_by_ref();

// The waker will set its TaskId in NEXT_TASKID. The executor will read it,
// and schedule the Task with that taskId to run.
thread_local! {
    static PID_WAKER: RefCell<HashMap<Pid, Waker>> = RefCell::new(HashMap::new());
    static NEXT_TASKID: RefCell<TaskId> = RefCell::new(0);
}

/// This is our futures runtime. It is responsible for accepting futures to run,
/// polling them, registering the Pid the future is waiting for, and scheduling,
/// the next task to run.

/// When we add a future we allow it to poll once. If it's pending, the future
/// registers it's (Pid, Waker) pair in thread local state.
/// The main executor loop blocks on waitid() until an event comes and scheudules
/// the task waiting for this event. This avoids all busy waiting.

/// This executor is meant to be used in a ptrace context. So all tasks run
/// in a single thread, as a ptrace tracer process is the only who can wait
/// on its tracees, it's threads can't.
struct WaitidExecutor<'a> {
    waiting_tasks: BTreeMap<TaskId, FutureObj<'a, WaitStatus>>,
    // Unique task id generator counter.
    counter: TaskId,
}

struct WaitidWaker {
    task_id: TaskId,
}

impl ArcWake for WaitidWaker {
    /// Set the TLS NEXT_TASKID who the id of the next task to run is.
    /// The executor will check this TLS to know who to run next.
    fn wake_by_ref(arc_self: &Arc<Self>) {
        NEXT_TASKID.with(|t| {
            *t.borrow_mut() = arc_self.task_id;
        });
    }
}

impl<'a> WaitidExecutor<'a> {
    fn new() -> Self {
        WaitidExecutor { counter: 0, waiting_tasks: BTreeMap::new() }
    }

    fn get_next_task_id(&mut self) -> TaskId {
        self.counter += 1;
        self.counter
    }

    /// Queue up all the tasks we want to run. Allow the task to run (poll) once
    /// If it's ready, we do not bother adding it to our task_queue.
    /// If the tasks is still pending, we add it to our task_queue.
    fn add_future(&mut self, mut future: FutureObj<'a, WaitStatus>) {
        println!("New future added!");
        let task_id = self.get_next_task_id();

        let waker = Arc::new(WaitidWaker { task_id }).into_waker();
        let mut ctx: Context = Context::from_waker(&waker);

        let pin_task = Pin::new(&mut future);

        // After this poll, our task has registered it's pid in the task queue
        // this is done in <PtraceEvent as Future>::poll.
        match pin_task.poll(&mut ctx){
            Poll::Pending => {
                println!("New future returned Pending!");
                self.waiting_tasks.insert(task_id, future);
            }
            Poll::Ready(_) => {
                println!("New future returned Ready!");
                // Task is done, no need to add it to our hash table.
                // get droped
            }
        }
    }

    // TODO: I don't know how to get BoxedFuture to work here instead of FutureObj.
    fn run_all(&mut self) {
        println!("Running all futures.");


        loop {
            let mut siginfo: libc::siginfo_t = unsafe { std::mem::zeroed() };
            match unsafe {
                let ignored = 0;
                // Block here for actual events to come.
                libc::waitid(libc::P_ALL, ignored, &mut siginfo as *mut libc::siginfo_t ,
                       libc::WNOWAIT | libc::WEXITED | libc::WSTOPPED ) } {
                -1 => {
                    let error = nix::Error::last();
                    if let Sys(Errno::ECHILD) = error {
                        // We are done.
                        println!("done!");
                        return;
                    } else {
                        panic!("Unexpected error reason: {}", error);
                    }
                }
                _ => {
                    let pid = unsafe {getPid(&mut siginfo as *mut libc::siginfo_t)};
                    println!("waitid() = {}", pid);

                    // after this call, NEXT_TASKID will be set.
                    PID_WAKER.with(|hashtable| {
                        hashtable.borrow_mut().get(&Pid::from_raw(pid)).
                            expect("No such entry, should have been there.").
                            wake_by_ref();
                    });

                    let task_id: TaskId = NEXT_TASKID.with(|id| *id.borrow());
                    let waker = Arc::new(WaitidWaker { task_id }).into_waker();
                    let mut ctx: Context = Context::from_waker(&waker);

                    use std::collections::btree_map::Entry;
                    match self.waiting_tasks.entry(task_id) {
                        Entry::Occupied(mut o) => {
                            let task = Pin::new(o.get_mut());
                            match task.poll(&mut ctx) {
                                Poll::Pending => { } // Made progress but still pending.
                                Poll::Ready(_) => { o.remove_entry(); }
                            }
                        }
                        Entry::Vacant(_) => {
                            panic!("No task waiting for this pid. This should be impossible.");
                        }
                    }
                }
            }
        }
    }
}


/// Future representing calling waitpid() on a Pid.
struct PtraceEvent {
    pid: Pid,
}

impl Future for PtraceEvent {
    type Output = WaitStatus;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        println!("future polling!");
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)).
            expect("Unable to waitpid from poll") {
                WaitStatus::StillAlive => {
                    // Inform TLS which Pid, we're waiting for. The waker contains the
                    // unique taskId that the executor assigned to us. The executor will
                    // use this taskId to schedule us later.
                    PID_WAKER.with(|pid_waker| {
                        pid_waker.borrow_mut().insert(self.pid, cx.waker().clone());
                    });
                    Poll::Pending
                }
                w => Poll::Ready(w),
            }
    }
}

fn main() -> nix::Result<()> {
    match fork()? {
        ForkResult::Parent { child: child1 } => {
            match fork()? {
                ForkResult::Parent { child: child2 } => {
                    let mut pool = WaitidExecutor::new();
                    let child1 = PtraceEvent { pid: child1 };
                    let child2 = PtraceEvent { pid: child2 };

                    let future1 = async {
                        let res: WaitStatus = child1.await;
                        println!("Return Value {:?}!", res);
                        res
                    };

                    let future2 = async {
                        let res: WaitStatus = child2.await;
                        println!("Return Value {:?}!", res);
                        res
                    };

                    pool.add_future(FutureObj::new(Box::new(future1)));
                    pool.add_future(FutureObj::new(Box::new(future2)));
                    pool.run_all();
                }
                ForkResult::Child => {
                    nix::unistd::sleep(1);
                    println!("Child done!");
                }
            }
        }
        ForkResult::Child => {
            nix::unistd::sleep(1);
            println!("Child done!");
        }
    }

    Ok(())
}
