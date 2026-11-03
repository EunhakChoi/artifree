//! The one-shot session as a real process (decision 0039). The tests need
//! Unix signals and permissions, so they live in `one_shot_session/unix.rs`
//! and this target is empty elsewhere.

#[cfg(unix)]
#[path = "one_shot_session/unix.rs"]
mod unix;
