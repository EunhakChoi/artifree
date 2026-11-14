//! Unit tests of the attempt registry (decision 0056, amendment of 2026-11-14; decision 0042 rule
//! 6): the identity and its group name, the strict record format, the decision on a record, the
//! quarantine file, the registry directory, and reconciliation against a fake cgroup mount. No test
//! touches a real cgroup. Each test works in a temporary directory that is removed when the test
//! ends, whether it passed or not.

use std::env;
use std::fs::{self, DirBuilder, Permissions};
use std::io::{self, ErrorKind};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process;

use rustix::fs::{Mode, OFlags};

use super::{
    Decision, Entry, HostIdentity, Identity, QuarantineReason, ReconcileOutcome, Reconciled,
    Record, RecordRefusal, Registry, RegistryRefusal, decide, kill_and_await_empty,
    persist_quarantine, quarantine_present, reconcile,
};

/// The machine identity of the test host, as 32 lowercase hexadecimal digits.
const MACHINE: &str = "0123456789abcdef0123456789abcdef";
/// The machine identity of another machine.
const OTHER_MACHINE: &str = "fedcba9876543210fedcba9876543210";
/// The boot identity of the test host.
const BOOT: &str = "01234567-89ab-cdef-0123-456789abcdef";
/// The boot identity of a later boot of the test host.
const OTHER_BOOT: &str = "11111111-2222-3333-4444-555555555555";
/// The cgroup namespace of the test supervisor.
const NAMESPACE: &str = "cgroup:[4026531835]";
/// Another cgroup namespace of the same boot.
const OTHER_NAMESPACE: &str = "cgroup:[4026531999]";
/// The delegated parent of the attempt group in the test records.
const PARENT: &str = "system.slice/x.service";
/// The hexadecimal form of `sample_identity`, written out here rather than computed by the code.
const IDENTITY_HEX: &str = "0102030405060708090a0b0c0d0e0f10";
/// The group name of `sample_identity`, written out here rather than computed by the code.
const GROUP: &str = "attempt-72623859790382856-651345242494996240";

/// The tag line of a valid record.
const TAG_LINE: &str = "artifree-attempt-v1";
/// The identity line of a valid record.
const IDENTITY_LINE: &str = "identity=0102030405060708090a0b0c0d0e0f10";
/// The machine line of a valid record.
const MACHINE_LINE: &str = "machine=0123456789abcdef0123456789abcdef";
/// The boot line of a valid record.
const BOOT_LINE: &str = "boot=01234567-89ab-cdef-0123-456789abcdef";
/// The cgroup namespace line of a valid record.
const NAMESPACE_LINE: &str = "cgroupns=cgroup:[4026531835]";
/// The parent line of a valid record.
const PARENT_LINE: &str = "parent=system.slice/x.service";
/// The group line of a valid record.
const GROUP_LINE: &str = "group=attempt-72623859790382856-651345242494996240";
/// The seven lines of a valid record, in order.
const VALID_LINES: [&str; 7] = [
    TAG_LINE,
    IDENTITY_LINE,
    MACHINE_LINE,
    BOOT_LINE,
    NAMESPACE_LINE,
    PARENT_LINE,
    GROUP_LINE,
];

/// A temporary directory with mode `0700`, removed when the guard is dropped.
#[derive(Debug)]
struct TempDir(PathBuf);

impl TempDir {
    /// Makes a fresh directory whose name holds `label` and this process, replacing a leftover of
    /// the same name.
    ///
    /// # Errors
    /// Returns the I/O error of a leftover that cannot be removed or of a directory that cannot be
    /// made.
    fn new(label: &str) -> io::Result<Self> {
        let path = env::temp_dir().join(format!("artifree-registry-{}-{label}", process::id()));
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    /// The directory's path.
    fn path(&self) -> &Path {
        &self.0
    }

    /// The path of `name` inside the directory.
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A drop has no one to report a failed removal to; `TempDir::new` reports a leftover that
        // cannot be removed when the next test of the same name starts.
        fs::remove_dir_all(&self.0).ok();
    }
}

/// The identity 1 to 16, whose bytes are easy to read in a failing assertion.
fn sample_identity() -> Identity {
    Identity([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16])
}

/// The record of `sample_identity` on the test host, with the group name written out.
fn sample_record() -> Record {
    Record {
        identity: sample_identity(),
        machine: MACHINE.to_owned(),
        boot: BOOT.to_owned(),
        cgroupns: NAMESPACE.to_owned(),
        parent: PARENT.to_owned(),
        group: GROUP.to_owned(),
        run: None,
    }
}

/// The host that the test records were written on.
fn test_host() -> HostIdentity {
    HostIdentity {
        machine: MACHINE.to_owned(),
        boot: BOOT.to_owned(),
        cgroupns: NAMESPACE.to_owned(),
    }
}

/// A record of `identity` written on `facts`, under `parent`. Its group is the identity's group
/// name, as a valid record's group must be.
fn record_on(identity: Identity, facts: &HostIdentity, parent: &str) -> Record {
    Record {
        identity,
        machine: facts.machine.clone(),
        boot: facts.boot.clone(),
        cgroupns: facts.cgroupns.clone(),
        parent: parent.to_owned(),
        group: identity.group_name(),
        run: None,
    }
}

