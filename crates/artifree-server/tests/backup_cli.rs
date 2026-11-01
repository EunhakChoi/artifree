//! The `backup` and `restore` commands of the installed binary, against real
//! `serve` processes.

use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_artifree-server"))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "artifree-backup-cli-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    child: Child,
    address: String,
    token: String,
    token_file: PathBuf,
}

impl Server {
    fn start(data_dir: &Path) -> Result<Self, Box<dyn Error>> {
        let mut child = binary()
            .args(["serve", "--data-dir"])
            .arg(data_dir)
            .args([
                "--api-listen",
                "127.0.0.1:0",
                "--content-listen",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)?;
        let ready: Value = serde_json::from_str(&line)?;
        let api_url = ready["api_url"].as_str().ok_or("api_url")?;
        let token_file = PathBuf::from(ready["token_file"].as_str().ok_or("token_file")?);
        let token = std::fs::read_to_string(&token_file)?.trim().to_owned();
        Ok(Self {
            child,
            address: api_url.trim_start_matches("http://").to_owned(),
            token,
            token_file,
        })
    }

    fn post(&self, key: &str, body: &Value) -> Result<(u16, Value), Box<dyn Error>> {
        let body = body.to_string();
        let request = format!(
            "POST /v1/artifacts HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Idempotency-Key: {key}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.address,
            self.token,
            body.len()
        );
        let mut stream = TcpStream::connect(&self.address)?;
        stream.write_all(request.as_bytes())?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        let status = response
            .split_whitespace()
            .nth(1)
            .ok_or("status line")?
            .parse()?;
        let (_, payload) = response.split_once("\r\n\r\n").ok_or("body")?;
        Ok((status, serde_json::from_str(payload)?))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn page(html: &str) -> Value {
    json!({ "title": "Kept", "files": [{ "path": "index.html", "encoding": "utf8", "content": html }] })
}

fn stdout_json(output: &Output) -> Result<Value, Box<dyn Error>> {
    assert!(
        output.status.success(),
        "failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "diagnostics on success");
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn a_backup_of_a_running_server_restores_into_a_serving_directory() -> TestResult {
    let scratch = Scratch::new("flow")?;
    let (live, snapshot, restored) = (
        scratch.0.join("live"),
        scratch.0.join("snapshot.sqlite3"),
        scratch.0.join("restored"),
    );
    let original_token;
    let artifact_id;
    {
        let server = Server::start(&live)?;
        original_token = server.token.clone();
        let (status, created) = server.post("backup-key-1", &page("<h1>one</h1>"))?;
        assert_eq!(status, 201, "{created}");
        artifact_id = created["artifact"]["id"].as_str().ok_or("id")?.to_owned();

        // The server is still running while the backup is taken.
        let output = binary()
            .arg("backup")
            .arg("--data-dir")
            .arg(&live)
            .arg("--output")
            .arg(&snapshot)
            .output()?;
        let summary = stdout_json(&output)?;
        assert_eq!(summary["event"], "backup");
        assert_eq!(summary["schema_version"], 5);

        // A second backup to the same name is refused and changes nothing.
        let before = std::fs::read(&snapshot)?;
        let again = binary()
            .arg("backup")
            .arg("--data-dir")
            .arg(&live)
            .arg("--output")
            .arg(&snapshot)
            .output()?;
        assert_eq!(again.status.code(), Some(1));
        assert!(again.stdout.is_empty());
        assert!(String::from_utf8(again.stderr)?.contains("already exists"));
        assert_eq!(std::fs::read(&snapshot)?, before);
    }

    let output = binary()
        .arg("restore")
        .arg("--input")
        .arg(&snapshot)
        .arg("--data-dir")
        .arg(&restored)
        .output()?;
    assert_eq!(stdout_json(&output)?["event"], "restore");

    let server = Server::start(&restored)?;
    assert_ne!(
        server.token, original_token,
        "a restored directory gets its own access token"
    );
    assert!(server.token_file.starts_with(&restored));
    let (status, replay) = server.post("backup-key-1", &page("<h1>one</h1>"))?;
    assert_eq!(status, 201, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["artifact"]["id"], artifact_id.as_str());
    let (status, reused) = server.post("backup-key-1", &page("<h1>other</h1>"))?;
    assert_eq!(status, 422, "{reused}");
    Ok(())
}

#[test]
fn a_restore_refuses_a_directory_that_holds_anything() -> TestResult {
    let scratch = Scratch::new("occupied")?;
    let (live, snapshot, target) = (
        scratch.0.join("live"),
        scratch.0.join("snapshot.sqlite3"),
        scratch.0.join("target"),
    );
    {
        let _server = Server::start(&live)?;
        let output = binary()
            .arg("backup")
            .arg("--data-dir")
            .arg(&live)
            .arg("--output")
            .arg(&snapshot)
            .output()?;
        stdout_json(&output)?;
    }
    std::fs::create_dir_all(&target)?;
    std::fs::write(target.join("access-token"), b"keep me")?;
    let output = binary()
        .arg("restore")
        .arg("--input")
        .arg(&snapshot)
        .arg("--data-dir")
        .arg(&target)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.contains("not empty"));
    assert_eq!(std::fs::read(target.join("access-token"))?, b"keep me");
    assert_eq!(std::fs::read_dir(&target)?.count(), 1, "nothing was added");
    Ok(())
}

#[test]
fn backup_and_restore_need_both_paths() -> TestResult {
    for args in [
        vec!["backup", "--data-dir", "/nowhere"],
        vec!["backup", "--output", "/nowhere"],
        vec!["restore", "--input"],
        vec!["restore", "--input", "a", "--input", "b", "--data-dir", "c"],
        vec!["backup", "--data-dir", "a", "--output", "b", "--extra"],
    ] {
        let output = binary().args(&args).output()?;
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8(output.stderr)?.contains("Unknown arguments."),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn help_describes_the_backup_commands() -> TestResult {
    let output = binary().arg("--help").output()?;
    let help = String::from_utf8(output.stdout)?;
    for fragment in [
        "artifree-server backup --data-dir <dir> --output <file>",
        "artifree-server restore --input <file> --data-dir <new-dir>",
    ] {
        assert!(help.contains(fragment), "help lacks {fragment:?}");
    }
    Ok(())
}
