//! Behavioral tests for the launcher's fixed argument list, its bounded runs,
//! the namespace comparison, the tool-root file checks and the host report's
//! rendering (decisions 0042 rule 1, 0056 rules 2, 9 and 13).

use super::super::report::{HostReport, sha256_file_hex, sha256_hex};
use super::*;

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};

/// A healthy launcher output: one `name=target` line per namespace, each target differing from the supervisor's `name:[1]`.
const LAUNCHER_OUTPUT: &str = "user=user:[2]\npid=pid:[2]\nnet=net:[2]\nmnt=mnt:[2]\nipc=ipc:[2]\nuts=uts:[2]\ncgroup=cgroup:[2]\n";

/// Gives each scratch directory of this test process its own name.
static NEXT_SCRATCH: AtomicU32 = AtomicU32::new(0);

/// A fresh directory under the system temporary directory, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates an empty directory whose name is unique to this process and call.
    fn new() -> io::Result<Self> {
        let path = env::temp_dir().join(format!(
            "artifree-launcher-{}-{}",
            process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        // A leftover with the same name is cleared first; its absence is expected.
        let cleared = fs::remove_dir_all(&path).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        });
        cleared?;
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let removed = fs::remove_dir_all(&self.path);
        // A failed removal fails the test, unless the test is already unwinding.
        assert!(
            removed.is_ok() || thread::panicking(),
            "could not remove the scratch directory {}: {removed:?}",
            self.path.display()
        );
    }
}

/// Each namespace paired with the target `name:[index]`, in `NAMESPACES` order.
fn tagged_namespaces(index: &str) -> Vec<(&'static str, String)> {
    NAMESPACES
        .into_iter()
        .map(|name| (name, format!("{name}:[{index}]")))
        .collect()
}

/// The healthy launcher output with `from` replaced by `to`; `from` must occur exactly once.
fn output_replacing(from: &str, to: &str) -> Vec<u8> {
    assert_eq!(
        LAUNCHER_OUTPUT.matches(from).count(),
        1,
        "{from:?} must occur exactly once in the launcher output"
    );
    LAUNCHER_OUTPUT.replacen(from, to, 1).into_bytes()
}