/// The text of a record made of `lines`, each followed by a newline.
fn record_text(lines: &[&str]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The bytes of a record made of `lines`, each followed by a newline.
fn record_bytes(lines: &[&str]) -> Vec<u8> {
    record_text(lines).into_bytes()
}

/// The valid record with the line at `position` replaced by `line`.
fn replaced(position: usize, line: &str) -> Vec<u8> {
    let lines: Vec<&str> = VALID_LINES
        .iter()
        .enumerate()
        .map(|(index, &existing)| if index == position { line } else { existing })
        .collect();
    record_bytes(&lines)
}

/// The valid record, with `x` appended to its cgroup namespace until it is `total` bytes long.
fn padded_to(total: usize) -> io::Result<Vec<u8>> {
    let base = record_text(&VALID_LINES).len();
    let padding = total.checked_sub(base).ok_or_else(|| {
        io::Error::other("the valid record is longer than the size that a test asks for")
    })?;
    let namespace = format!("{NAMESPACE_LINE}{}", "x".repeat(padding));
    Ok(record_bytes(&[
        TAG_LINE,
        IDENTITY_LINE,
        MACHINE_LINE,
        BOOT_LINE,
        namespace.as_str(),
        PARENT_LINE,
        GROUP_LINE,
    ]))
}

/// The records that the layout check refuses, each with the refusal that the spec gives it.
fn layout_cases() -> io::Result<Vec<(&'static str, Vec<u8>, RecordRefusal)>> {
    Ok(vec![
        (
            "a bad tag line",
            replaced(0, "artifree-attempt-v2"),
            RecordRefusal::Layout,
        ),
        (
            "a missing boot line",
            record_bytes(&[
                TAG_LINE,
                IDENTITY_LINE,
                MACHINE_LINE,
                NAMESPACE_LINE,
                PARENT_LINE,
                GROUP_LINE,
            ]),
            RecordRefusal::Layout,
        ),
        (
            "an extra line before the group",
            record_bytes(&[
                TAG_LINE,
                IDENTITY_LINE,
                MACHINE_LINE,
                BOOT_LINE,
                NAMESPACE_LINE,
                PARENT_LINE,
                "extra=1",
                GROUP_LINE,
            ]),
            RecordRefusal::Layout,
        ),
        (
            "an extra line after the group",
            record_bytes(&[
                TAG_LINE,
                IDENTITY_LINE,
                MACHINE_LINE,
                BOOT_LINE,
                NAMESPACE_LINE,
                PARENT_LINE,
                GROUP_LINE,
                "extra=1",
            ]),
            RecordRefusal::Layout,
        ),
        (
            "the identity and the machine swapped",
            record_bytes(&[
                TAG_LINE,
                MACHINE_LINE,
                IDENTITY_LINE,
                BOOT_LINE,
                NAMESPACE_LINE,
                PARENT_LINE,
                GROUP_LINE,
            ]),
            RecordRefusal::Layout,
        ),
        (
            "the identity repeated in place of the machine",
            replaced(2, IDENTITY_LINE),
            RecordRefusal::Layout,
        ),
        (
            "no trailing newline",
            without_final_newline(),
            RecordRefusal::Layout,
        ),
        (
            "a blank line after the group",
            record_bytes(&[
                TAG_LINE,
                IDENTITY_LINE,
                MACHINE_LINE,
                BOOT_LINE,
                NAMESPACE_LINE,
                PARENT_LINE,
                GROUP_LINE,
                "",
            ]),
            RecordRefusal::Layout,
        ),
        (
            "bytes that are not UTF-8",
            b"artifree-attempt-v1\n\xff\n".to_vec(),
            RecordRefusal::NotText,
        ),
        ("1025 bytes", padded_to(1025)?, RecordRefusal::TooLong),
    ])
}

/// The valid record without its final newline.
fn without_final_newline() -> Vec<u8> {
    let mut bytes = record_bytes(&VALID_LINES);
    let newline = bytes.pop();
    assert_eq!(newline, Some(b'\n'), "the fixture ends with a newline");
    bytes
}

/// The records whose fields are outside their formats, each with the refusal that the spec gives.
fn field_cases() -> Vec<(&'static str, Vec<u8>, RecordRefusal)> {
    let other_group = format!("group={}", Identity([0xee; 16]).group_name());
    vec![
        (
            "an uppercase identity",
            replaced(1, "identity=0102030405060708090A0B0C0D0E0F10"),
            RecordRefusal::IdentityField,
        ),
        (
            "an uppercase machine",
            replaced(2, "machine=0123456789ABCDEF0123456789ABCDEF"),
            RecordRefusal::MachineField,
        ),
        (
            "a boot identity of 35 characters",
            replaced(3, "boot=01234567-89ab-cdef-0123-456789abcde"),
            RecordRefusal::BootField,
        ),
        (
            "a boot identity with its dashes moved",
            replaced(3, "boot=0123456-789ab-cdef-0123-456789abcdef"),
            RecordRefusal::BootField,
        ),
        (
            "an uppercase boot identity",
            replaced(3, "boot=01234567-89AB-cdef-0123-456789abcdef"),
            RecordRefusal::BootField,
        ),
        (
            "an empty cgroup namespace",
            replaced(4, "cgroupns="),
            RecordRefusal::NamespaceField,
        ),
        (
            "a tab in the cgroup namespace",
            replaced(4, "cgroupns=cgroup:\t[4026531835]"),
            RecordRefusal::NamespaceField,
        ),
        (
            "the group name of another identity",
            replaced(6, &other_group),
            RecordRefusal::ForeignGroup,
        ),
        (
            "an absolute parent",
            replaced(5, "parent=/system.slice"),
            RecordRefusal::ParentField,
        ),
        (
            "a parent with a dot component",
            replaced(5, "parent=system.slice/./x.service"),
            RecordRefusal::ParentField,
        ),
        (
            "a parent ending in a dot-dot component",
            replaced(5, "parent=a/supervisor/.."),
            RecordRefusal::ParentField,
        ),
        (
            "a parent with an empty component",
            replaced(5, "parent=a//b"),
            RecordRefusal::ParentField,
        ),
        (
            "an empty parent",
            replaced(5, "parent="),
            RecordRefusal::ParentField,
        ),
        (
            "a parent with a NUL byte",
            replaced(5, "parent=a\0b"),
            RecordRefusal::ParentField,
        ),
    ]
}

#[test]
fn identity_hex_round_trip_and_group_name() {
    let identity = sample_identity();
    assert_eq!(
        identity.hex(),
        IDENTITY_HEX,
        "each byte is written as two lowercase hexadecimal digits"
    );
    assert_eq!(
        Identity::parse_hex(IDENTITY_HEX),
        Some(identity),
        "the hexadecimal form parses back to the same identity"
    );
    assert_eq!(
        identity.group_name(),
        GROUP,
        "the group name is made of the first and the last eight bytes as decimal numbers"
    );
}

#[test]
fn parse_hex_refuses_anything_but_32_lowercase_digits() {
    assert_eq!(
        Identity::parse_hex("0102030405060708090A0B0C0D0E0F10"),
        None,
        "uppercase digits are refused"
    );
    assert_eq!(
        Identity::parse_hex("0102030405060708090a0b0c0d0e0f1"),
        None,
        "31 digits are refused"
    );
    assert_eq!(
        Identity::parse_hex(&format!("{IDENTITY_HEX}0")),
        None,
        "33 digits are refused"
    );
    assert_eq!(
        Identity::parse_hex("0102030405060708090a0b0c0d0e0f1g"),
        None,
        "a character that is not a hexadecimal digit is refused"
    );
}

#[test]
fn record_renders_seven_lines_and_parses_back() {
    let record = sample_record();
    assert_eq!(
        record.render(),
        record_text(&VALID_LINES),
        "a record is the tag line and six fields, one per line"
    );
    assert_eq!(
        Record::parse(&record_bytes(&VALID_LINES)),
        Ok(record),
        "the rendered text parses back to the same record"
    );
}

#[test]
fn malformed_records_are_refused() -> io::Result<()> {
    let mut cases = layout_cases()?;
    cases.extend(field_cases());
    for (label, bytes, refusal) in cases {
        assert_eq!(
            Record::parse(&bytes),
            Err(refusal),
            "{label} must be refused"
        );
    }
    Ok(())
}

#[test]
fn a_record_of_exactly_1024_bytes_is_accepted() -> io::Result<()> {
    let bytes = padded_to(1024)?;
    assert_eq!(bytes.len(), 1024, "the fixture is exactly at the limit");
    assert_eq!(
        Record::parse(&bytes).map(|record| record.group),
        Ok(GROUP.to_owned()),
        "a record of 1024 bytes is within the limit"
    );
    Ok(())
}

#[test]
fn decide_checks_the_machine_then_the_boot_then_the_namespace() {
    let record = sample_record();
    let cases = [
        ("the same host", test_host(), Decision::SameBoot),
        (
            "another boot",
            HostIdentity {
                boot: OTHER_BOOT.to_owned(),
                ..test_host()
            },
            Decision::ChangedBoot,
        ),
        (
            "another namespace",
            HostIdentity {
                cgroupns: OTHER_NAMESPACE.to_owned(),
                ..test_host()
            },
            Decision::OtherNamespace,
        ),
        (
            "another boot and namespace",
            HostIdentity {
                boot: OTHER_BOOT.to_owned(),
                cgroupns: OTHER_NAMESPACE.to_owned(),
                ..test_host()
            },
            Decision::ChangedBoot,
        ),
        (
            "another machine",
            HostIdentity {
                machine: OTHER_MACHINE.to_owned(),
                ..test_host()
            },
            Decision::OtherMachine,
        ),
        (
            "another machine, boot and namespace",
            HostIdentity {
                machine: OTHER_MACHINE.to_owned(),
                boot: OTHER_BOOT.to_owned(),
                cgroupns: OTHER_NAMESPACE.to_owned(),
            },
            Decision::OtherMachine,
        ),
    ];
    for (label, facts, expected) in cases {
        assert_eq!(decide(&record, &facts), expected, "{label}");
    }
}

/// The `proc` directory of a test, whose `ns/cgroup` link names `NAMESPACE`.
fn proc_fixture(root: &TempDir) -> io::Result<PathBuf> {
    let proc_self = root.join("proc");
    fs::create_dir_all(proc_self.join("ns"))?;
    symlink(NAMESPACE, proc_self.join("ns").join("cgroup"))?;
    Ok(proc_self)
}

#[test]
fn host_identity_reads_the_machine_boot_and_namespace() -> io::Result<()> {
    let root = TempDir::new("host-read")?;
    let proc_self = proc_fixture(&root)?;
    let machine_id = root.join("machine-id");
    let boot_id = root.join("boot_id");
    fs::write(&machine_id, format!("{MACHINE}\n"))?;
    fs::write(&boot_id, format!("{BOOT}\n"))?;
    assert_eq!(
        HostIdentity::read(&proc_self, &machine_id, &boot_id)?,
        test_host(),
        "the surrounding whitespace is trimmed and the namespace link is read"
    );
    Ok(())
}

#[test]
fn host_identity_refuses_values_outside_their_format() -> io::Result<()> {
    let root = TempDir::new("host-refuse")?;
    let proc_self = proc_fixture(&root)?;
    let machine_id = root.join("machine-id");
    let boot_id = root.join("boot_id");
    let cases = [
        (
            "an uppercase machine identity",
            "0123456789ABCDEF0123456789ABCDEF\n",
            "01234567-89ab-cdef-0123-456789abcdef\n",
        ),
        (
            "a boot identity of 35 characters",
            "0123456789abcdef0123456789abcdef\n",
            "01234567-89ab-cdef-0123-456789abcde\n",
        ),
        (
            "a boot identity without dashes",
            "0123456789abcdef0123456789abcdef\n",
            "0123456789abcdef0123456789abcdef\n",
        ),
    ];
    for (label, machine_text, boot_text) in cases {
        fs::write(&machine_id, machine_text)?;
        fs::write(&boot_id, boot_text)?;
        assert_eq!(
            HostIdentity::read(&proc_self, &machine_id, &boot_id).map_err(|error| error.kind()),
            Err(ErrorKind::InvalidData),
            "{label} must be refused as invalid data"
        );
    }
    assert_eq!(
        HostIdentity::read(&proc_self, &root.join("absent"), &boot_id)
            .map_err(|error| error.kind()),
        Err(ErrorKind::NotFound),
        "a missing machine identity is a read error of its own"
    );
    Ok(())
}

#[test]
fn quarantine_is_absent_until_it_is_persisted() -> io::Result<()> {
    let root = TempDir::new("quarantine-absent")?;
    assert!(
        !quarantine_present(root.path()),
        "a fresh execution directory holds no quarantine"
    );
    persist_quarantine(root.path(), QuarantineReason::Routing)?;
    assert!(
        quarantine_present(root.path()),
        "a persisted quarantine is present"
    );
    Ok(())
}

#[test]
fn a_failed_quarantine_lookup_counts_as_present() -> io::Result<()> {
    let root = TempDir::new("quarantine-lookup")?;
    // The execution directory is a file, so the lookup below fails with `ENOTDIR`, not `ENOENT`.
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, b"not a directory")?;
    assert!(
        quarantine_present(&blocker),
        "a lookup that fails for any reason but an absent file must fail closed"
    );
    Ok(())
}

