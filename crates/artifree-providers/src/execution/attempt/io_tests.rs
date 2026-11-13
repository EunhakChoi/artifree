//! Behavioral tests of the input and output of one attempt (decision 0056 rule 14, row R02af,
//! with the amendment of 2026-11-13, and decision 0042 rule 7): the staged input is read-only in
//! the payload and becomes the expected output, and the output root is either collected or
//! refused by its reason.
//!
//! Every test is ignored here, because each one launches the fixture in its attempt group.
//! `scripts/enforcement/run.sh` (part 9) prepares one payload scenario with
//! `prepare.sh scenario <name>` and runs one test by its exact name. The payload reads the
//! scenario word itself, so each test launches the fixture plan and checks what its scenario
//! produced.

use super::super::FIXTURE;
use super::super::collection::{CollectedOutput, CollectionRefusal, PackageReason};
use super::super::report::sha256_hex;
use super::resource_tests::{evidence_of, stdout_text};
use super::tests::{ExecutionDir, assert_no_attempt_groups};
use super::{AttemptEvidence, AttemptPlan, FIXTURE_PAGE, OutputRefusal, launch};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::OwnedFd;

/// The bytes that the `copy` scenario appends to the staged page.
const COPIED: &[u8] = b"<!-- copied -->";
/// The bytes of the asset that the `copy` scenario writes beside the page.
const ASSET: &[u8] = b"collected";
/// The most that the `Shmem` line may grow across one `copy` launch, in KiB: 16 MiB. The scenario
/// writes 64 MiB into the scratch, which must be freed once the launch has returned.
const SHMEM_GROWTH_LIMIT_KIB: u64 = 16 * 1024;
/// The mount options that the payload must see on `/input` (decision 0056 rule 3, amendment of
/// 2026-11-13).
const INPUT_FLAGS: [&str; 4] = ["ro", "nosuid", "nodev", "noexec"];
/// The lines of the `input-write` scenario, one per change, with the outcome that each must have:
/// `error=30` is EROFS and `error=13` is EACCES.
const INPUT_LINES: [&str; 8] = [
    "input-write create error=30",
    "input-write unlink error=30",
    "input-write mkdir error=30",
    "input-write append error=13",
    "input-write init-append error=13",
    "input-write init-create error=30",
    "input-write init-unlink error=30",
    "input-write init-mkdir error=30",
];

/// Plants a relative symbolic link, `link.txt`, that points at `index.html`.
fn plant_link(out: &OwnedFd) -> io::Result<()> {
    rustix::fs::symlinkat("index.html", out, "link.txt")?;
    Ok(())
}

/// Plants `seed.txt` and a second name for it, `hard.txt`, so that the file has two links.
fn plant_hard_link(out: &OwnedFd) -> io::Result<()> {
    let seed = rustix::fs::openat(
        out,
        "seed.txt",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    File::from(seed).write_all(b"x")?;
    rustix::fs::linkat(out, "seed.txt", out, "hard.txt", AtFlags::empty())?;
    Ok(())
}

/// Plants a FIFO, `fifo.txt`, which the output root must refuse without opening it.
fn plant_fifo(out: &OwnedFd) -> io::Result<()> {
    rustix::fs::mknodat(out, "fifo.txt", FileType::Fifo, Mode::RUSR | Mode::WUSR, 0)?;
    Ok(())
}

/// The `Shmem` line of `/proc/meminfo`, in KiB. The key is matched exactly.
fn shmem_kib() -> Result<u64, Box<dyn Error>> {
    let meminfo = fs::read_to_string("/proc/meminfo")?;
    let value = meminfo
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key == "Shmem").then_some(value)
        })
        .ok_or("/proc/meminfo has no Shmem line")?;
    let kib = value
        .split_whitespace()
        .next()
        .ok_or("the Shmem line has no value")?;
    let kib: u64 = kib.parse()?;
    Ok(kib)
}

/// Whether `text` has a line that is exactly `line`.
fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|candidate| candidate == line)
}

/// The files of a collected package with their bytes, in path order.
fn files_of(collected: &CollectedOutput) -> Vec<(String, Vec<u8>)> {
    collected
        .package
        .files()
        .map(|(path, bytes)| (path.as_str().to_owned(), bytes.to_vec()))
        .collect()
}

