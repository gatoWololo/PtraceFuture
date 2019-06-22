#![feature(async_await)]
use futures::future::Future;
use nix::sys::wait::{WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};

use wait_status_future::WaitStatusEvent;
use wait_status_future::Executor;

fn main() -> nix::Result<()> {
    match fork()? {
        ForkResult::Parent { child: child1 } => {
            match fork()? {
                ForkResult::Parent { child: child2 } => {
                    let mut pool = Executor::new();

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
    let child = WaitStatusEvent { pid };

    async {
        let res: WaitStatus = child.await;
        println!("Return Value {:?}!", res);
        res
    }
}
