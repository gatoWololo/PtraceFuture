use std::cell::RefCell;
use std::collections::HashMap;
use nix::errno::Errno;
use nix::unistd::{Pid};
use futures::task::Waker;
use nix::Error::Sys;

// Currently libc::siginfo_t does not have the si_pid field.
// So we go to C to fetch the pid_t that was set by waitid().
extern "C" {
    fn getPid(infop: *mut libc::siginfo_t) -> libc::pid_t;
}

thread_local! {
    pub static PID_WAKER: RefCell<HashMap<Pid, Waker>> = RefCell::new(HashMap::new());
}

pub fn register(pid: Pid, waker: Waker) {
    PID_WAKER.with(|pid_waker| {
        pid_waker.borrow_mut().insert(pid, waker);
    });
}

pub fn wait_for_event() {
    let ignored = 0;
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
        }
    }
}

