//! Behavioral tests of the Linux host admission: the facility order in `admit_with` and the
//! cgroup, probe and cleanup checks that it calls. Each test works in a temporary tree and
//! starts no process. The order tests stop before facility 8, the namespace check that
//! starts the launcher.

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{
    AdmissionRefusal, Controller, InspectionProfile, cgroup, launcher, probe_group,
};
use super::*;

/// The pid that the baseline host lists beside the provider's own pid in `cgroup.procs`.
const OTHER_PID: u32 = 4242;
/// The deadline that the timed kill tests pass to `kill_and_await_empty`.
const DEADLINE: Duration = Duration::from_secs(3);
/// Numbers the scratch directories of this process, so that no two tests share a name.
static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temporary directory for one test's files, removed when the value is dropped.
#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// Creates an empty directory whose name no other scratch directory of this process uses.
    fn new() -> io::Result<Self> {
        let name = format!(
            "artifree-admission-{}-{}",
            process::id(),
            SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = env::temp_dir().join(name);
        fs::create_dir(&root)?;
        Ok(Self { root })
    }

    /// The scratch directory itself.
    fn path(&self) -> &Path {
        &self.root
    }

    /// Creates `relative` below the scratch directory, with any missing parents.
    fn dir(&self, relative: &str) -> io::Result<PathBuf> {
        let path = self.root.join(relative);
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let removed = fs::remove_dir_all(self.root.as_path());
        // A failed removal fails the test, unless the test is already unwinding.
        if !thread::panicking() {
            assert!(
                removed.is_ok(),
                "could not remove {}: {removed:?}",
                self.root.display()
            );
        }
    }
}

/// Sets the permission bits of `path` to exactly `mode`.
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Writes an empty file at `path` and gives it exactly `mode`.
fn write_with_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::write(path, "")?;
    set_mode(path, mode)
}

/// Creates `parent/probe` with `create_probe` and writes each of `files` into it.
fn probe_with_files(
    scratch: &Scratch,
    parent: &str,
    files: &[(&str, &str)],
) -> Result<probe_group::ProbeGroup, Box<dyn Error>> {
    let parent_path = scratch.dir(parent)?;
    let probe = probe_group::create_probe(&parent_path, "probe")?;
    for &(name, contents) in files {
        fs::write(parent_path.join("probe").join(name), contents)?;
    }
    Ok(probe)
}

/// The directory of the probe group that `probe_with_files` creates under `parent`.
fn probe_path(scratch: &Scratch, parent: &str) -> PathBuf {
    scratch.path().join(parent).join("probe")
}

/// A clock that reads `base` plus each offset in turn, then keeps reading the last offset.
fn scripted_clock(base: Instant, offsets_secs: &'static [u64]) -> impl FnMut() -> Instant {
    let mut reading = 0;
    move || {
        let offset = offsets_secs
            .get(reading)
            .or_else(|| offsets_secs.last())
            .copied()
            .unwrap_or(0);
        reading += 1;
        base + Duration::from_secs(offset)
    }
}

/// A host laid out like the inputs that `admit_with` reads.
///
/// Every facility before the sole-occupant check passes. The group holds the provider and
/// `OTHER_PID`, so that check refuses the baseline. No variation of this host reaches the
/// namespace check, which would start the launcher.
struct FakeHost {
    proc_self: PathBuf,
    osrelease: PathBuf,
    core_pattern: PathBuf,
    cgroup_mount: PathBuf,
    tool_root: PathBuf,
}