/// Writes a small file at `path` and sets its permission bits to exactly `mode`.
fn write_file(path: &Path, mode: u32) -> io::Result<()> {
    fs::write(path, "#!/bin/sh\n")?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Creates a tool root with a `bin` directory, writing the launcher and the fixture with the given modes; `None` leaves a file out.
fn tool_root(
    launcher_mode: Option<u32>,
    fixture_mode: Option<u32>,
) -> Result<Scratch, Box<dyn Error>> {
    let scratch = Scratch::new()?;
    fs::create_dir(scratch.path().join("bin"))?;
    if let Some(mode) = launcher_mode {
        write_file(&scratch.path().join(LAUNCHER), mode)?;
    }
    if let Some(mode) = fixture_mode {
        write_file(&scratch.path().join(FIXTURE_FILE), mode)?;
    }
    Ok(scratch)
}

/// The launcher arguments as text, one string per argument.
fn text_arguments(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// Runs `head -c <size> /dev/zero`, which writes exactly `size` zero bytes, under `limit`.
fn run_zeros(size: u64, limit: u64) -> Result<Vec<u8>, RunFailure> {
    let size = size.to_string();
    let mut command = Command::new("/usr/bin/head");
    command.args(["-c", size.as_str(), "/dev/zero"]);
    run_bounded(command, limit, RUN_DEADLINE)
}

/// A host report with every value set and two limits, as one admission run records it.
fn sample_report() -> HostReport {
    HostReport {
        kernel_release: "7.0.0-test".to_owned(),
        architecture: "x86_64",
        launcher_version: Some("bubblewrap 0.9.0".to_owned()),
        launcher_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            .to_owned(),
        tool_manifest_sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            .to_owned(),
        policy_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            .to_owned(),
        core_routing: "|/usr/lib/systemd/systemd-coredump".to_owned(),
        delegated_controllers: "cpu io memory pids".to_owned(),
        effective_limits: vec![
            ("memory.max", "67108864".to_owned()),
            ("pids.max", "64".to_owned()),
        ],
        peak_memory_bytes: Some(1_048_576),
    }
}

#[test]
fn namespaces_ok_accepts_seven_distinct_targets() {
    assert_eq!(
        compare_namespaces(&tagged_namespaces("1"), LAUNCHER_OUTPUT.as_bytes()),
        Ok(())
    );
}

#[test]
fn namespaces_equal_refuses_a_target_equal_to_the_supervisors() {
    for name in NAMESPACES {
        let output = output_replacing(&format!("{name}={name}:[2]"), &format!("{name}={name}:[1]"));
        assert_eq!(
            compare_namespaces(&tagged_namespaces("1"), &output),
            Err(AdmissionRefusal::NamespacesUnavailable),
            "a launcher target equal to the supervisor's {name} namespace must be refused"
        );
    }
}

#[test]
fn namespaces_rejects_malformed_output() {
    let own = tagged_namespaces("1");
    let mut not_text = LAUNCHER_OUTPUT.as_bytes().to_vec();
    not_text.push(0xff);
    let cases: [(&str, Vec<u8>); 8] = [
        ("no output", Vec::new()),
        (
            "one namespace missing",
            output_replacing("cgroup=cgroup:[2]\n", ""),
        ),
        (
            "a namespace repeated",
            format!("{LAUNCHER_OUTPUT}user=user:[2]\n").into_bytes(),
        ),
        (
            "an unknown name",
            format!("{LAUNCHER_OUTPUT}time=time:[3]\n").into_bytes(),
        ),
        (
            "a line without '='",
            output_replacing("user=user:[2]", "user"),
        ),
        (
            "an empty target",
            output_replacing("user=user:[2]", "user="),
        ),
        (
            "an empty name",
            output_replacing("user=user:[2]", "=user:[2]"),
        ),
        ("bytes that are not UTF-8", not_text),
    ];
    for (description, output) in cases {
        assert_eq!(
            compare_namespaces(&own, &output),
            Err(AdmissionRefusal::NamespacesUnavailable),
            "output with {description} must be refused"
        );
    }
}

#[test]
fn supervisor_namespaces_reads_seven_links_in_order() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    for (name, target) in tagged_namespaces("9") {
        symlink(target, scratch.path().join(name))?;
    }
    assert_eq!(
        supervisor_namespaces(scratch.path()),
        Ok(tagged_namespaces("9"))
    );
    Ok(())
}

#[test]
fn supervisor_namespaces_refuses_a_missing_or_non_text_link() -> Result<(), Box<dyn Error>> {
    for missing in NAMESPACES {
        let scratch = Scratch::new()?;
        for (name, target) in tagged_namespaces("9") {
            if name != missing {
                symlink(target, scratch.path().join(name))?;
            }
        }
        assert_eq!(
            supervisor_namespaces(scratch.path()),
            Err(AdmissionRefusal::NamespacesUnavailable),
            "a missing {missing} link must be refused"
        );
    }
    let scratch = Scratch::new()?;
    for (name, target) in tagged_namespaces("9") {
        symlink(target, scratch.path().join(name))?;
    }
    let user = scratch.path().join("user");
    fs::remove_file(&user)?;
    symlink(OsStr::from_bytes(&[0xff]), user)?;
    assert_eq!(
        supervisor_namespaces(scratch.path()),
        Err(AdmissionRefusal::NamespacesUnavailable),
        "a link whose target is not text must be refused"
    );
    Ok(())
}

#[test]
fn run_bounded_returns_output_of_a_successful_run() {
    let mut command = Command::new("/bin/echo");
    command.arg("hello");
    assert_eq!(
        run_bounded(command, MAX_OUTPUT_BYTES, RUN_DEADLINE),
        Ok(b"hello\n".to_vec())
    );
}

#[test]
fn run_bounded_accepts_output_of_exactly_its_bound() {
    let result =
        run_zeros(4096, 4096).map(|bytes| (bytes.len(), bytes.iter().all(|&byte| byte == 0)));
    assert_eq!(
        result,
        Ok((4096, true)),
        "4096 zero bytes under a 4096-byte bound must be returned in full"
    );
}

#[test]
fn run_bounded_refuses_output_longer_than_its_bound() {
    assert_eq!(
        run_zeros(5000, 4096).map(|bytes| bytes.len()),
        Err(RunFailure::Oversize),
        "5000 bytes under a 4096-byte bound must be refused as Oversize"
    );
    assert_eq!(
        run_zeros(17, 16).map(|bytes| bytes.len()),
        Err(RunFailure::Oversize),
        "17 bytes under a 16-byte bound must be refused as Oversize"
    );
}

#[test]
fn run_bounded_kills_a_run_past_its_deadline() {
    let mut command = Command::new("/bin/sleep");
    command.arg("2");
    let started = Instant::now();
    let result = run_bounded(command, MAX_OUTPUT_BYTES, Duration::from_millis(100));
    let elapsed = started.elapsed();
    assert_eq!(
        result,
        Err(RunFailure::Deadline),
        "a run past its 100 ms deadline must be reported as Deadline"
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "the run must be killed at its deadline rather than awaited; it took {elapsed:?}"
    );
}

#[test]
fn run_bounded_reports_an_unsuccessful_exit() {
    assert_eq!(
        run_bounded(Command::new("/bin/false"), MAX_OUTPUT_BYTES, RUN_DEADLINE),
        Err(RunFailure::Exit),
        "a command that exits with status 1 must be reported as Exit"
    );
}

#[test]
fn run_bounded_reports_a_command_that_cannot_start() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    assert_eq!(
        run_bounded(
            Command::new(scratch.path().join("missing")),
            MAX_OUTPUT_BYTES,
            RUN_DEADLINE
        ),
        Err(RunFailure::Spawn),
        "a command path that does not exist must be reported as Spawn"
    );
    Ok(())
}