/// The files that the `copy` scenario must leave in the output root, in path order.
fn copied_files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("assets/result.txt".to_owned(), ASSET.to_vec()),
        ("index.html".to_owned(), [FIXTURE_PAGE, COPIED].concat()),
    ]
}

/// The collected output of a launch whose output must be collected. A refusal is an error that
/// names it.
fn collected_of(
    label: &str,
    output: Result<CollectedOutput, OutputRefusal>,
) -> Result<CollectedOutput, Box<dyn Error>> {
    output.map_err(|refusal| format!("the {label} output was not collected: {refusal:?}").into())
}

/// Runs one launch of `plan` in a new execution directory and returns its evidence. The exit
/// status must be `exit`, and the launch must leave no staging directory behind.
fn launch_checked(
    label: &str,
    plan: &AttemptPlan,
    exit: Option<i32>,
) -> Result<AttemptEvidence, Box<dyn Error>> {
    let dir = ExecutionDir::new(label)?;
    let evidence = evidence_of(launch(plan, dir.path()))?;
    assert_eq!(
        evidence.exit.code(),
        exit,
        "the {label} payload must exit with status {exit:?}, got {:?}",
        evidence.exit
    );
    assert!(
        !dir.path().join("input").exists(),
        "the staging directory of the {label} attempt must be removed"
    );
    Ok(evidence)
}

/// Requires that one launch of `plan` exits with `exit`, leaves no staging directory and refuses
/// its output as `expected`.
fn assert_refused_launch(
    label: &str,
    plan: &AttemptPlan,
    exit: Option<i32>,
    expected: OutputRefusal,
) -> Result<(), Box<dyn Error>> {
    let evidence = launch_checked(label, plan, exit)?;
    assert_eq!(
        evidence.output.as_ref().err(),
        Some(&expected),
        "the {label} output must be refused as {expected:?}"
    );
    Ok(())
}

/// Requires that the `label` scenario of the fixture plan exits with status 0 and refuses its
/// output as `reason`.
fn assert_scenario_refused(label: &str, reason: CollectionRefusal) -> Result<(), Box<dyn Error>> {
    assert_refused_launch(
        label,
        &AttemptPlan::fixture(),
        Some(0),
        OutputRefusal::Refused(reason),
    )
}