impl FakeHost {
    /// Writes the baseline host below `scratch`.
    fn new(scratch: &Scratch) -> Result<Self, Box<dyn Error>> {
        let proc_self = scratch.dir("proc-self")?;
        let cgroup_mount = scratch.dir("mount")?;
        let tool_root = scratch.dir("tool-root")?;
        let osrelease = scratch.path().join("osrelease");
        let core_pattern = scratch.path().join("core_pattern");
        fs::write(proc_self.join("cgroup"), "0::/provider.service\n")?;
        fs::write(&osrelease, "7.0.0-test\n")?;
        fs::write(&core_pattern, "core\n")?;
        let group = cgroup_mount.join("provider.service");
        fs::create_dir(&group)?;
        fs::write(group.join("cgroup.kill"), "")?;
        fs::write(group.join("cgroup.subtree_control"), "")?;
        fs::write(group.join("cgroup.controllers"), "cpu io memory pids\n")?;
        let own = process::id();
        fs::write(group.join("cgroup.procs"), format!("{own}\n{OTHER_PID}\n"))?;
        fs::create_dir(tool_root.join("bin"))?;
        write_with_mode(&tool_root.join(launcher::LAUNCHER), 0o755)?;
        write_with_mode(&tool_root.join(launcher::FIXTURE_FILE), 0o755)?;
        Ok(Self {
            proc_self,
            osrelease,
            core_pattern,
            cgroup_mount,
            tool_root,
        })
    }

    /// The roots that `admit_with` reads from this host.
    fn roots(&self) -> AdmissionRoots<'_> {
        AdmissionRoots {
            proc_self: &self.proc_self,
            osrelease: &self.osrelease,
            core_pattern: &self.core_pattern,
            cgroup_mount: &self.cgroup_mount,
            tool_root: &self.tool_root,
        }
    }

    /// The provider's delegated group in this host.
    fn group(&self) -> PathBuf {
        self.cgroup_mount.join("provider.service")
    }
}

/// A change to the baseline host, with the refusal that the admission must then report.
type Variation = (
    &'static str,
    fn(&FakeHost) -> io::Result<()>,
    AdmissionRefusal,
);

/// Routes core dumps to a pipe, which the admission refuses at facility 3.
fn route_to_pipe(host: &FakeHost) -> io::Result<()> {
    fs::write(&host.core_pattern, "|/x\n")
}

/// Runs `admit_with` against `host`, with `worker_uid` as the uid the worker reports, and
/// returns the refusal, or `None` when the admission passed.
fn refusal_as(
    host: &FakeHost,
    worker_uid: impl FnOnce() -> Result<u32, AdmissionRefusal>,
) -> Option<AdmissionRefusal> {
    admit_with(
        InspectionProfile::fixture(),
        &host.roots(),
        process::id(),
        worker_uid,
    )
    .err()
}

/// The uid that owns `host`, and the next uid, which owns nothing in it.
fn owner_and_stranger(host: &FakeHost) -> Result<(u32, u32), Box<dyn Error>> {
    let owner = cgroup::own_uid(&host.proc_self)?;
    let stranger = owner
        .checked_add(1)
        .ok_or("the uid cannot be incremented")?;
    Ok((owner, stranger))
}

#[test]
fn discover_own_group_finds_the_one_unified_line_and_refuses_the_rest() -> Result<(), Box<dyn Error>>
{
    let scratch = Scratch::new()?;
    let mount = scratch.dir("mount")?;
    let refused: Result<PathBuf, AdmissionRefusal> = Err(AdmissionRefusal::CgroupV2Unavailable);
    let cases: [(&str, Result<PathBuf, AdmissionRefusal>); 7] = [
        (
            "0::/user.slice/a.service\n",
            Ok(mount.join("user.slice/a.service")),
        ),
        ("1:name=systemd:/x\n", refused.clone()),
        ("0::/a\n0::/b\n", refused.clone()),
        ("0::relative\n", refused.clone()),
        ("0::/a/../b\n", refused.clone()),
        ("0::/a/./b\n", refused.clone()),
        ("", refused.clone()),
    ];
    for (index, (text, expected)) in cases.into_iter().enumerate() {
        let file = scratch.path().join(format!("cgroup-{index}"));
        fs::write(&file, text)?;
        assert_eq!(
            cgroup::discover_own_group(&file, &mount),
            expected,
            "/proc/self/cgroup {text:?}"
        );
    }
    let absent = scratch.path().join("cgroup-absent");
    assert_eq!(
        cgroup::discover_own_group(&absent, &mount),
        refused,
        "a missing /proc/self/cgroup"
    );
    Ok(())
}