#[test]
fn persisting_again_keeps_the_first_reason() -> io::Result<()> {
    let root = TempDir::new("quarantine-twice")?;
    persist_quarantine(root.path(), QuarantineReason::Routing)?;
    persist_quarantine(root.path(), QuarantineReason::Cleanup)?;
    assert_eq!(
        fs::read_to_string(root.join("quarantine"))?,
        "artifree-quarantine-v1\nreason=routing\n",
        "the second persist is accepted and the file keeps the first reason"
    );
    Ok(())
}

#[test]
fn quarantine_records_each_reason_word() -> io::Result<()> {
    let root = TempDir::new("quarantine-words")?;
    let cases = [
        (QuarantineReason::Cleanup, "cleanup"),
        (QuarantineReason::Routing, "routing"),
        (QuarantineReason::Guardian, "guardian"),
        (QuarantineReason::Registry, "registry"),
    ];
    for (reason, word) in cases {
        let directory = root.join(word);
        DirBuilder::new().mode(0o700).create(&directory)?;
        persist_quarantine(&directory, reason)?;
        assert_eq!(
            fs::read_to_string(directory.join("quarantine"))?,
            format!("artifree-quarantine-v1\nreason={word}\n"),
            "the persisted file names the reason {word}"
        );
    }
    Ok(())
}

