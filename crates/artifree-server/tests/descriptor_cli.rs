//! Verify the installed binary's stdout/stderr and failure protocol.
use std::error::Error;
use std::process::Command;

#[test]
fn descriptor_is_machine_readable_and_exits_without_serving() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_artifree-server"))
        .arg("--describe")
        .output()?;
    assert!(
        output.status.success(),
        "Descriptor failed: {:?}",
        output.stderr
    );
    assert!(
        output.stderr.is_empty(),
        "Success emitted errors: {:?}",
        output.stderr
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(actual["name"], "Artifree");
    assert_eq!(actual["phase"], "early-development");
    assert_eq!(actual["http_api"], true);
    assert_eq!(actual["server_framework"], "axum");
    assert_eq!(actual["async_runtime"], "tokio");
    Ok(())
}

#[test]
fn invalid_arguments_fail_without_success_output() -> Result<(), Box<dyn Error>> {
    for args in [
        ["--serve", ""],
        ["--describe", "extra"],
        ["--version", "--describe"],
        ["serve", "--unknown"],
        ["serve", "--data-dir"],
    ] {
        let args: Vec<_> = args.into_iter().filter(|arg| !arg.is_empty()).collect();
        let output = Command::new(env!("CARGO_BIN_EXE_artifree-server"))
            .args(&args)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "Invalid arguments accepted: {args:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "Invalid arguments emitted success output: {args:?}"
        );
        let message = String::from_utf8(output.stderr)?;
        assert!(
            message.contains("Unknown arguments."),
            "Expected actionable failure for {args:?}, got {message:?}"
        );
    }
    Ok(())
}

#[test]
fn serve_requires_a_data_directory() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_artifree-server"))
        .args(["serve", "--api-listen", "127.0.0.1:0"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "no readiness line may be printed");
    assert!(String::from_utf8(output.stderr)?.contains("--data-dir"));
    Ok(())
}

#[test]
fn serve_rejects_a_preview_lifetime_out_of_range() -> Result<(), Box<dyn Error>> {
    // An unbounded lifetime overflowed expiry arithmetic while issuing a
    // ticket; it must fail at startup instead. The invalid listen address
    // makes a wrongly accepted lifetime fail on that message rather than
    // start a server that never exits.
    for seconds in ["0", "86401", "18446744073709551615"] {
        let output = Command::new(env!("CARGO_BIN_EXE_artifree-server"))
            .args([
                "serve",
                "--data-dir",
                "unused",
                "--preview-ttl-seconds",
                seconds,
                "--api-listen",
                "not-an-address",
            ])
            .output()?;
        assert_eq!(output.status.code(), Some(1), "{seconds}");
        assert!(output.stdout.is_empty(), "no readiness line may be printed");
        let message = String::from_utf8(output.stderr)?;
        assert!(
            message.contains("from 1 to 86400"),
            "{seconds}: {message:?}"
        );
    }
    Ok(())
}

#[cfg(unix)]
mod data_directory {
    use std::error::Error;
    use std::fs;
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("artifree-cli-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Kills the server even when an assertion fails first.
    struct Server(Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn serve(data_dir: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_artifree-server"));
        command.arg("serve").arg("--data-dir").arg(data_dir).args([
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ]);
        command
    }

    #[test]
    fn serve_creates_the_data_directory_for_the_owner_only() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new("private");
        let data_dir = scratch.0.join("nested").join("data");
        let mut server = Server(serve(&data_dir).stdout(Stdio::piped()).spawn()?);
        let stdout = server.0.stdout.take().ok_or("no stdout")?;
        let mut ready = String::new();
        BufReader::new(stdout).read_line(&mut ready)?;
        assert!(ready.contains("\"event\":\"ready\""), "{ready:?}");
        let mode = fs::metadata(&data_dir)?.permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "data directory mode is {mode:03o}");
        Ok(())
    }

    #[test]
    fn serve_refuses_a_data_directory_other_users_can_read() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new("shared");
        fs::create_dir_all(&scratch.0)?;
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755))?;
        // Read stdout instead of waiting for exit: a server that wrongly
        // starts prints its readiness line, fails here, and is killed on drop.
        let mut server = Server(
            serve(&scratch.0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let mut first_line = String::new();
        BufReader::new(server.0.stdout.take().ok_or("no stdout")?).read_line(&mut first_line)?;
        assert_eq!(first_line, "", "no readiness line may be printed");
        let status = server.0.wait()?;
        assert_eq!(status.code(), Some(1));
        let mut message = String::new();
        server
            .0
            .stderr
            .take()
            .ok_or("no stderr")?
            .read_to_string(&mut message)?;
        assert!(message.contains("accessible to other users"), "{message:?}");
        assert_eq!(
            fs::read_dir(&scratch.0)?.count(),
            0,
            "nothing may be written into a refused directory"
        );
        assert_eq!(
            fs::metadata(&scratch.0)?.permissions().mode() & 0o777,
            0o755,
            "the existing mode is reported, not changed"
        );
        Ok(())
    }
}