#[test]
fn kill_support_requires_a_cgroup_kill_file() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let group = scratch.dir("group")?;
    assert_eq!(
        cgroup::check_kill_support(&group),
        Err(AdmissionRefusal::MissingKillSupport),
        "a group without cgroup.kill"
    );
    fs::create_dir(group.join("cgroup.kill"))?;
    assert_eq!(
        cgroup::check_kill_support(&group),
        Err(AdmissionRefusal::MissingKillSupport),
        "a directory named cgroup.kill is not a file"
    );
    fs::remove_dir(group.join("cgroup.kill"))?;
    fs::write(group.join("cgroup.kill"), "")?;
    assert_eq!(
        cgroup::check_kill_support(&group),
        Ok(()),
        "a group with a cgroup.kill file"
    );
    Ok(())
}

#[test]
fn delegation_requires_the_providers_own_uid() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let group = scratch.dir("group")?;
    fs::write(group.join("cgroup.procs"), "")?;
    fs::write(group.join("cgroup.subtree_control"), "")?;
    let owner = cgroup::own_uid(scratch.path())?;
    let stranger = owner
        .checked_add(1)
        .ok_or("the uid cannot be incremented")?;
    assert_eq!(
        cgroup::check_delegation(&group, owner),
        Ok(()),
        "the provider's own uid owns the group"
    );
    assert_eq!(
        cgroup::check_delegation(&group, stranger),
        Err(AdmissionRefusal::UndelegatedCgroup),
        "uid {stranger} does not own the group"
    );
    Ok(())
}

#[test]
fn delegation_requires_a_directory_with_both_control_files() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let owner = cgroup::own_uid(scratch.path())?;
    let group = scratch.dir("group")?;
    assert_eq!(
        cgroup::check_delegation(&group, owner),
        Err(AdmissionRefusal::UndelegatedCgroup),
        "a group without its control files"
    );
    fs::write(group.join("cgroup.procs"), "")?;
    assert_eq!(
        cgroup::check_delegation(&group, owner),
        Err(AdmissionRefusal::UndelegatedCgroup),
        "a group without cgroup.subtree_control"
    );
    let plain = scratch.path().join("plain");
    fs::write(&plain, "")?;
    assert_eq!(
        cgroup::check_delegation(&plain, owner),
        Err(AdmissionRefusal::UndelegatedCgroup),
        "a plain file is not a group"
    );
    Ok(())
}

#[test]
fn first_missing_controller_matches_whole_tokens_in_checking_order() {
    let cases: [(&str, Option<Controller>); 6] = [
        ("memory pids cpu", None),
        ("io cpu pids memory", None),
        ("memoryx pids cpu", Some(Controller::Memory)),
        ("memory cpu", Some(Controller::Pids)),
        ("memory pids", Some(Controller::Cpu)),
        ("cpu", Some(Controller::Memory)),
    ];
    for (listed, expected) in cases {
        assert_eq!(
            cgroup::first_missing_controller(listed),
            expected,
            "cgroup.controllers {listed:?}"
        );
    }
}

#[test]
fn check_controllers_names_the_required_controller_a_line_lacks() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let group = scratch.dir("group")?;
    for missing in Controller::REQUIRED {
        let mut listed: Vec<&str> = Controller::REQUIRED
            .into_iter()
            .filter(|&controller| controller != missing)
            .map(Controller::name)
            .collect();
        listed.push("io");
        let line = listed.join(" ");
        fs::write(group.join("cgroup.controllers"), format!("{line}\n"))?;
        assert_eq!(
            cgroup::check_controllers(&group),
            Err(AdmissionRefusal::MissingController(missing)),
            "cgroup.controllers {line:?} lacks {}",
            missing.name()
        );
    }
    fs::write(group.join("cgroup.controllers"), "io cpu pids memory\n")?;
    assert_eq!(
        cgroup::check_controllers(&group),
        Ok("io cpu pids memory".to_owned()),
        "a complete line is returned without its newline"
    );
    fs::remove_file(group.join("cgroup.controllers"))?;
    assert_eq!(
        cgroup::check_controllers(&group),
        Err(AdmissionRefusal::MissingController(Controller::Memory)),
        "an unreadable controller list names memory"
    );
    Ok(())
}