#[test]
fn run_bounded_reaps_the_child_it_kills() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let pid_file = scratch.path().join("pid");
    // The shell records its own pid, then replaces itself with sleep, so the recorded pid is the child's.
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "echo $$ > \"$0\"; exec /bin/sleep 30"])
        .arg(&pid_file);
    let result = run_bounded(command, MAX_OUTPUT_BYTES, Duration::from_secs(1));
    assert_eq!(
        result,
        Err(RunFailure::Deadline),
        "a child that outlives its deadline must be reported as Deadline"
    );
    let recorded = fs::read_to_string(pid_file)?;
    let pid = recorded.trim();
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "the killed child {pid} must be reaped, but its process entry is still present"
    );
    Ok(())
}

#[test]
fn launcher_arguments_enable_every_namespace_explicitly() {
    let arguments = text_arguments(&launcher_arguments(Path::new("/opt/t")));
    for flag in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
    ] {
        assert!(
            arguments.iter().any(|argument| argument == flag),
            "the launcher arguments must contain {flag}: {arguments:?}"
        );
    }
}

#[test]
fn launcher_arguments_avoid_the_implicit_and_optional_forms() {
    let arguments = text_arguments(&launcher_arguments(Path::new("/opt/t")));
    assert!(
        !arguments.iter().any(|argument| argument == "--unshare-all"),
        "--unshare-all must not be used: {arguments:?}"
    );
    assert!(
        !arguments.iter().any(|argument| argument.ends_with("-try")),
        "an optional -try form skips a namespace silently: {arguments:?}"
    );
}

