# Rust Ptrace Future Attempt

This is my attempt at implementing a Rust future for ptrace events. Still WIP.

We do not use tokio, or mio. Instead implement our own executor and reactor which
communicate with futures through thread local state.