#[test]
fn sole_occupant_accepts_only_the_providers_own_pid() -> Result<(), Box<dyn Error>> {
    let refused: Result<(), AdmissionRefusal> = Err(AdmissionRefusal::OccupiedCgroup);
    let cases: [(Option<&str>, Result<(), AdmissionRefusal>); 7] = [
        (Some("1234\n"), Ok(())),
        (Some("1234\n4242\n"), refused),
        (Some("4242\n"), refused),
        (Some("1234\n1234\n"), refused),
        (Some(""), refused),
        (Some("abc\n"), refused),
        (None, refused),
    ];
    for (procs, expected) in cases {
        let scratch = Scratch::new()?;
        let group = scratch.dir("group")?;
        if let Some(text) = procs {
            fs::write(group.join("cgroup.procs"), text)?;
        }
        assert_eq!(
            cgroup::check_sole_occupant(&group, 1234),
            expected,
            "cgroup.procs {procs:?}, provider pid 1234"
        );
    }
    Ok(())
}

#[test]
fn subtree_write_refusal_maps_busy_to_occupied_and_other_errors_to_undelegated() {
    let busy = io::Error::from(io::ErrorKind::ResourceBusy);
    assert_eq!(
        cgroup::subtree_write_refusal(&busy),
        AdmissionRefusal::OccupiedCgroup,
        "EBUSY means another process holds the group"
    );
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::NotFound,
        io::ErrorKind::InvalidInput,
    ] {
        let error = io::Error::from(kind);
        assert_eq!(
            cgroup::subtree_write_refusal(&error),
            AdmissionRefusal::UndelegatedCgroup,
            "{kind:?} is a delegation failure"
        );
    }
}

#[test]
fn read_back_accepts_each_probe_limit_exactly_and_refuses_any_other_value() {
    for (file, value) in probe_group::PROBE_LIMITS {
        let exact = format!("{value}\n");
        let extra_newline = format!("{value}\n\n");
        let longer = format!("{value}9\n");
        assert_eq!(
            probe_group::verify_read_back(value, exact.as_bytes()),
            Ok(()),
            "{file} reads back {exact:?}"
        );
        for actual in ["max\n", extra_newline.as_str(), longer.as_str()] {
            assert_eq!(
                probe_group::verify_read_back(value, actual.as_bytes()),
                Err(AdmissionRefusal::LimitNotEnforced),
                "{file} was written {value:?} and reads back {actual:?}"
            );
        }
    }
}

#[test]
fn apply_limits_writes_and_reads_back_every_probe_limit() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let empty: Vec<(&str, &str)> = probe_group::PROBE_LIMITS
        .iter()
        .map(|&(file, _)| (file, ""))
        .collect();
    let probe = probe_with_files(&scratch, "parent", &empty)?;
    let applied = probe_group::apply_limits(&probe)?;
    let expected: Vec<(&str, String)> = probe_group::PROBE_LIMITS
        .iter()
        .map(|&(file, value)| (file, value.to_owned()))
        .collect();
    assert_eq!(
        applied, expected,
        "every probe limit is applied and read back"
    );
    for (file, value) in probe_group::PROBE_LIMITS {
        let written = fs::read_to_string(probe_path(&scratch, "parent").join(file))?;
        assert_eq!(
            written, value,
            "the control file {file} holds exactly the value written"
        );
    }
    Ok(())
}

