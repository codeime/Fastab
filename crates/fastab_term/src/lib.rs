//! Shared terminal subprocess support, independent of the PTY main loop.

pub mod process_output;

// Compile the production outbox's tests in the library harness too. Some of
// its APIs are consumed only by the binary's connection supervisor.
#[cfg(test)]
#[allow(dead_code)]
#[path = "ipc/outbox.rs"]
mod outbox;
