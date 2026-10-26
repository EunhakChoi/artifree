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