/// The mode bits of `path`, without its file type.
fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.mode() & 0o777)
}

/// The result of opening the registry at `path`, reduced to its error kind.
fn open_kind(path: &Path) -> Result<(), ErrorKind> {
    Registry::open(path)
        .map(|_| ())
        .map_err(|error| error.kind())
}

#[test]
fn open_creates_the_attempts_directory_private_to_the_account() -> io::Result<()> {
    let root = TempDir::new("registry-open")?;
    Registry::open(root.path())?;
    assert_eq!(
        mode_of(&root.join("attempts"))?,
        0o700,
        "the attempts directory is created with mode 0700"
    );
    Ok(())
}

#[test]
fn open_refuses_an_attempts_directory_that_others_can_enter() -> io::Result<()> {
    let root = TempDir::new("registry-shared")?;
    Registry::open(root.path())?;
    fs::set_permissions(root.join("attempts"), Permissions::from_mode(0o750))?;
    assert_eq!(
        open_kind(root.path()),
        Err(ErrorKind::PermissionDenied),
        "a group-readable attempts directory is refused"
    );
    Ok(())
}

#[test]
fn open_refuses_an_attempts_entry_that_is_not_a_directory() -> io::Result<()> {
    let root = TempDir::new("registry-file")?;
    fs::write(root.join("attempts"), b"not a directory")?;
    assert_eq!(
        open_kind(root.path()),
        Err(ErrorKind::NotADirectory),
        "a file where the attempts directory belongs is refused"
    );
    Ok(())
}

