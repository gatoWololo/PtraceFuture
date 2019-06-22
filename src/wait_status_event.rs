use futures::core_reexport::pin::Pin;
use futures::future::Future;
use futures::task::{Context, Poll};

use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{Pid};

use crate::reactor;

/// Future representing calling waitpid() on a Pid.
pub struct WaitStatusEvent {
    pub pid: Pid,
}

impl Future for WaitStatusEvent {
    type Output = WaitStatus;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        println!("future polling!");
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)).expect("Unable to waitpid from poll") {
            WaitStatus::StillAlive => {
                // let reactor know this future is waiting on self.pid
                reactor::register(self.pid, cx.waker().clone());
                Poll::Pending
            }
            w => Poll::Ready(w),
        }
    }
}
