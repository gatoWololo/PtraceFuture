use futures::core_reexport::pin::Pin;
use futures::future::Future;
use futures::task::{Context, Poll};

use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{Pid};

use crate::executor::PID_WAKER;

/// Future representing calling waitpid() on a Pid.
pub struct PtraceEvent {
    pub pid: Pid,
}

impl Future for PtraceEvent {
    type Output = WaitStatus;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        println!("future polling!");
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)).expect("Unable to waitpid from poll") {
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