#[test]
fn a_written_record_is_listed_with_its_bytes_and_private_mode() -> io::Result<()> {
    let root = TempDir::new("registry-write")?;
    let registry = Registry::open(root.path())?;
    registry.write(&sample_record())?;
    assert_eq!(
        registry.entries()?,
        [Entry::Record {
            identity: sample_identity(),
            bytes: record_bytes(&VALID_LINES),
        }],
        "the listing shows the record's exact bytes and no temporary file"
    );
    assert_eq!(
        mode_of(&root.join("attempts").join(format!("{IDENTITY_HEX}.record")))?,
        0o600,
        "a record is readable and writable by the account only"
    );
    Ok(())
}

#[test]
fn write_refuses_a_record_that_would_not_parse_back() -> io::Result<()> {
    let root = TempDir::new("registry-refuse-write")?;
    let registry = Registry::open(root.path())?;
    let foreign_group = Record {
        group: "attempt-1-2".to_owned(),
        ..sample_record()
    };
    let absolute_parent = Record {
        parent: "/system.slice".to_owned(),
        ..sample_record()
    };
    let cases = [
        ("a foreign group", foreign_group),
        ("an absolute parent", absolute_parent),
    ];
    for (label, record) in cases {
        assert_eq!(
            registry.write(&record).map_err(|error| error.kind()),
            Err(ErrorKind::InvalidData),
            "{label} must be refused before anything is written"
        );
    }
    assert!(
        registry.entries()?.is_empty(),
        "a refused record leaves no file behind, not even a temporary one"
    );
    Ok(())
}

#[test]
fn listing_classifies_leftovers_and_sorts_by_name() -> io::Result<()> {
    let root = TempDir::new("registry-list")?;
    let registry = Registry::open(root.path())?;
    let low = record_on(Identity([0x01; 16]), &test_host(), PARENT);
    let high = record_on(Identity([0xff; 16]), &test_host(), PARENT);
    registry.write(&low)?;
    registry.write(&high)?;
    let attempts = root.join("attempts");
    let temporary = format!(".{}.tmp", "22".repeat(16));
    fs::write(attempts.join(&temporary), b"partial")?;
    let directory = format!("{}.record", "33".repeat(16));
    fs::create_dir(attempts.join(&directory))?;
    let link = format!("{}.record", "44".repeat(16));
    symlink("notes", attempts.join(&link))?;
    fs::write(attempts.join("notes"), b"stray")?;
    assert_eq!(
        registry.entries()?,
        [
            Entry::Temp { name: temporary },
            Entry::Record {
                identity: Identity([0x01; 16]),
                bytes: low.render().into_bytes(),
            },
            Entry::Other { name: directory },
            Entry::Other { name: link },
            Entry::Record {
                identity: Identity([0xff; 16]),
                bytes: high.render().into_bytes(),
            },
            Entry::Other {
                name: "notes".to_owned(),
            },
        ],
        "entries are listed in name order; a directory and a link named like a record are other"
    );
    Ok(())
}

#[test]
fn remove_takes_away_only_the_named_record() -> io::Result<()> {
    let root = TempDir::new("registry-remove")?;
    let registry = Registry::open(root.path())?;
    let kept = record_on(Identity([0xff; 16]), &test_host(), PARENT);
    registry.write(&sample_record())?;
    registry.write(&kept)?;
    registry.remove(sample_identity())?;
    assert_eq!(
        registry.entries()?,
        [Entry::Record {
            identity: Identity([0xff; 16]),
            bytes: kept.render().into_bytes(),
        }],
        "removing one record leaves the other"
    );
    Ok(())
}

