#![feature(async_await)]

use futures::future::Future;
use nix::sys::wait::{WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};

mod executor;
mod ptrace_event;

use crate::executor::WaitidExecutor;
use crate::ptrace_event::PtraceEvent;


fn main() -> nix::Result<()> {
    match fork()? {
        ForkResult::Parent { child: child1 } => {
            match fork()? {
                ForkResult::Parent { child: child2 } => {
                    let mut pool = WaitidExecutor::new();

                    pool.add_future(wait_for_child(child1));
                    pool.add_future(wait_for_child(child2));
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

fn wait_for_child(pid: Pid) -> impl Future<Output=WaitStatus> {
    let child = PtraceEvent { pid };

    async {
        let res: WaitStatus = child.await;
        println!("Return Value {:?}!", res);
        res
    }
}
