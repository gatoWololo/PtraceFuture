use futures::future::Future;
use futures::future::BoxFuture;
use futures::task::{Context, Poll, Waker, ArcWake};

use nix::errno::Errno;
use nix::sys::wait::{WaitStatus};
use nix::unistd::{Pid};
use nix::Error::Sys;

use std::sync::Arc;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::collections::btree_map::Entry;

// Currently libc::siginfo_t does not have the si_pid field.
// So we go to C to fetch the pid_t that was set by waitid().
extern "C" {
    fn getPid(infop: *mut libc::siginfo_t) -> libc::pid_t;
}

type TaskId = u32;

// Our future will add an entry here mapping it's Pid to the Waker we passed it
// as part of it's context. When a waitid() returns a Pid, the executor will:
// let waker = PID_WAKER[pid];
// waker.wake_by_ref();

// The waker will set its TaskId in NEXT_TASKID. The executor will read it,
// and schedule the Task with that taskId to run.
thread_local! {
    pub static PID_WAKER: RefCell<HashMap<Pid, Waker>> = RefCell::new(HashMap::new());
    pub static NEXT_TASKID: RefCell<TaskId> = RefCell::new(0);
}

/// This is our futures runtime. It is responsible for accepting futures to run,
/// polling them, registering the Pid the future is waiting for, and scheduling,
/// the next task to run.

/// When we add a future we allow it to poll once. If it's pending, the future
/// registers it's (Pid, Waker) pair in thread local state.
/// The main executor loop blocks on waitid() until an event comes and scheudules
/// the task waiting for this event. This avoids busy waiting.

/// This executor is meant to be used in a ptrace context. So all tasks run
/// in the main process, as child-threads of a ptracer are not allowed to ptrace or
/// wait on the tracee.
pub struct WaitidExecutor<'a> {
    waiting_tasks: BTreeMap<TaskId, BoxFuture<'a, WaitStatus>>,
    // Unique task id generator counter.
    counter: TaskId,
}

struct WaitidWaker {
    task_id: TaskId,
}

impl ArcWake for WaitidWaker {
    /// Set the TLS NEXT_TASKID to the ID of the next task to run.
    /// The executor will check this TLS to know who to run next.
    fn wake_by_ref(arc_self: &Arc<Self>) {
        NEXT_TASKID.with(|t| {
            *t.borrow_mut() = arc_self.task_id;
        });
    }
}

impl<'a> WaitidExecutor<'a> {
    pub fn new() -> Self {
        WaitidExecutor {
            counter: 0,
            waiting_tasks: BTreeMap::new(),
        }
    }

    pub fn get_next_task_id(&mut self) -> TaskId {
        self.counter += 1;
        self.counter
    }

    /// Queue up all the tasks we want to run. Allow the task to run (poll) once
    /// If it's ready, we do not bother adding it to our task_queue.
    /// If the tasks is still pending, we add it to our task_queue.
    pub fn add_future<F>(&mut self, future: F)
    where
        F: Future<Output = WaitStatus> + Send + 'a,
    {
        println!("New future added!");

        // Create waker.
        let task_id = self.get_next_task_id();
        let waker = Arc::new(WaitidWaker { task_id }).into_waker();

        // Pin it, and box it up for storing.
        let mut future: BoxFuture<'a, WaitStatus> = Box::pin(future);

        // After this poll, our task has registered it's pid in the task queue
        // this is done in <PtraceEvent as Future>::poll.
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
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

    pub fn run_all(&mut self) {
        println!("Running all futures.");
        let ignored = 0;

        loop {
            let mut siginfo: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let ret = unsafe {
                libc::waitid(
                    libc::P_ALL,
                    ignored,
                    &mut siginfo as *mut libc::siginfo_t,
                    libc::WNOWAIT | libc::WEXITED | libc::WSTOPPED,
                )
            };

            // Block here for actual events to come.
            match ret {
                -1 => {
                    let error = nix::Error::last();
                    if let Sys(Errno::ECHILD) = error {
                        println!("done!");
                        return;
                    } else {
                        panic!("Unexpected error reason: {}", error);
                    }
                }
                _ => {
                    let pid = unsafe { getPid(&mut siginfo as *mut libc::siginfo_t) };
                    println!("waitid() = {}", pid);

                    // After this call, NEXT_TASKID will be set.
                    PID_WAKER.with(|hashtable| {
                        hashtable
                            .borrow_mut()
                            .get(&Pid::from_raw(pid))
                            .expect("No such entry, should have been there.")
                            .wake_by_ref();
                    });

                    let task_id = NEXT_TASKID.with(|id| *id.borrow());
                    let waker = Arc::new(WaitidWaker { task_id }).into_waker();

                    match self.waiting_tasks.entry(task_id) {
                        Entry::Occupied(mut task) => {
                            let poll = task
                                .get_mut()
                                .as_mut()
                                .poll(&mut Context::from_waker(&waker));

                            match poll {
                                Poll::Pending => {} // Made progress but still pending.
                                Poll::Ready(_) => {
                                    task.remove_entry();
                                }
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