#[cfg(feature = "enforcement-fixture")]
#[test]
fn a_record_is_absent_only_when_the_registry_shows_none() -> io::Result<()> {
    let root = TempDir::new("registry-absent")?;
    let registry = Registry::open(root.path())?;
    assert!(
        registry.record_absent(sample_identity()),
        "an empty registry holds no record"
    );
    registry.write(&sample_record())?;
    assert!(
        !registry.record_absent(sample_identity()),
        "a written record is present, so its attempt is not settled"
    );
    registry.remove(sample_identity())?;
    assert!(
        registry.record_absent(sample_identity()),
        "a removed record is absent"
    );
    // A lookup that fails for another reason than a missing file counts as present.
    let attempts = root.join(super::ATTEMPTS_DIR);
    fs::set_permissions(&attempts, Permissions::from_mode(0o000))?;
    let unreadable = registry.record_absent(sample_identity());
    fs::set_permissions(&attempts, Permissions::from_mode(0o700))?;
    assert!(
        !unreadable,
        "a registry that cannot be searched may hold the record"
    );
    Ok(())
}

/// The execution directory and the fake cgroup mount of a reconciliation test, both under `root`.
/// The mount is owned by this account, as a delegated parent is.
fn layout(root: &TempDir) -> io::Result<(PathBuf, PathBuf)> {
    let execution = root.join("execution");
    DirBuilder::new().mode(0o700).create(&execution)?;
    let mount = root.join("mount");
    fs::create_dir(&mount)?;
    Ok((execution, mount))
}

/// The provider's account, as the reconciliation of these tests takes it.
fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Reconciles the test's execution directory against the test host, as this account.
fn reconcile_here(execution: &Path, mount: &Path) -> Result<Vec<Reconciled>, RegistryRefusal> {
    reconcile(execution, mount, own_uid(), &test_host())
}

/// Writes `record` into the registry of `execution`.
fn write_record(execution: &Path, record: &Record) -> io::Result<()> {
    Registry::open(execution)?.write(record)
}

/// Places a raw entry named `name` with `bytes` into the registry of `execution`.
fn place_entry(execution: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    Registry::open(execution)?;
    fs::write(execution.join("attempts").join(name), bytes)
}

/// The entries of the registry of `execution`.
fn listing(execution: &Path) -> io::Result<Vec<Entry>> {
    Registry::open(execution)?.entries()
}

/// The identities of the records in the registry of `execution`, in listing order.
fn record_identities(execution: &Path) -> io::Result<Vec<Identity>> {
    let mut identities = Vec::new();
    for entry in listing(execution)? {
        if let Entry::Record { identity, .. } = entry {
            identities.push(identity);
        }
    }
    Ok(identities)
}

/// Creates the input directory that a launch stages, holding one file.
fn stage_input(execution: &Path) -> io::Result<()> {
    let input = execution.join("input");
    fs::create_dir(&input)?;
    fs::write(input.join("page.html"), b"<p>staged</p>")
}

/// Creates the attempt group `group` under `parent` of the mount, with a file that a
/// reconciliation must leave alone. The group has no `cgroup.kill`, so it can never be ended here.
fn make_group(mount: &Path, parent: &str, group: &str) -> io::Result<PathBuf> {
    let path = mount.join(parent).join(group);
    fs::create_dir_all(&path)?;
    fs::write(path.join("sentinel"), b"untouched")?;
    Ok(path)
}

#[test]
fn reconcile_removes_a_leftover_temporary_file() -> io::Result<()> {
    let root = TempDir::new("reconcile-temporary")?;
    let (execution, mount) = layout(&root)?;
    place_entry(
        &execution,
        &format!(".{IDENTITY_HEX}.tmp"),
        b"partial record",
    )?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Ok(Vec::new()),
        "a temporary file is not a record, so nothing is reconciled"
    );
    assert!(
        listing(&execution)?.is_empty(),
        "the temporary file is removed"
    );
    Ok(())
}

#[test]
fn reconcile_of_a_changed_boot_removes_its_records_and_touches_no_group() -> io::Result<()> {
    let root = TempDir::new("reconcile-changed-boot")?;
    let (execution, mount) = layout(&root)?;
    let earlier = HostIdentity {
        boot: OTHER_BOOT.to_owned(),
        ..test_host()
    };
    let low = Identity([0x01; 16]);
    let high = Identity([0xff; 16]);
    write_record(&execution, &record_on(low, &earlier, PARENT))?;
    write_record(&execution, &record_on(high, &earlier, PARENT))?;
    let low_group = make_group(&mount, PARENT, &low.group_name())?;
    let high_group = make_group(&mount, PARENT, &high.group_name())?;
    stage_input(&execution)?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Ok(vec![
            Reconciled {
                identity: low,
                outcome: ReconcileOutcome::ChangedBoot,
            },
            Reconciled {
                identity: high,
                outcome: ReconcileOutcome::ChangedBoot,
            },
        ]),
        "each record of an earlier boot is settled, in name order, as a changed boot"
    );
    assert!(listing(&execution)?.is_empty(), "both records are removed");
    assert!(
        !execution.join("input").exists(),
        "the staged input is removed"
    );
    assert!(
        low_group.join("sentinel").exists(),
        "the low group of an earlier boot is not touched"
    );
    assert!(
        high_group.join("sentinel").exists(),
        "the high group of an earlier boot is not touched"
    );
    Ok(())
}

