
# Rust Futures From Scratch

This is my attempt at implementing a Rust future "from scratch" for process events.

We do not use Tokio, or Mio. Instead implement our own executor and reactor which
communicate with futures through thread local state.

This is an example implementation for (TODO this blog).