/// Requires that the fixture plan with `plant` in its output root exits with status 0 and refuses
/// the output as `reason`.
fn assert_planted_refused(
    label: &str,
    plant: fn(&OwnedFd) -> io::Result<()>,
    reason: CollectionRefusal,
) -> Result<(), Box<dyn Error>> {
    let plan = AttemptPlan {
        plant: Some(plant),
        ..AttemptPlan::fixture()
    };
    assert_refused_launch(label, &plan, Some(0), OutputRefusal::Refused(reason))
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_input_becomes_the_expected_output() -> Result<(), Box<dyn Error>> {
    let shmem_before = shmem_kib()?;
    let evidence = launch_checked("io-copy", &AttemptPlan::fixture(), Some(0))?;
    let shmem_after = shmem_kib()?;
    let text = stdout_text(&evidence)?;
    assert!(
        has_line(text, "copy done"),
        "the copy payload must print its `copy done` line"
    );
    let tool_root = evidence.report.tool_manifest_sha256.clone();
    let filter = evidence.filter_sha256.clone();
    let collected = collected_of("io-copy", evidence.output)?;

    assert_eq!(
        files_of(&collected),
        copied_files(),
        "the output must hold the copied page and the asset, in path order"
    );

    let page = [FIXTURE_PAGE, COPIED].concat();
    let asset_line = format!(
        "file\tassets/result.txt\t{}\t{}\n",
        ASSET.len(),
        sha256_hex(ASSET)
    );
    let page_line = format!("file\tindex.html\t{}\t{}\n", page.len(), sha256_hex(&page));
    let listing = format!(
        "artifree-input-listing-v1\nindex.html\t{}\t{}\n",
        FIXTURE_PAGE.len(),
        sha256_hex(FIXTURE_PAGE)
    );
    let expected = format!(
        "artifree-output-manifest-v1\n\
         profile\t{}\n\
         profile-manifest\t{}\n\
         tool-root\t{tool_root}\n\
         filter\t{filter}\n\
         input\t{}\n\
         {asset_line}{page_line}",
        FIXTURE.identity(),
        sha256_hex(FIXTURE.manifest()),
        sha256_hex(listing.as_bytes()),
    );
    let manifest = &collected.manifest;
    assert_eq!(
        manifest.canonical_text(),
        expected,
        "the canonical manifest text must bind the profile, the tool root, the filter, the input \
         listing and the files"
    );
    let expected_digest: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    assert_eq!(
        manifest.digest(),
        expected_digest,
        "the manifest digest must be the SHA-256 of its canonical text"
    );

    let growth = shmem_after.abs_diff(shmem_before);
    assert!(
        growth < SHMEM_GROWTH_LIMIT_KIB,
        "the Shmem line must grow by less than 16 MiB across the copy, it grew by {growth} KiB"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_input_cannot_be_changed() -> Result<(), Box<dyn Error>> {
    let evidence = launch_checked("io-input-write", &AttemptPlan::fixture(), Some(0))?;
    let text = stdout_text(&evidence)?;
    for line in INPUT_LINES {
        assert!(
            has_line(text, line),
            "the input-write payload must print `{line}`"
        );
    }
    let mount = text
        .lines()
        .find_map(|line| line.strip_prefix("input-write mount "))
        .ok_or("the input-write payload printed no mount line")?;
    // The mountinfo fields after the scenario's two words: the id, the parent, the device, the
    // root, the mount point (field 4) and the mount options (field 5).
    let fields: Vec<&str> = mount.split_whitespace().collect();
    assert_eq!(
        fields.get(4).copied(),
        Some("/input"),
        "the mount line must name /input as its mount point: {mount}"
    );
    let options = fields.get(5).ok_or("the mount line has no options field")?;
    for flag in INPUT_FLAGS {
        assert!(
            options.split(',').any(|option| option == flag),
            "/input must be mounted with {flag}: {mount}"
        );
    }

    let collected = collected_of("io-input-write", evidence.output)?;
    assert_eq!(
        files_of(&collected),
        vec![("index.html".to_owned(), FIXTURE_PAGE.to_vec())],
        "the staged page must be unchanged, so the output is the input page"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_socket_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-socket", CollectionRefusal::Socket)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_many_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused(
        "out-many",
        CollectionRefusal::Package(PackageReason::TooManyFiles),
    )?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_big_file_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-big-file", CollectionRefusal::FileTooLarge)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_big_total_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-big-total", CollectionRefusal::TooLarge)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_bomb_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-bomb", CollectionRefusal::TooManyEntries)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_deep_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-deep", CollectionRefusal::TooDeep)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_invalid_name_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-invalid-name", CollectionRefusal::InvalidPath)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_collision_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused(
        "out-collision",
        CollectionRefusal::Package(PackageReason::CaseCollision),
    )?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_extension_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused("out-extension", CollectionRefusal::InvalidPath)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_output_out_no_entry_is_refused() -> Result<(), Box<dyn Error>> {
    assert_scenario_refused(
        "out-no-entry",
        CollectionRefusal::Package(PackageReason::MissingEntry),
    )?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_planted_entries_are_refused() -> Result<(), Box<dyn Error>> {
    assert_planted_refused("planted-link", plant_link, CollectionRefusal::Link)?;
    assert_planted_refused("planted-hard", plant_hard_link, CollectionRefusal::HardLink)?;
    assert_planted_refused("planted-fifo", plant_fifo, CollectionRefusal::Fifo)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_failed_attempt_is_not_collected() -> Result<(), Box<dyn Error>> {
    assert_refused_launch(
        "out-fail",
        &AttemptPlan::fixture(),
        Some(3),
        OutputRefusal::NotCollected,
    )?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_fresh_attempt_succeeds_after_a_refused_one() -> Result<(), Box<dyn Error>> {
    assert_planted_refused("fresh-first", plant_link, CollectionRefusal::Link)?;
    let evidence = launch_checked("fresh-second", &AttemptPlan::fixture(), Some(0))?;
    let collected = collected_of("fresh-second", evidence.output)?;
    assert_eq!(
        files_of(&collected),
        copied_files(),
        "the second attempt must collect the copied page and the asset"
    );
    assert_no_attempt_groups()
}