#[test]
fn launcher_arguments_bind_only_the_tool_root_and_its_libraries() {
    let arguments = text_arguments(&launcher_arguments(Path::new("/opt/t")));
    let binds: Vec<(&str, &str)> = arguments
        .windows(3)
        .filter_map(|window| match window {
            [flag, source, destination] if flag == "--ro-bind" => {
                Some((source.as_str(), destination.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        binds,
        [
            ("/opt/t", "/tools"),
            ("/opt/t/lib", "/lib"),
            ("/opt/t/lib64", "/lib64"),
        ],
        "the launcher must bind the tool root, lib and lib64 read-only and nothing else"
    );
    let bind_options = arguments
        .iter()
        .filter(|argument| argument.contains("bind"))
        .count();
    assert_eq!(
        bind_options, 3,
        "every bind option must be one of the three read-only binds: {arguments:?}"
    );
}

#[test]
fn launcher_arguments_end_with_the_fixture_namespace_check() -> Result<(), Box<dyn Error>> {
    let arguments = text_arguments(&launcher_arguments(Path::new("/opt/t")));
    let [.., fixture, mode] = arguments.as_slice() else {
        return Err("the launcher arguments end before the fixture".into());
    };
    assert_eq!(
        fixture, "/tools/bin/artifree-execution-fixture",
        "the fixture must run from the tool root's mount"
    );
    assert_eq!(
        mode, "namespaces",
        "the fixture must run in namespaces mode"
    );
    Ok(())
}

#[test]
fn tool_files_report_the_launcher_before_the_fixture() -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "neither file",
            None,
            None,
            Err(AdmissionRefusal::MissingLauncher),
        ),
        (
            "only a launcher",
            Some(0o755),
            None,
            Err(AdmissionRefusal::MissingFixture),
        ),
        (
            "a launcher without an execute bit",
            Some(0o644),
            Some(0o755),
            Err(AdmissionRefusal::MissingLauncher),
        ),
        (
            "a fixture without an execute bit",
            Some(0o755),
            Some(0o644),
            Err(AdmissionRefusal::MissingFixture),
        ),
        ("two executable files", Some(0o755), Some(0o755), Ok(())),
    ];
    for (description, launcher_mode, fixture_mode, expected) in cases {
        let root = tool_root(launcher_mode, fixture_mode)?;
        assert_eq!(
            check_tool_files(root.path()),
            expected,
            "a tool root with {description}"
        );
    }
    Ok(())
}

#[test]
fn tool_files_refuse_a_launcher_that_is_a_link() -> Result<(), Box<dyn Error>> {
    let root = tool_root(None, Some(0o755))?;
    write_file(&root.path().join("bin/real-bwrap"), 0o755)?;
    symlink("real-bwrap", root.path().join(LAUNCHER))?;
    assert_eq!(
        check_tool_files(root.path()),
        Err(AdmissionRefusal::MissingLauncher),
        "a launcher that is a link must be refused, not followed to its target"
    );
    Ok(())
}

#[test]
fn report_render_lists_every_key_once_in_order() {
    let rendered = sample_report().to_string();
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(
        lines,
        [
            "kernel_release=7.0.0-test",
            "architecture=x86_64",
            "launcher_version=bubblewrap 0.9.0",
            "launcher_sha256=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "tool_manifest_sha256=2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "policy_sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "core_routing=|/usr/lib/systemd/systemd-coredump",
            "delegated_controllers=cpu io memory pids",
            "limit.memory.max=67108864",
            "limit.pids.max=64",
            "peak_memory_bytes=1048576",
            "cleanup_observation=killed, populated 0 within 5 s, child reaped, group removed",
        ],
        "the report must render each key once, in this order"
    );
    assert!(
        !rendered.to_lowercase().contains("supported"),
        "the report must not claim that the host is supported: {rendered}"
    );
}

#[test]
fn report_render_marks_missing_values_unavailable() {
    let report = HostReport {
        launcher_version: None,
        peak_memory_bytes: None,
        ..sample_report()
    };
    let rendered = report.to_string();
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines.contains(&"launcher_version=unavailable"),
        "a launcher version that could not be read must render as unavailable: {rendered}"
    );
    assert!(
        lines.contains(&"peak_memory_bytes=unavailable"),
        "a peak that the kernel does not offer must render as unavailable: {rendered}"
    );
}

#[test]
fn sha256_known_digests() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha256_file_matches_known_digests_across_chunks() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let small = scratch.path().join("small");
    fs::write(&small, b"abc")?;
    assert_eq!(
        sha256_file_hex(&small)?,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let empty = scratch.path().join("empty");
    fs::write(&empty, b"")?;
    assert_eq!(
        sha256_file_hex(&empty)?,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    // One million bytes take several 64 KiB reads, so the streaming loop runs more than once.
    let large = scratch.path().join("large");
    fs::write(&large, vec![b'a'; 1_000_000])?;
    assert_eq!(
        sha256_file_hex(&large)?,
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    Ok(())
}