#[test]
fn reconcile_treats_a_same_boot_record_without_its_group_as_gone() -> io::Result<()> {
    // The parent is missing in the first case; the parent is present and the group is missing in
    // the second. Both mean that no group is left to end.
    for (label, parent_exists) in [("no-parent", false), ("no-group", true)] {
        let root = TempDir::new(label)?;
        let (execution, mount) = layout(&root)?;
        if parent_exists {
            fs::create_dir_all(mount.join(PARENT))?;
        }
        let identity = sample_identity();
        write_record(&execution, &record_on(identity, &test_host(), PARENT))?;
        stage_input(&execution)?;
        assert_eq!(
            reconcile_here(&execution, &mount),
            Ok(vec![Reconciled {
                identity,
                outcome: ReconcileOutcome::Gone,
            }]),
            "{label}: the record is settled as gone"
        );
        assert!(
            listing(&execution)?.is_empty(),
            "{label}: the record is removed"
        );
        assert!(
            !execution.join("input").exists(),
            "{label}: the staged input is removed"
        );
    }
    Ok(())
}

#[test]
fn reconcile_refuses_a_group_it_cannot_kill() -> io::Result<()> {
    let root = TempDir::new("reconcile-no-kill")?;
    let (execution, mount) = layout(&root)?;
    let identity = sample_identity();
    write_record(&execution, &record_on(identity, &test_host(), PARENT))?;
    let group = make_group(&mount, PARENT, &identity.group_name())?;
    stage_input(&execution)?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Err(RegistryRefusal::Cleanup),
        "a group without cgroup.kill cannot be ended, so the reconciliation is refused"
    );
    assert_eq!(
        record_identities(&execution)?,
        [identity],
        "the record of a refused group stays"
    );
    assert!(
        execution.join("input").exists(),
        "the staged input of a refused group stays"
    );
    assert!(
        group.join("sentinel").exists(),
        "the refused group is left as it was"
    );
    Ok(())
}

/// The flags of a cgroup directory that the tests open, as the reconciliation opens one.
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);

/// Opens the cgroup directory `path` as a handle, as the reconciliation opens a parent.
fn open_handle(path: &Path) -> io::Result<OwnedFd> {
    rustix::fs::open(path, DIRECTORY_FLAGS, Mode::empty()).map_err(io::Error::from)
}

/// Opens the cgroup directory `name` beneath `parent`, as the reconciliation opens a group.
fn open_group(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    rustix::fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(io::Error::from)
}

#[test]
fn a_group_removed_after_its_lookup_is_gone() -> io::Result<()> {
    // The group is looked up and then removed, as a service manager may remove an empty group
    // between the lookup and the kill. A removed group has no process left to kill.
    let root = TempDir::new("kill-removed")?;
    let (_, mount) = layout(&root)?;
    let parent_path = mount.join(PARENT);
    fs::create_dir_all(parent_path.join(GROUP))?;
    let parent = open_handle(&parent_path)?;
    let group = open_group(&parent, GROUP)?;
    fs::remove_dir(parent_path.join(GROUP))?;
    assert_eq!(
        kill_and_await_empty(&parent, GROUP, &group),
        Ok(ReconcileOutcome::Gone),
        "a group removed after its lookup has nothing left to kill, so it is gone"
    );
    Ok(())
}

#[test]
fn a_live_group_without_cgroup_kill_is_refused_rather_than_gone() -> io::Result<()> {
    // A group that is still there cannot be killed without its cgroup.kill file, so the
    // vanished-group path must not settle it as gone: the refusal keeps the group as it is.
    let root = TempDir::new("kill-live")?;
    let (_, mount) = layout(&root)?;
    let group_path = make_group(&mount, PARENT, GROUP)?;
    let parent = open_handle(&mount.join(PARENT))?;
    let group = open_group(&parent, GROUP)?;
    assert_eq!(
        kill_and_await_empty(&parent, GROUP, &group),
        Err(RegistryRefusal::Cleanup),
        "a live group without cgroup.kill is refused, not reported as gone"
    );
    assert!(
        group_path.join("sentinel").exists(),
        "the refused group is left as it was"
    );
    Ok(())
}

#[test]
fn reconcile_refuses_a_parent_of_another_account() -> io::Result<()> {
    let root = TempDir::new("reconcile-owner")?;
    let (execution, mount) = layout(&root)?;
    let identity = sample_identity();
    write_record(&execution, &record_on(identity, &test_host(), PARENT))?;
    let group = make_group(&mount, PARENT, &identity.group_name())?;
    stage_input(&execution)?;
    // No file is owned by user -1, so this uid is never the owner of the parent.
    assert_eq!(
        reconcile(&execution, &mount, u32::MAX, &test_host()),
        Err(RegistryRefusal::Owner),
        "a parent owned by another account is refused before its group is touched"
    );
    assert_eq!(
        record_identities(&execution)?,
        [identity],
        "the record of a refused parent stays"
    );
    assert!(
        group.join("sentinel").exists(),
        "the group of a refused parent is left as it was"
    );
    Ok(())
}

