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

use futures::future::FutureObj;
use futures::task::Waker;
use std::sync::Arc;

use std::collections::HashMap;
use std::collections::BTreeMap;
use futures::task::ArcWake;
use std::cell::RefCell;

use nix::errno::errno;

extern "C" {
    fn getPid(infop: *mut libc::siginfo_t ) -> libc::pid_t;
}

type TaskId = u32;
// We need a way for our future to inform us what Pid it is waiting on.
// We solve that here. The future will tell us it's pid by accessing this thread
// local state by calling the waker's function. The executor chose an unique taskId
// for this specific future, the executor will check this taskIdToPid after letting
// the task poll once on Pending.

thread_local! {
    static taskPidToWaker: RefCell<HashMap<Pid, Waker>> = RefCell::new(HashMap::new());
    static taskIdToWakeUp: RefCell<TaskId> = RefCell::new(0);
}

struct Executor<'a> {
    // Task will tell us it's PID after the first time it runs.
    waiting_tasks: BTreeMap<TaskId, FutureObj<'a, WaitStatus>>,
    // Unique task id generator counter.
    counter: TaskId,
}

struct MyWaker {
    taskId: TaskId,
}

impl ArcWake for MyWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        taskIdToWakeUp.with(|t| {
            *t.borrow_mut() = arc_self.taskId;
        });
    }
}

impl<'a> Executor<'a> {
    fn new() -> Self {
        Executor { counter: 0, waiting_tasks: BTreeMap::new() }
    }

    fn get_next_task_id(&mut self) -> TaskId {
        self.counter += 1;
        self.counter
    }

    /// Queue up all the tasks we want to run.
    fn add_future(&mut self, mut future: FutureObj<'a, WaitStatus>) {
        println!("New future added!");
        let taskId = self.get_next_task_id();

        let waker = Arc::new(MyWaker { taskId }).into_waker();
        let mut ctx: Context = Context::from_waker(&waker);

        let pinnedTask = Pin::new(&mut future);

        // After this poll, our task has registered it's pid in the task queue
        // this is done in <PtraceEvent as Future>::poll.
        match pinnedTask.poll(&mut ctx){
            Poll::Pending => {
                println!("New future returned Pending!");
                self.waiting_tasks.insert(taskId, future);
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
        // Wait here for actual event to come
        let ignored = 0;
        let mut siginfo: libc::siginfo_t = unsafe { std::mem::zeroed() };

        loop {
            println!("Looping once");
            match unsafe {
                libc::waitid(libc::P_ALL, ignored, &mut siginfo as *mut libc::siginfo_t ,
                       libc::WNOWAIT | libc::WEXITED | libc::WSTOPPED ) } {
                -1 => {
                    use nix::errno::Errno;
                    use nix::Error::Sys;

                    let error = nix::Error::last();
                    if let Sys(Errno::ECHILD) = error {
                        // We are done.
                        println!("done!");
                    } else {
                        panic!("Unexpected error reason: {}", error);
                    }

                    break;
                }
                _ => {
                    let pid = unsafe {getPid(&mut siginfo as *mut libc::siginfo_t)};
                    println!("waitid() = {}", pid);
                    // after this call, taskIdToWakeUp will be set.
                    taskPidToWaker.with(|hashtable| {
                        hashtable.borrow_mut().get(&Pid::from_raw(pid)).
                            expect("No such entry, should have been there.").
                            wake_by_ref();
                    });

                    let taskId: TaskId = taskIdToWakeUp.with(|id| *id.borrow());
                    let waker = Arc::new(MyWaker { taskId }).into_waker();
                    let mut ctx: Context = Context::from_waker(&waker);

                    use std::collections::btree_map::Entry;
                    match self.waiting_tasks.entry(taskId) {
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
