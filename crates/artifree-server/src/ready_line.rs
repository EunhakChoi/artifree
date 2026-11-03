//! The readiness line of `serve`: one JSON object on stdout that a supervisor
//! waits for (decisions 0003 and 0039).

use std::io::Write;
use std::path::Path;

use artifree_server::serve::Ready;
use serde::Serialize;

/// Which storage mode the server runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A data directory the operator chose and keeps.
    Persistent,
    /// A temporary session with the given lifetime.
    OneShot {
        /// Seconds after the process start that the session ends.
        ttl_seconds: u64,
    },
}

#[derive(Serialize)]
struct ReadyLine<'a> {
    event: &'static str,
    api_url: &'a str,
    content_origin: &'a str,
    token_file: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loopback_api_url: Option<&'a str>,
    mode: &'static str,
    data_dir: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_ttl_seconds: Option<u64>,
}

/// The line, without its newline. A path that is not valid UTF-8 is an error:
/// the line is JSON text, and a lossy path would point somewhere else.
pub fn render(ready: &Ready, mode: &Mode) -> Result<String, String> {
    let line = ReadyLine {
        event: "ready",
        api_url: &ready.api_url,
        content_origin: &ready.content_origin,
        token_file: &ready.token_file,
        web_url: ready.web_url.as_deref(),
        loopback_api_url: ready.loopback_api_url.as_deref(),
        mode: match mode {
            Mode::Persistent => "persistent",
            Mode::OneShot { .. } => "one-shot",
        },
        data_dir: &ready.data_dir,
        session_ttl_seconds: match mode {
            Mode::Persistent => None,
            Mode::OneShot { ttl_seconds } => Some(*ttl_seconds),
        },
    };
    serde_json::to_string(&line).map_err(|error| {
        format!("Could not build the readiness line (a path may not be valid UTF-8): {error}")
    })
}

/// Writes the line and a newline to stdout and flushes it.
pub fn announce(ready: &Ready, mode: &Mode) -> Result<(), String> {
    write_line(&mut std::io::stdout().lock(), &render(ready, mode)?)
}

fn write_line(out: &mut impl Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|error| format!("Could not write the readiness line: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ready(data_dir: PathBuf) -> Ready {
        Ready {
            api_url: "http://127.0.0.1:1".into(),
            web_url: None,
            loopback_api_url: None,
            content_origin: "http://127.0.0.1:2".into(),
            token_file: data_dir.join("access-token"),
            data_dir,
        }
    }

    #[test]
    fn persistent_line_has_mode_and_data_dir_but_no_ttl() -> Result<(), Box<dyn std::error::Error>>
    {
        let line = render(&ready(PathBuf::from("d")), &Mode::Persistent)?;
        let json: serde_json::Value = serde_json::from_str(&line)?;
        assert_eq!(json["event"], "ready");
        assert_eq!(json["mode"], "persistent");
        assert_eq!(json["data_dir"], "d");
        assert!(json.get("session_ttl_seconds").is_none(), "{line}");
        assert!(json.get("web_url").is_none(), "{line}");
        Ok(())
    }

    #[test]
    fn one_shot_line_adds_the_lifetime() -> Result<(), Box<dyn std::error::Error>> {
        let line = render(
            &ready(PathBuf::from("d")),
            &Mode::OneShot { ttl_seconds: 90 },
        )?;
        let json: serde_json::Value = serde_json::from_str(&line)?;
        assert_eq!(json["mode"], "one-shot");
        assert_eq!(json["session_ttl_seconds"], 90);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_an_error_not_a_panic() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(vec![b'd', 0xff]));
        let error = render(&ready(bad), &Mode::OneShot { ttl_seconds: 1 }).err();
        assert!(
            error.is_some_and(|message| message.contains("UTF-8")),
            "a lossy or panicking line would hide the wrong path"
        );
    }

    #[test]
    fn a_failed_write_is_an_error() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("closed"))
            }
        }
        let error = write_line(&mut Broken, "{}").err();
        assert!(error.is_some_and(|message| message.contains("closed")));
    }
}
