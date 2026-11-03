//! The readiness line of `serve`: one JSON object on stdout that a supervisor
//! waits for (decisions 0003 and 0039).
//!
//! It is built from borrowed text, never from a path inside `json!`, so a
//! path that is not valid UTF-8 is an error and not a panic, and it is written
//! with `writeln!` so that a closed pipe or a full device is an error as well.

use std::io::Write;
use std::path::Path;

use artifree_server::serve::Ready;
use serde::Serialize;

#[derive(Serialize)]
struct Line<'a> {
    event: &'static str,
    mode: &'static str,
    api_url: &'a str,
    content_origin: &'a str,
    token_file: &'a str,
    data_dir: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loopback_api_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sweep: Option<SweepLine>,
}

#[derive(Serialize)]
struct SweepLine {
    performed: bool,
    removed: u32,
    kept: u32,
    failed: u32,
}

fn utf8<'a>(what: &str, path: &'a Path) -> Result<&'a str, String> {
    path.to_str().ok_or_else(|| {
        format!(
            "{what} {} is not valid UTF-8, so the readiness line cannot name it; use a UTF-8 path",
            path.display()
        )
    })
}

impl<'a> Line<'a> {
    fn of(ready: &'a Ready) -> Result<Self, String> {
        Ok(Self {
            event: "ready",
            mode: if ready.session.is_some() {
                "one_shot"
            } else {
                "persistent"
            },
            api_url: &ready.api_url,
            content_origin: &ready.content_origin,
            token_file: utf8("The token file", &ready.token_file)?,
            data_dir: utf8("The data directory", &ready.data_dir)?,
            web_url: ready.web_url.as_deref(),
            loopback_api_url: ready.loopback_api_url.as_deref(),
            session_ttl_seconds: ready.session.as_ref().map(|session| session.ttl.as_secs()),
            sweep: ready.session.as_ref().map(|session| SweepLine {
                performed: session.sweep.performed,
                removed: session.sweep.removed,
                kept: session.sweep.kept,
                failed: session.sweep.failed,
            }),
        })
    }
}

/// Writes the readiness line and flushes it.
///
/// # Errors
///
/// When a path in `ready` is not valid UTF-8, or the line cannot be written
/// or flushed (for example the reader has gone away).
pub fn write(out: &mut impl Write, ready: &Ready) -> Result<(), String> {
    let line = serde_json::to_string(&Line::of(ready)?)
        .map_err(|error| format!("cannot build the readiness line: {error}"))?;
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|error| format!("cannot write the readiness line: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use artifree_server::serve::{SessionReady, SweepReport};
    use std::path::PathBuf;
    use std::time::Duration;

    fn ready(data_dir: &str, session: Option<SessionReady>) -> Ready {
        Ready {
            api_url: "http://127.0.0.1:1".into(),
            web_url: None,
            loopback_api_url: None,
            content_origin: "http://127.0.0.1:2".into(),
            token_file: PathBuf::from(data_dir).join("access-token"),
            data_dir: PathBuf::from(data_dir),
            session,
        }
    }

    fn written(ready: &Ready) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let mut out = Vec::new();
        write(&mut out, ready)?;
        let text = String::from_utf8(out)?;
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 1, "one line: {text:?}");
        Ok(serde_json::from_str(&text)?)
    }

    #[test]
    fn a_persistent_line_names_its_mode_and_directory_and_nothing_of_a_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let line = written(&ready("/srv/artifree", None))?;
        assert_eq!(line["event"], "ready");
        assert_eq!(line["mode"], "persistent");
        assert_eq!(line["data_dir"], "/srv/artifree");
        // Joined by the platform, so the separator is the platform's.
        let token_file = PathBuf::from("/srv/artifree").join("access-token");
        assert_eq!(line["token_file"], token_file.to_string_lossy().as_ref());
        assert_eq!(line["api_url"], "http://127.0.0.1:1");
        let object = line.as_object().ok_or("an object")?;
        for absent in [
            "session_ttl_seconds",
            "sweep",
            "web_url",
            "loopback_api_url",
        ] {
            assert!(!object.contains_key(absent), "{absent} in {line}");
        }
        Ok(())
    }

    #[test]
    fn a_one_shot_line_adds_the_lifetime_and_the_sweep() -> Result<(), Box<dyn std::error::Error>> {
        let session = SessionReady {
            ttl: Duration::from_secs(90),
            sweep: SweepReport {
                performed: true,
                removed: 2,
                kept: 3,
                failed: 1,
            },
        };
        let mut ready = ready("/tmp/artifree-session-0123456789abcdef", Some(session));
        ready.web_url = Some("http://127.0.0.1:1".into());
        let line = written(&ready)?;
        assert_eq!(line["mode"], "one_shot");
        assert_eq!(line["session_ttl_seconds"], 90);
        assert_eq!(
            line["sweep"],
            serde_json::json!({"performed": true, "removed": 2, "kept": 3, "failed": 1})
        );
        assert_eq!(line["web_url"], "http://127.0.0.1:1");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_an_error_and_writes_nothing() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let mut ready = ready("/x", None);
        ready.data_dir = PathBuf::from(OsString::from_vec(b"/tmp/\xff\xfe".to_vec()));
        let mut out = Vec::new();
        let Err(message) = write(&mut out, &ready) else {
            panic!("a non-UTF-8 data directory must be refused");
        };
        assert!(message.contains("data directory"), "{message}");
        assert!(message.contains("UTF-8"), "{message}");
        assert!(out.is_empty(), "nothing is written for a refused line");
        ready.data_dir = PathBuf::from("/x");
        ready.token_file = PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()));
        assert!(write(&mut out, &ready).is_err());
        assert!(out.is_empty());
    }

    /// A writer that fails like a closed pipe, or like a full device.
    struct Failing {
        fail_write: bool,
    }

    impl Write for Failing {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.fail_write {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(buffer.len())
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn a_failed_write_or_flush_is_an_error_not_a_panic() {
        for fail_write in [true, false] {
            let result = write(&mut Failing { fail_write }, &ready("/x", None));
            let Err(message) = result else {
                panic!("fail_write={fail_write}: a failing writer must give an error");
            };
            assert!(
                message.contains("cannot write the readiness line"),
                "{message}"
            );
        }
    }
}