#[test]
fn apply_limits_refuses_a_missing_control_file_without_creating_it() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(&scratch, "parent", &[])?;
    assert_eq!(
        probe_group::apply_limits(&probe).err(),
        Some(AdmissionRefusal::LimitNotEnforced),
        "no control file exists to write"
    );
    assert!(
        !probe_path(&scratch, "parent").join("memory.max").exists(),
        "a limit write must not create memory.max"
    );
    Ok(())
}

#[test]
fn create_probe_never_reuses_an_existing_group() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let parent = scratch.dir("parent")?;
    probe_group::create_probe(&parent, "probe")?;
    assert_eq!(
        probe_group::create_probe(&parent, "probe").err(),
        Some(AdmissionRefusal::UndelegatedCgroup),
        "an existing group is not reused"
    );
    Ok(())
}

#[test]
fn parse_populated_reads_exactly_one_populated_line() {
    let cases: [(&str, Option<bool>); 9] = [
        ("populated 0\nfrozen 0\n", Some(false)),
        ("frozen 1\npopulated 1\n", Some(true)),
        ("populated 1\n", Some(true)),
        ("populated=0\n", None),
        ("populated\n", None),
        ("", None),
        ("populated 2\n", None),
        ("populated 0\npopulated 0\n", None),
        ("populated 0\npopulated 1\n", None),
    ];
    for (text, expected) in cases {
        assert_eq!(
            probe_group::parse_populated(text.as_bytes()),
            expected,
            "cgroup.events {text:?}"
        );
    }
    assert_eq!(
        probe_group::parse_populated(&[0xff, b'\n']),
        None,
        "non-UTF-8 cgroup.events"
    );
}

#[test]
fn kill_writes_one_to_cgroup_kill_and_accepts_an_observed_empty_group() -> Result<(), Box<dyn Error>>
{
    let scratch = Scratch::new()?;
    let probe = probe_with_files(
        &scratch,
        "parent",
        &[("cgroup.kill", ""), ("cgroup.events", "populated 0\n")],
    )?;
    let clock = scripted_clock(Instant::now(), &[0]);
    probe_group::kill_and_await_empty(&probe, probe_group::CLEANUP_DEADLINE, clock)?;
    assert_eq!(
        fs::read_to_string(probe_path(&scratch, "parent").join("cgroup.kill"))?,
        "1",
        "cgroup.kill receives exactly one 1"
    );
    Ok(())
}

#[test]
fn kill_refuses_a_group_still_populated_at_the_deadline() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(
        &scratch,
        "parent",
        &[("cgroup.kill", ""), ("cgroup.events", "populated 1\n")],
    )?;
    let clock = scripted_clock(Instant::now(), &[0, 1, 2, 3]);
    assert_eq!(
        probe_group::kill_and_await_empty(&probe, DEADLINE, clock),
        Err(AdmissionRefusal::CleanupNotObserved),
        "a group still populated at the deadline"
    );
    assert_eq!(
        fs::read_to_string(probe_path(&scratch, "parent").join("cgroup.kill"))?,
        "1",
        "the kill was written before the wait"
    );
    Ok(())
}

#[test]
fn kill_refuses_a_missing_cgroup_kill_file_and_does_not_create_it() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(&scratch, "parent", &[("cgroup.events", "populated 0\n")])?;
    let clock = scripted_clock(Instant::now(), &[0]);
    assert_eq!(
        probe_group::kill_and_await_empty(&probe, DEADLINE, clock),
        Err(AdmissionRefusal::CleanupNotObserved),
        "the kill cannot be written without cgroup.kill"
    );
    assert!(
        !probe_path(&scratch, "parent").join("cgroup.kill").exists(),
        "the kill write must not create cgroup.kill"
    );
    Ok(())
}

