//! A real signal received during readiness must stop every later listener.

#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use artifree_server::changes;
use artifree_server::limits::RequestLimits;
use artifree_server::serve::{self, ServeOptions};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CHILD_DIR: &str = "ARTIFREE_SHUTDOWN_TEST_DIR";
const CHILD_SIGNAL: &str = "ARTIFREE_SHUTDOWN_TEST_SIGNAL";
const PATIENCE: Duration = Duration::from_secs(15);

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn a_single_signal_during_readiness_stops_later_listeners() -> TestResult {
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        return run_child(Path::new(&dir));
    }

    for signal in ["TERM", "INT"] {
        let dir = std::env::temp_dir().join(format!(
            "artifree-serve-shutdown-{}-{signal}",
            std::process::id()
        ));
        let mut child = RunningChild(
            Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "a_single_signal_during_readiness_stops_later_listeners",
                    "--nocapture",
                ])
                .env(CHILD_DIR, &dir)
                .env(CHILD_SIGNAL, signal)
                .spawn()?,
        );
        let deadline = Instant::now() + PATIENCE;
        let result = loop {
            if let Some(status) = child.0.try_wait()? {
                break Ok(status);
            }
            if Instant::now() >= deadline {
                break Err(format!("one SIG{signal} did not stop every late listener"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Reap before deleting the database, including when the test fails.
        drop(child);
        std::fs::remove_dir_all(dir)?;
        assert!(result?.success(), "SIG{signal} must cause graceful exit");
    }
    Ok(())
}

fn run_child(data_dir: &Path) -> TestResult {
    let options = ServeOptions {
        data_dir: data_dir.to_owned(),
        web_dir: None,
        api_address: "127.0.0.1:0".parse()?,
        loopback_api_address: Some("127.0.0.1:0".parse()?),
        content_address: "127.0.0.1:0".parse()?,
        content_origin: None,
        frame_ancestors: serve::DEFAULT_FRAME_ANCESTORS.to_owned(),
        preview_ttl: serve::DEFAULT_PREVIEW_TTL,
        limits: RequestLimits::default(),
        change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        api_origin: None,
        trusted_proxies: Vec::new(),
        allowed_hosts: Vec::new(),
        allowed_origins: Vec::new(),
        allow_remote_mcp: false,
    };
    let signal = std::env::var(CHILD_SIGNAL)?;
    serve::run(options, |_| {
        // The callback is synchronous: none of the three Axum listener
        // shutdown futures can start until this signal has been sent.
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(std::process::id().to_string())
            .status();
        assert!(
            matches!(&status, Ok(status) if status.success()),
            "kill -{signal} must succeed: {status:?}"
        );
    })?;
    Ok(())
}
