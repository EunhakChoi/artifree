//! One-shot sessions as real processes (decision 0039).
//!
//! This file holds the checks that run on every platform: a session that ends
//! at its TTL leaves no directory, a persistent server still reports its mode,
//! and contradictory flags create nothing. The Unix checks (signals, modes, the
//! sweep, stopping at an exact startup stage) are in `unix.rs` beside this file.
//! The file is not gated as a whole, so this crate stays documented on Windows.
//!
//! Every wait is bounded and observes a condition (a line, an exit status). A
//! child is killed and reaped when its guard is dropped.

mod support;
#[cfg(unix)]
mod unix;

use std::path::Path;
use std::process::Command;

use support::{Root, Server, TestResult, data_dir_of};

#[test]
fn ttl_expiry_ends_the_session_and_removes_the_directory() -> TestResult {
    let root = Root::new()?;
    let mut server = Server::start(
        &[
            "serve",
            "--one-shot-ttl-seconds",
            "1",
            "--session-root",
            root.arg(),
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ],
        &[],
    )?;
    // Startup consumes the lifetime; expiry need not produce a ready line.
    let status = server.wait_exit()?;
    assert!(status.success(), "TTL expiry is a normal end: {status:?}");
    assert_eq!(root.entries()?, 0);
    server.wait_stderr_contains("session ended")?;
    Ok(())
}

#[test]
fn a_one_shot_ready_line_identifies_its_live_directory() -> TestResult {
    let root = Root::new()?;
    let mut server = Server::start(
        &[
            "serve",
            "--one-shot-ttl-seconds",
            "120",
            "--session-root",
            root.arg(),
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ],
        &[],
    )?;
    let ready = server.ready()?;
    let data_dir = data_dir_of(&ready)?;
    assert_eq!(ready["mode"], "one-shot");
    assert_eq!(ready["session_ttl_seconds"], 120);
    assert_eq!(data_dir.parent(), Some(root.path()), "{ready}");
    assert_eq!(
        Path::new(ready["token_file"].as_str().ok_or("no token_file")?),
        data_dir.join("access-token")
    );
    assert!(data_dir.join("access-token").is_file());

    assert_eq!(
        root.entries()?,
        1,
        "the ready session has its live directory"
    );
    server.wait_stderr_contains("nothing is kept")?;
    // This case observes readiness, not TTL cleanup. The owned process guard
    // kills and reaps it; Root removes only this test's scratch directory.
    drop(server);
    Ok(())
}

#[test]
fn a_persistent_server_reports_its_mode_and_directory() -> TestResult {
    let root = Root::new()?;
    let data = root.path().join("data");
    let mut server = Server::start(
        &[
            "serve",
            "--data-dir",
            data.to_str().ok_or("data path")?,
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ],
        &[],
    )?;
    let ready = server.ready()?;
    assert_eq!(ready["mode"], "persistent");
    assert_eq!(data_dir_of(&ready)?, data);
    assert!(ready.get("session_ttl_seconds").is_none(), "{ready}");
    drop(server);
    assert!(data.is_dir(), "a persistent directory is kept");
    Ok(())
}

#[test]
fn contradictory_or_invalid_flags_create_nothing() -> TestResult {
    let root = Root::new()?;
    let data = root.path().join("never");
    let cases: [Vec<&str>; 3] = [
        vec![
            "--data-dir",
            data.to_str().ok_or("data path")?,
            "--one-shot-ttl-seconds",
            "5",
            "--session-root",
            root.arg(),
        ],
        vec!["--one-shot-ttl-seconds", "+5", "--session-root", root.arg()],
        vec!["--session-root", root.arg()],
    ];
    for flags in cases {
        let output = Command::new(support::BIN)
            .arg("serve")
            .args(&flags)
            .output()?;
        assert_eq!(output.status.code(), Some(1), "{flags:?}");
        assert!(output.stdout.is_empty(), "{flags:?}");
        assert_eq!(root.entries()?, 0, "{flags:?} must create nothing");
        assert!(!data.exists(), "{flags:?}");
    }
    Ok(())
}