#[test]
fn kill_refuses_unreadable_or_malformed_events() -> Result<(), Box<dyn Error>> {
    let cases: [(Option<&str>, &str); 4] = [
        (None, "cgroup.events is absent"),
        (Some("populated 2\n"), "a value other than 0 or 1"),
        (Some("populated=0\n"), "a field without a space"),
        (Some(""), "an empty file"),
    ];
    for (events, label) in cases {
        let scratch = Scratch::new()?;
        let mut files: Vec<(&str, &str)> = vec![("cgroup.kill", "")];
        if let Some(text) = events {
            files.push(("cgroup.events", text));
        }
        let probe = probe_with_files(&scratch, "parent", &files)?;
        let clock = scripted_clock(Instant::now(), &[0]);
        assert_eq!(
            probe_group::kill_and_await_empty(&probe, DEADLINE, clock),
            Err(AdmissionRefusal::CleanupNotObserved),
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn kill_refuses_a_clock_that_moves_backwards() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(
        &scratch,
        "parent",
        &[("cgroup.kill", ""), ("cgroup.events", "populated 1\n")],
    )?;
    let clock = scripted_clock(Instant::now(), &[5, 1]);
    assert_eq!(
        probe_group::kill_and_await_empty(&probe, DEADLINE, clock),
        Err(AdmissionRefusal::CleanupNotObserved),
        "a reading earlier than the first reading"
    );
    Ok(())
}

#[test]
fn remove_probe_removes_an_empty_group() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(&scratch, "parent", &[])?;
    assert_eq!(
        probe_group::remove_probe(probe),
        Ok(()),
        "an empty group is removed"
    );
    assert!(
        !probe_path(&scratch, "parent").exists(),
        "the group directory is gone"
    );
    Ok(())
}

#[test]
fn remove_probe_refuses_a_group_that_still_holds_a_file() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let probe = probe_with_files(&scratch, "parent", &[("cgroup.procs", "")])?;
    assert_eq!(
        probe_group::remove_probe(probe),
        Err(AdmissionRefusal::CleanupNotObserved),
        "rmdir refuses a group that holds a file"
    );
    assert!(
        probe_path(&scratch, "parent").is_dir(),
        "a refused group stays in place"
    );
    Ok(())
}

#[test]
fn failed_probe_attempt_leaves_its_group_in_place() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let delegated = scratch.dir("delegated")?;
    // The new probe group has no control files, so its first limit write fails before any
    // child starts. `tool_root` is not read before that write.
    assert_eq!(
        run_probe(&delegated, scratch.path()).err(),
        Some(AdmissionRefusal::LimitNotEnforced),
        "a probe group without control files refuses its first limit"
    );
    let entries = fs::read_dir(&delegated)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<PathBuf>, io::Error>>()?;
    assert_eq!(
        entries.len(),
        1,
        "the failed attempt keeps its probe group, found {entries:?}"
    );
    assert!(
        entries.iter().all(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("probe-"))
        }),
        "the entry left in place is a probe group, found {entries:?}"
    );
    Ok(())
}