#[test]
fn reconcile_refuses_records_of_another_host() -> io::Result<()> {
    let cases = [
        (
            "other-machine",
            HostIdentity {
                machine: OTHER_MACHINE.to_owned(),
                ..test_host()
            },
            RegistryRefusal::OtherMachine,
        ),
        (
            "other-machine-and-boot",
            HostIdentity {
                machine: OTHER_MACHINE.to_owned(),
                boot: OTHER_BOOT.to_owned(),
                ..test_host()
            },
            RegistryRefusal::OtherMachine,
        ),
        (
            "other-namespace",
            HostIdentity {
                cgroupns: OTHER_NAMESPACE.to_owned(),
                ..test_host()
            },
            RegistryRefusal::OtherNamespace,
        ),
    ];
    for (label, writer, refusal) in cases {
        let root = TempDir::new(label)?;
        let (execution, mount) = layout(&root)?;
        let identity = sample_identity();
        write_record(&execution, &record_on(identity, &writer, PARENT))?;
        // The group exists, so a same-boot reconciliation would try to end it.
        let group = make_group(&mount, PARENT, &identity.group_name())?;
        stage_input(&execution)?;
        assert_eq!(
            reconcile_here(&execution, &mount),
            Err(refusal),
            "{label} must be refused"
        );
        assert_eq!(
            record_identities(&execution)?,
            [identity],
            "{label}: the record stays"
        );
        assert!(
            execution.join("input").exists(),
            "{label}: the staged input stays"
        );
        assert!(
            group.join("sentinel").exists(),
            "{label}: the group is not touched"
        );
    }
    Ok(())
}

#[test]
fn reconcile_refuses_a_stray_entry() -> io::Result<()> {
    let root = TempDir::new("reconcile-stray")?;
    let (execution, mount) = layout(&root)?;
    place_entry(&execution, "notes", b"stray")?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Err(RegistryRefusal::Entry),
        "an entry that is not a record or a temporary file is refused"
    );
    assert!(
        execution.join("attempts").join("notes").exists(),
        "the stray entry stays"
    );
    Ok(())
}

#[test]
fn reconcile_refuses_a_record_that_does_not_parse() -> io::Result<()> {
    let root = TempDir::new("reconcile-malformed")?;
    let (execution, mount) = layout(&root)?;
    place_entry(
        &execution,
        &format!("{IDENTITY_HEX}.record"),
        b"not a record\n",
    )?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Err(RegistryRefusal::Record(RecordRefusal::Layout)),
        "a record without the tag line is refused as a layout error"
    );
    assert_eq!(
        listing(&execution)?,
        [Entry::Record {
            identity: sample_identity(),
            bytes: b"not a record\n".to_vec(),
        }],
        "the malformed record stays"
    );
    Ok(())
}

#[test]
fn reconcile_refuses_a_record_filed_under_another_identity() -> io::Result<()> {
    let root = TempDir::new("reconcile-misfiled")?;
    let (execution, mount) = layout(&root)?;
    place_entry(
        &execution,
        &format!("{}.record", "ff".repeat(16)),
        &record_bytes(&VALID_LINES),
    )?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Err(RegistryRefusal::Entry),
        "a record whose identity differs from its file name is refused"
    );
    Ok(())
}

#[test]
fn reconcile_stops_at_the_first_refusal() -> io::Result<()> {
    let root = TempDir::new("reconcile-first-refusal")?;
    let (execution, mount) = layout(&root)?;
    // The zero identity sorts before the other one, so its malformed record is refused first.
    let zero = Identity([0x00; 16]);
    place_entry(
        &execution,
        &format!("{}.record", "00".repeat(16)),
        b"not a record\n",
    )?;
    let later = Identity([0xff; 16]);
    let earlier = HostIdentity {
        boot: OTHER_BOOT.to_owned(),
        ..test_host()
    };
    write_record(&execution, &record_on(later, &earlier, PARENT))?;
    let later_group = make_group(&mount, PARENT, &later.group_name())?;
    stage_input(&execution)?;
    assert_eq!(
        reconcile_here(&execution, &mount),
        Err(RegistryRefusal::Record(RecordRefusal::Layout)),
        "the first malformed record is the refusal"
    );
    assert_eq!(
        listing(&execution)?,
        [
            Entry::Record {
                identity: zero,
                bytes: b"not a record\n".to_vec(),
            },
            Entry::Record {
                identity: later,
                bytes: record_on(later, &earlier, PARENT).render().into_bytes(),
            },
        ],
        "the record after the refusal is not settled or removed"
    );
    assert!(
        execution.join("input").exists(),
        "the input stays, because no record after the refusal was settled"
    );
    assert!(
        later_group.join("sentinel").exists(),
        "the group of the record after the refusal is not touched"
    );
    Ok(())
}