#[test]
fn kernel_and_cgroup_facilities_come_before_core_routing() -> Result<(), Box<dyn Error>> {
    let variations: [Variation; 4] = [
        (
            "unreadable kernel release",
            |host: &FakeHost| -> io::Result<()> {
                route_to_pipe(host)?;
                fs::remove_file(&host.osrelease)
            },
            AdmissionRefusal::UnsupportedKernel,
        ),
        (
            "empty kernel release",
            |host: &FakeHost| -> io::Result<()> {
                route_to_pipe(host)?;
                fs::write(&host.osrelease, "\n")
            },
            AdmissionRefusal::UnsupportedKernel,
        ),
        (
            "cgroup file without a unified line",
            |host: &FakeHost| -> io::Result<()> {
                route_to_pipe(host)?;
                fs::write(host.proc_self.join("cgroup"), "1:name=systemd:/x\n")
            },
            AdmissionRefusal::CgroupV2Unavailable,
        ),
        (
            "group without cgroup.kill",
            |host: &FakeHost| -> io::Result<()> {
                route_to_pipe(host)?;
                fs::remove_file(host.group().join("cgroup.kill"))
            },
            AdmissionRefusal::MissingKillSupport,
        ),
    ];
    for (name, edit, expected) in variations {
        let scratch = Scratch::new()?;
        let host = FakeHost::new(&scratch)?;
        edit(&host)?;
        assert_eq!(
            refusal_as(&host, || cgroup::own_uid(&host.proc_self)),
            Some(expected),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn core_routing_and_tool_files_come_before_delegation() -> Result<(), Box<dyn Error>> {
    let variations: [Variation; 6] = [
        (
            "pipe routing with an empty tool root",
            |host: &FakeHost| -> io::Result<()> {
                route_to_pipe(host)?;
                fs::remove_dir_all(&host.tool_root)
            },
            AdmissionRefusal::PipeCoreRouting,
        ),
        (
            "empty tool root",
            |host: &FakeHost| -> io::Result<()> { fs::remove_dir_all(&host.tool_root) },
            AdmissionRefusal::MissingLauncher,
        ),
        (
            "launcher without the execute bit",
            |host: &FakeHost| -> io::Result<()> {
                set_mode(&host.tool_root.join(launcher::LAUNCHER), 0o644)
            },
            AdmissionRefusal::MissingLauncher,
        ),
        (
            "launcher that is a symbolic link",
            |host: &FakeHost| -> io::Result<()> {
                let launcher_path = host.tool_root.join(launcher::LAUNCHER);
                fs::remove_file(&launcher_path)?;
                std::os::unix::fs::symlink(
                    host.tool_root.join(launcher::FIXTURE_FILE),
                    launcher_path,
                )
            },
            AdmissionRefusal::MissingLauncher,
        ),
        (
            "fixture absent",
            |host: &FakeHost| -> io::Result<()> {
                fs::remove_file(host.tool_root.join(launcher::FIXTURE_FILE))
            },
            AdmissionRefusal::MissingFixture,
        ),
        (
            "fixture without the execute bit",
            |host: &FakeHost| -> io::Result<()> {
                set_mode(&host.tool_root.join(launcher::FIXTURE_FILE), 0o644)
            },
            AdmissionRefusal::MissingFixture,
        ),
    ];
    for (name, edit, expected) in variations {
        let scratch = Scratch::new()?;
        let host = FakeHost::new(&scratch)?;
        edit(&host)?;
        let (_, stranger) = owner_and_stranger(&host)?;
        assert_eq!(
            refusal_as(&host, move || Ok(stranger)),
            Some(expected),
            "{name}, with a worker uid that does not own the group"
        );
    }
    Ok(())
}

#[test]
fn foreign_owner_is_refused_as_undelegated() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let host = FakeHost::new(&scratch)?;
    let (_, stranger) = owner_and_stranger(&host)?;
    assert_eq!(
        refusal_as(&host, move || Ok(stranger)),
        Some(AdmissionRefusal::UndelegatedCgroup),
        "a worker uid that does not own the group"
    );
    Ok(())
}

#[test]
fn controllers_come_before_occupancy() -> Result<(), Box<dyn Error>> {
    let variations: [Variation; 2] = [
        (
            "a required controller missing",
            |host: &FakeHost| -> io::Result<()> {
                fs::write(host.group().join("cgroup.controllers"), "memory cpu\n")
            },
            AdmissionRefusal::MissingController(Controller::Pids),
        ),
        (
            "two processes in the group",
            |_host: &FakeHost| -> io::Result<()> { Ok(()) },
            AdmissionRefusal::OccupiedCgroup,
        ),
    ];
    for (name, edit, expected) in variations {
        let scratch = Scratch::new()?;
        let host = FakeHost::new(&scratch)?;
        edit(&host)?;
        assert_eq!(
            refusal_as(&host, || cgroup::own_uid(&host.proc_self)),
            Some(expected),
            "{name}"
        );
    }
    Ok(())
}
