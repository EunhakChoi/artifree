//! The attempt registry of decision 0042 rule 6, as decision 0056's amendment of 2026-11-14
//! (the attempt registry) sets it.
//!
//! The registry is the directory `attempts` inside the execution directory, mode `0700`. It holds
//! `<identity>.record` for each attempt that may still own a resource, and `.<identity>.tmp` for a
//! record being written. A record is at most 1 KiB of text. The record holds no file content,
//! input, output, credential or digest of them. The persisted quarantine is the file `quarantine`
//! in the execution directory, which records the reason that the provider was quarantined.
//!
//! Nothing in the product removes the quarantine file; the operator removes it after settling the
//! host.
//!
//! The database identity and the job's identity and fence of 0042 rule 6 are R02b's; they would
//! come as further lines of a `v2` record.
//!
//! The module is compiled only into the test build (0056 rule 16), like the launch that uses it.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::str::Split;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

use super::probe_group;
use super::staging::{INPUT_DIR, remove_tree};

/// The directory of records, inside the execution directory.
pub(super) const ATTEMPTS_DIR: &str = "attempts";
/// The persisted quarantine file, inside the execution directory.
pub(super) const QUARANTINE_FILE: &str = "quarantine";
/// The first line of every record.
pub(super) const RECORD_TAG: &str = "artifree-attempt-v1";
/// The first line of the persisted quarantine file.
pub(super) const QUARANTINE_TAG: &str = "artifree-quarantine-v1";
/// The largest record, in bytes; a longer file is refused before it is parsed.
pub(super) const RECORD_LIMIT: usize = 1024;
/// The text that every quarantine refusal shows the operator.
pub(super) const OPERATOR_TEXT: &str = "operator action is required: settle the host (a record of another machine means its remote cleanup cannot be proved), remove each record in the execution directory's attempts directory that the host no longer needs, then remove the quarantine file of the execution directory";
/// The machine identity file, whose 32 digits identify the machine.
pub(super) const MACHINE_ID: &str = "/etc/machine-id";
/// The kernel's boot identity file, which changes at every boot.
pub(super) const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
/// The suffix of a record's file name, after the identity's 32 digits.
const RECORD_SUFFIX: &str = ".record";
/// The suffix of a temporary file's name, after the identity's 32 digits and the leading dot.
const TEMPORARY_SUFFIX: &str = ".tmp";
/// The flags of the attempts directory and of the execution directory: read-only, a directory, no
/// link followed, and closed on exec.
const OPEN_DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
/// The flags of a cgroup directory opened beneath a handle. The resolution flags refuse links, so
/// the no-follow flag is not needed here.
const CGROUP_DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);
/// The flags of a record opened for reading. A link is refused with `ELOOP`, and a FIFO is opened
/// without waiting for a writer, so that it is refused as not a regular file.
const OPEN_RECORD: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
/// The flags of a file created by the registry: created exclusively, never following a link.
const CREATE_FILE: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
/// The mode of the attempts directory, which only the provider's account may use.
const PRIVATE_DIRECTORY: Mode = Mode::from_raw_mode(0o700);
/// The mode of every record and quarantine file.
const PRIVATE_FILE: Mode = Mode::from_raw_mode(0o600);
/// The resolution of a path beneath a cgroup handle: no escape, no link and no magic link.
const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS);
/// The most bytes of a record that are read: the record limit and one more, so that a longer file
/// is seen as too long.
const RECORD_READ_BYTES: u64 = 1025;
/// The most bytes of a `cgroup.events` file that are read.
const EVENTS_BYTES: u64 = 4096;
/// The wait between two reads of `cgroup.events` while a group is being killed.
const POLL: Duration = Duration::from_millis(10);

/// The 16 random bytes that name one attempt and its record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity(pub(super) [u8; 16]);

impl Identity {
    /// Sixteen bytes from the system's randomness source.
    ///
    /// # Errors
    /// Returns an error when the system has no randomness source.
    pub(super) fn random() -> io::Result<Self> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(bytes))
    }

    /// The identity as 32 lowercase hexadecimal digits.
    #[must_use]
    pub(super) fn hex(&self) -> String {
        // Every nibble is below 16, so `from_digit` never returns `None`.
        self.0
            .iter()
            .flat_map(|&byte| [byte >> 4, byte & 0x0f])
            .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
            .collect()
    }

    /// Parses exactly 32 lowercase hexadecimal digits, the form that `hex` writes.
    #[must_use]
    pub(super) fn parse_hex(text: &str) -> Option<Self> {
        if !is_lower_hex(text, 32) {
            return None;
        }
        let mut bytes = [0; 16];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// The attempt group's name, `attempt-<a>-<b>`: the first and the last eight bytes as unsigned
    /// decimal numbers, which is the form the guardian accepts.
    #[must_use]
    pub(super) fn group_name(&self) -> String {
        let [high @ .., _, _, _, _, _, _, _, _] = self.0;
        let [_, _, _, _, _, _, _, _, low @ ..] = self.0;
        format!(
            "attempt-{}-{}",
            u64::from_be_bytes(high),
            u64::from_be_bytes(low)
        )
    }
}

/// Whether `text` is exactly `len` lowercase hexadecimal digits.
#[must_use]
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `text` has the shape of a kernel boot identity: 36 characters in five groups of 8, 4,
/// 4, 4 and 12 lowercase hexadecimal digits separated by hyphens.
#[must_use]
fn is_boot_id(text: &str) -> bool {
    text.len() == 36
        && text.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && text
            .bytes()
            .all(|byte| byte == b'-' || matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `text` is a cgroup namespace name: non-empty, with no control character.
#[must_use]
fn is_namespace(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

/// Whether `text` is a path relative to the cgroup mount: non-empty, not absolute, without NUL,
/// and with no empty component and no `.` or `..` component.
#[must_use]
fn is_parent(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('/')
        && !text.contains('\0')
        && text
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// The host facts that a record must match before reconciliation touches anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HostIdentity {
    /// The 32 digits of the machine identity.
    pub(super) machine: String,
    /// The 36 characters of the kernel's boot identity.
    pub(super) boot: String,
    /// The target of the process's cgroup namespace link.
    pub(super) cgroupns: String,
}

impl HostIdentity {
    /// Reads the machine identity from `machine_id`, the boot identity from `boot_id` and the
    /// cgroup namespace from the `ns/cgroup` link beneath `proc_self`.
    ///
    /// # Errors
    /// Returns the I/O error of a file or link that cannot be read, and `InvalidData` for a value
    /// outside its format.
    pub(super) fn read(proc_self: &Path, machine_id: &Path, boot_id: &Path) -> io::Result<Self> {
        let machine = fs::read_to_string(machine_id)?;
        let boot = fs::read_to_string(boot_id)?;
        let cgroupns = fs::read_link(proc_self.join("ns").join("cgroup"))?
            .into_os_string()
            .into_string()
            .map_err(|_| invalid_data("the cgroup namespace link is not UTF-8"))?;
        let host = Self {
            machine: machine.trim().to_owned(),
            boot: boot.trim().to_owned(),
            cgroupns,
        };
        if !is_lower_hex(&host.machine, 32) {
            return Err(invalid_data(
                "the machine identity is not 32 lowercase hexadecimal digits",
            ));
        }
        if !is_boot_id(&host.boot) {
            return Err(invalid_data("the boot identity is not a UUID"));
        }
        if !is_namespace(&host.cgroupns) {
            return Err(invalid_data(
                "the cgroup namespace is empty or has a control character",
            ));
        }
        Ok(host)
    }
}

/// An `InvalidData` error with a fixed message.
fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// One attempt's record: what the attempt made and where, as the registry holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Record {
    /// The attempt's identity, which names the record's file.
    pub(super) identity: Identity,
    /// The machine identity of the host that wrote the record.
    pub(super) machine: String,
    /// The boot identity of the host that wrote the record.
    pub(super) boot: String,
    /// The cgroup namespace of the supervisor that wrote the record.
    pub(super) cgroupns: String,
    /// The delegated group that holds the attempt group, relative to the cgroup mount.
    pub(super) parent: String,
    /// The attempt group's name, which must be the identity's group name.
    pub(super) group: String,
}

impl Record {
    /// The record's exact bytes: the tag line, then the six fields, one per line, in order.
    #[must_use]
    pub(super) fn render(&self) -> String {
        format!(
            "{RECORD_TAG}\nidentity={}\nmachine={}\nboot={}\ncgroupns={}\nparent={}\ngroup={}\n",
            self.identity.hex(),
            self.machine,
            self.boot,
            self.cgroupns,
            self.parent,
            self.group
        )
    }

    /// Parses a record strictly. Every line must be in its place, every value must have its
    /// format, and nothing may follow the last line.
    ///
    /// # Errors
    /// Returns the first `RecordRefusal` that applies, checking the size, the text, the layout,
    /// then each field in order.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, RecordRefusal> {
        if bytes.len() > RECORD_LIMIT {
            return Err(RecordRefusal::TooLong);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| RecordRefusal::NotText)?;
        let body = text.strip_suffix('\n').ok_or(RecordRefusal::Layout)?;
        let mut lines = body.split('\n');
        if lines.next() != Some(RECORD_TAG) {
            return Err(RecordRefusal::Layout);
        }
        let identity_text = field(&mut lines, "identity")?;
        let machine = field(&mut lines, "machine")?;
        let boot = field(&mut lines, "boot")?;
        let cgroupns = field(&mut lines, "cgroupns")?;
        let parent = field(&mut lines, "parent")?;
        let group = field(&mut lines, "group")?;
        if lines.next().is_some() {
            return Err(RecordRefusal::Layout);
        }
        let identity = Identity::parse_hex(identity_text).ok_or(RecordRefusal::IdentityField)?;
        if !is_lower_hex(machine, 32) {
            return Err(RecordRefusal::MachineField);
        }
        if !is_boot_id(boot) {
            return Err(RecordRefusal::BootField);
        }
        if !is_namespace(cgroupns) {
            return Err(RecordRefusal::NamespaceField);
        }
        if !is_parent(parent) {
            return Err(RecordRefusal::ParentField);
        }
        if group != identity.group_name() {
            return Err(RecordRefusal::ForeignGroup);
        }
        Ok(Self {
            identity,
            machine: machine.to_owned(),
            boot: boot.to_owned(),
            cgroupns: cgroupns.to_owned(),
            parent: parent.to_owned(),
            group: group.to_owned(),
        })
    }
}

/// The value of the next line, which must be `key=<value>`.
fn field<'a>(lines: &mut Split<'a, char>, key: &str) -> Result<&'a str, RecordRefusal> {
    lines
        .next()
        .and_then(|line| line.strip_prefix(key))
        .and_then(|rest| rest.strip_prefix('='))
        .ok_or(RecordRefusal::Layout)
}

/// Why a file is not a valid record (decision 0056, the attempt registry).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordRefusal {
    /// More than `RECORD_LIMIT` bytes.
    TooLong,
    /// Not UTF-8 text.
    NotText,
    /// Not the tag line and the six fields in order, each ending in a newline, with nothing
    /// after them: an unknown, missing, repeated or reordered line.
    Layout,
    /// The identity is not 32 lowercase hexadecimal digits.
    IdentityField,
    /// The machine identity is not 32 lowercase hexadecimal digits.
    MachineField,
    /// The boot identity is not a 36-character UUID.
    BootField,
    /// The cgroup namespace is empty or holds a control character.
    NamespaceField,
    /// The parent is empty, absolute, holds NUL, or has an empty, `.` or `..` component.
    ParentField,
    /// The group is not the name of the record's own identity.
    ForeignGroup,
}

/// What reconciliation does with a valid record, judged against the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Decision {
    /// The record was written on another machine, so its remote cleanup cannot be proved.
    OtherMachine,
    /// The record was written in an earlier boot of this machine.
    ChangedBoot,
    /// The record was written in this boot but in another cgroup namespace.
    OtherNamespace,
    /// The record was written in this boot and namespace, so its group may still exist.
    SameBoot,
}

/// Judges a valid record against the host, checking the machine first, then the boot and then
/// the cgroup namespace.
#[must_use]
pub(super) fn decide(record: &Record, host: &HostIdentity) -> Decision {
    if record.machine != host.machine {
        return Decision::OtherMachine;
    }
    if record.boot != host.boot {
        return Decision::ChangedBoot;
    }
    if record.cgroupns != host.cgroupns {
        return Decision::OtherNamespace;
    }
    Decision::SameBoot
}

/// Why the provider was quarantined; the persisted file records the word for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QuarantineReason {
    /// A cleanup of an attempt's group or input was not observed.
    Cleanup,
    /// The core routing changed during an attempt.
    Routing,
    /// A guardian was not reaped.
    Guardian,
    /// The registry itself refused: a bad record, another machine or namespace, or a failed write.
    Registry,
}

impl QuarantineReason {
    /// The word that the persisted quarantine file records for this reason.
    #[must_use]
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup",
            Self::Routing => "routing",
            Self::Guardian => "guardian",
            Self::Registry => "registry",
        }
    }
}

/// The attempts directory, held as a handle so that every later access is relative to it.
#[derive(Debug)]
pub(super) struct Registry {
    /// The attempts directory.
    dir: OwnedFd,
}

impl Registry {
    /// Opens the attempts directory of `execution_dir`, creating it with mode `0700` when it is
    /// missing.
    ///
    /// # Errors
    /// Returns the I/O error of a directory that cannot be opened or created, and
    /// `PermissionDenied` for an attempts directory that is not a directory of the provider's
    /// account, or that has a group or other permission.
    pub(super) fn open(execution_dir: &Path) -> io::Result<Self> {
        let execution = rustix::fs::open(execution_dir, OPEN_DIRECTORY, Mode::empty())?;
        match rustix::fs::mkdirat(&execution, ATTEMPTS_DIR, PRIVATE_DIRECTORY) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let dir = rustix::fs::openat(&execution, ATTEMPTS_DIR, OPEN_DIRECTORY, Mode::empty())?;
        let stat = rustix::fs::fstat(&dir)?;
        let private = FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            && stat.st_uid == rustix::process::geteuid().as_raw()
            && !Mode::from_raw_mode(stat.st_mode).intersects(Mode::RWXG | Mode::RWXO);
        if !private {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the attempts directory is not a private directory of this account",
            ));
        }
        Ok(Self { dir })
    }

    /// Writes `record` as `<identity>.record`. The record goes first to `.<identity>.tmp`, which is
    /// created exclusively with mode `0600`, written and synced. The rename then makes it the
    /// record, and the directory is synced. A record that would not parse back to itself is
    /// refused before anything is written. A temporary file that a failed step leaves behind is
    /// removed by the next reconciliation.
    ///
    /// # Errors
    /// Returns `InvalidData` for a record that would not parse back to itself, and the I/O error
    /// of any step that fails.
    pub(super) fn write(&self, record: &Record) -> io::Result<()> {
        let text = record.render();
        if Record::parse(text.as_bytes()).as_ref().ok() != Some(record) {
            return Err(invalid_data("the record does not parse back to itself"));
        }
        let temporary = format!(".{}{TEMPORARY_SUFFIX}", record.identity.hex());
        let mut file = File::from(rustix::fs::openat(
            &self.dir,
            temporary.as_str(),
            CREATE_FILE,
            PRIVATE_FILE,
        )?);
        let staged = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = staged {
            // The temporary file was created exclusively by this call. If this removal fails, the
            // file stays, and the next reconciliation removes it.
            rustix::fs::unlinkat(&self.dir, temporary.as_str(), AtFlags::empty()).ok();
            return Err(error);
        }
        rustix::fs::renameat(
            &self.dir,
            temporary.as_str(),
            &self.dir,
            record_name(record.identity).as_str(),
        )?;
        rustix::fs::fsync(&self.dir)?;
        Ok(())
    }

    /// Whether no record of `identity` is in the registry. Any answer but "no such file" counts as
    /// present, so a caller that settles on `true` never settles a record that may still exist.
    #[cfg(feature = "enforcement-fixture")]
    #[must_use]
    pub(super) fn record_absent(&self, identity: Identity) -> bool {
        matches!(
            rustix::fs::statat(
                &self.dir,
                record_name(identity).as_str(),
                AtFlags::SYMLINK_NOFOLLOW
            ),
            Err(Errno::NOENT)
        )
    }

    /// Removes the record of `identity`, then syncs the directory.
    ///
    /// # Errors
    /// Returns the I/O error of the removal or of the sync.
    pub(super) fn remove(&self, identity: Identity) -> io::Result<()> {
        rustix::fs::unlinkat(&self.dir, record_name(identity).as_str(), AtFlags::empty())?;
        rustix::fs::fsync(&self.dir)?;
        Ok(())
    }

    /// Lists the attempts directory, sorted by name, and classifies each entry.
    ///
    /// # Errors
    /// Returns the I/O error of the listing, or of a classification that fails for a reason other
    /// than a link.
    pub(super) fn entries(&self) -> io::Result<Vec<Entry>> {
        // A fresh handle, since a duplicate would share the listing position of this one.
        let listing = rustix::fs::openat(&self.dir, ".", OPEN_DIRECTORY, Mode::empty())?;
        let mut sorted = BTreeMap::new();
        for entry in Dir::new(listing)? {
            let entry = entry?;
            let raw = entry.file_name();
            if raw == c"." || raw == c".." {
                continue;
            }
            let name = raw.to_string_lossy().into_owned();
            let classified = match raw.to_str() {
                Ok(text) => self.classify(text)?,
                Err(_) => Entry::Other { name: name.clone() },
            };
            sorted.insert(name, classified);
        }
        Ok(sorted.into_values().collect())
    }

    /// Classifies the entry `name` by its name and its kind, without following a link.
    ///
    /// # Errors
    /// Returns the I/O error of a status or an open that fails for a reason other than a link.
    fn classify(&self, name: &str) -> io::Result<Entry> {
        if let Some(identity) = name
            .strip_suffix(RECORD_SUFFIX)
            .and_then(Identity::parse_hex)
        {
            return self.read_record(name, identity);
        }
        let is_temporary = name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(TEMPORARY_SUFFIX))
            .and_then(Identity::parse_hex)
            .is_some();
        if is_temporary {
            let stat = rustix::fs::statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile {
                return Ok(Entry::Temp {
                    name: name.to_owned(),
                });
            }
        }
        Ok(Entry::Other {
            name: name.to_owned(),
        })
    }

    /// Reads the record `name` of `identity`, which must be a regular file. A link, and any other
    /// kind of file, is `Other`.
    ///
    /// # Errors
    /// Returns the I/O error of an open, a status or a read, except that a link is not an error.
    fn read_record(&self, name: &str, identity: Identity) -> io::Result<Entry> {
        let opened = match rustix::fs::openat(&self.dir, name, OPEN_RECORD, Mode::empty()) {
            Ok(opened) => opened,
            Err(Errno::LOOP) => {
                return Ok(Entry::Other {
                    name: name.to_owned(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        if FileType::from_raw_mode(rustix::fs::fstat(&opened)?.st_mode) != FileType::RegularFile {
            return Ok(Entry::Other {
                name: name.to_owned(),
            });
        }
        let mut bytes = Vec::new();
        File::from(opened)
            .take(RECORD_READ_BYTES)
            .read_to_end(&mut bytes)?;
        Ok(Entry::Record { identity, bytes })
    }
}

/// One entry of the attempts directory, as a listing classifies it.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Entry {
    /// A regular file named `<identity>.record`, with its bytes up to `RECORD_READ_BYTES`.
    Record {
        /// The identity that the file name gives.
        identity: Identity,
        /// The file's bytes.
        bytes: Vec<u8>,
    },
    /// A regular file named `.<identity>.tmp`: a record whose rename never happened.
    Temp {
        /// The file name.
        name: String,
    },
    /// Any other entry: a file with another name, a link, or a file that is not regular.
    Other {
        /// The file name, with bytes that are not UTF-8 replaced.
        name: String,
    },
}

/// The file name of the record of `identity`.
#[must_use]
fn record_name(identity: Identity) -> String {
    format!("{}{RECORD_SUFFIX}", identity.hex())
}

/// Persists the quarantine of `execution_dir` for `reason`: creates the file `quarantine`
/// exclusively with mode `0600`, holding the tag line and the reason, then syncs the file and the
/// directory. A file that exists already is left as it is.
///
/// A write or a sync that fails leaves whatever reached the disk: the launch looks only for the
/// file's presence, so a partial file still refuses a restart, which fails closed. The caller still
/// reports that the quarantine may not be persisted.
///
/// # Errors
/// Returns the I/O error of a step that fails.
pub(super) fn persist_quarantine(execution_dir: &Path, reason: QuarantineReason) -> io::Result<()> {
    let execution = rustix::fs::open(execution_dir, OPEN_DIRECTORY, Mode::empty())?;
    let created = match rustix::fs::openat(&execution, QUARANTINE_FILE, CREATE_FILE, PRIVATE_FILE) {
        Ok(created) => created,
        Err(Errno::EXIST) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let text = format!("{QUARANTINE_TAG}\nreason={}\n", reason.word());
    let mut file = File::from(created);
    let staged = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| rustix::fs::fsync(&execution).map_err(io::Error::from));
    drop(file);
    staged
}

/// Whether the execution directory holds a quarantine file, whatever the file holds. A lookup that
/// fails for any reason other than an absent file counts as present, so that the launch fails
/// closed.
#[must_use]
pub(super) fn quarantine_present(execution_dir: &Path) -> bool {
    match fs::symlink_metadata(execution_dir.join(QUARANTINE_FILE)) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

/// What reconciliation did with one record that it settled and removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReconcileOutcome {
    /// The record's parent or group did not exist, or was removed before its kill, so no group was
    /// left to end.
    Gone,
    /// The group was killed, seen empty and removed.
    Killed,
    /// The record was written in an earlier boot, so no group was touched.
    ChangedBoot,
}

/// One record that reconciliation settled and removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Reconciled {
    /// The identity of the record.
    pub(super) identity: Identity,
    /// What was done for the record.
    pub(super) outcome: ReconcileOutcome,
}

/// Why reconciliation refused, so that the provider is quarantined (decision 0042 rule 6). A
/// refusal leaves the record or entry that caused it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RegistryRefusal {
    /// The attempts directory could not be opened, checked or listed, or a temporary file could not
    /// be removed.
    Unusable,
    /// An entry is not a regular file named like a record or a temporary file, or a record's
    /// identity differs from its file name.
    Entry,
    /// A record is not valid.
    Record(RecordRefusal),
    /// The record was written on another machine, so its remote cleanup cannot be proved.
    OtherMachine,
    /// The record was written in this boot but in another cgroup namespace.
    OtherNamespace,
    /// The record's parent is not owned by the provider's account.
    Owner,
    /// A parent or group could not be opened, killed, observed or removed.
    Cleanup,
    /// The staged input could not be removed.
    Input,
    /// The record could not be removed after its group was ended.
    RecordNotRemoved,
}

/// Reconciles the registry before an attempt is admitted. Each record is taken in name order: a
/// temporary file is removed; a record from another machine or cgroup namespace is refused; a
/// record from an earlier boot is removed without touching its group; a record from this boot and
/// namespace has its group ended, then the staged input and the record are removed.
///
/// `own_uid` is the provider's account, and `host` is the host's identity as the caller read it.
///
/// # Errors
/// Returns the first `RegistryRefusal` that applies. The entries after it are left as they are.
pub(super) fn reconcile(
    execution_dir: &Path,
    cgroup_mount: &Path,
    own_uid: u32,
    host: &HostIdentity,
) -> Result<Vec<Reconciled>, RegistryRefusal> {
    let registry = Registry::open(execution_dir).map_err(|_| RegistryRefusal::Unusable)?;
    // One handle on the mount, opened right after admission checked it, serves every record.
    let mount = rustix::fs::open(cgroup_mount, CGROUP_DIRECTORY, Mode::empty())
        .map_err(|_| RegistryRefusal::Cleanup)?;
    let mut reconciled = Vec::new();
    for entry in registry.entries().map_err(|_| RegistryRefusal::Unusable)? {
        let (identity, bytes) = match entry {
            Entry::Temp { name } => {
                rustix::fs::unlinkat(&registry.dir, name.as_str(), AtFlags::empty())
                    .map_err(|_| RegistryRefusal::Unusable)?;
                continue;
            }
            Entry::Other { .. } => return Err(RegistryRefusal::Entry),
            Entry::Record { identity, bytes } => (identity, bytes),
        };
        let record = Record::parse(&bytes).map_err(RegistryRefusal::Record)?;
        if record.identity != identity {
            return Err(RegistryRefusal::Entry);
        }
        let outcome = match decide(&record, host) {
            Decision::OtherMachine => return Err(RegistryRefusal::OtherMachine),
            Decision::OtherNamespace => return Err(RegistryRefusal::OtherNamespace),
            Decision::ChangedBoot => ReconcileOutcome::ChangedBoot,
            Decision::SameBoot => end_group(&mount, &record, own_uid)?,
        };
        remove_tree(&execution_dir.join(INPUT_DIR)).map_err(|_| RegistryRefusal::Input)?;
        registry
            .remove(identity)
            .map_err(|_| RegistryRefusal::RecordNotRemoved)?;
        reconciled.push(Reconciled { identity, outcome });
    }
    Ok(reconciled)
}

/// Ends the group of a record from this boot and namespace. A parent or a group that does not
/// exist means that the group is gone. The parent must belong to `own_uid`; the group is then
/// killed, seen empty and removed.
///
/// # Errors
/// Returns `Owner` for a parent of another account, and `Cleanup` for any other failure.
fn end_group(
    mount: &OwnedFd,
    record: &Record,
    own_uid: u32,
) -> Result<ReconcileOutcome, RegistryRefusal> {
    let parent = match rustix::fs::openat2(
        mount,
        record.parent.as_str(),
        CGROUP_DIRECTORY,
        Mode::empty(),
        RESOLVE,
    ) {
        Ok(parent) => parent,
        Err(Errno::NOENT) => return Ok(ReconcileOutcome::Gone),
        Err(_) => return Err(RegistryRefusal::Cleanup),
    };
    let owner = rustix::fs::fstat(&parent)
        .map_err(|_| RegistryRefusal::Cleanup)?
        .st_uid;
    if owner != own_uid {
        return Err(RegistryRefusal::Owner);
    }
    let group = match rustix::fs::openat2(
        &parent,
        record.group.as_str(),
        CGROUP_DIRECTORY,
        Mode::empty(),
        RESOLVE,
    ) {
        Ok(group) => group,
        Err(Errno::NOENT) => return Ok(ReconcileOutcome::Gone),
        Err(_) => return Err(RegistryRefusal::Cleanup),
    };
    let outcome = kill_and_await_empty(&parent, record.group.as_str(), &group)?;
    drop(group);
    match rustix::fs::unlinkat(&parent, record.group.as_str(), AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(outcome),
        Err(_) => Err(RegistryRefusal::Cleanup),
    }
}

/// Kills the processes of `group` through `cgroup.kill`, then reads `cgroup.events` every `POLL`
/// until it reports `populated 0`, for at most `probe_group::CLEANUP_DEADLINE`. An events file that
/// is gone means that the group holds no process.
///
/// A `cgroup.kill` that cannot be opened or written because the group was removed after its lookup
/// is settled by [`settle_vanished`]: a removed group has no process left to kill.
///
/// Returns `Killed` once the kill was written, and `Gone` when the group was already removed.
///
/// # Errors
/// Returns `Cleanup` when the kill fails for another reason, when the events file cannot be read or
/// does not parse, when the group is still populated at the deadline, or when a group that is still
/// there has no usable `cgroup.kill`.
fn kill_and_await_empty(
    parent: &OwnedFd,
    name: &str,
    group: &OwnedFd,
) -> Result<ReconcileOutcome, RegistryRefusal> {
    let kill = match rustix::fs::openat(
        group,
        "cgroup.kill",
        OFlags::WRONLY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(kill) => kill,
        Err(error) if group_vanished(error) => return settle_vanished(parent, name),
        Err(_) => return Err(RegistryRefusal::Cleanup),
    };
    if let Err(error) = File::from(kill).write_all(b"1") {
        return match Errno::from_io_error(&error) {
            Some(errno) if group_vanished(errno) => settle_vanished(parent, name),
            _ => Err(RegistryRefusal::Cleanup),
        };
    }
    let deadline = Instant::now()
        .checked_add(probe_group::CLEANUP_DEADLINE)
        .ok_or(RegistryRefusal::Cleanup)?;
    loop {
        let events = match rustix::fs::openat(
            group,
            "cgroup.events",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(events) => events,
            Err(error) if group_vanished(error) => return Ok(ReconcileOutcome::Killed),
            Err(_) => return Err(RegistryRefusal::Cleanup),
        };
        let mut bytes = Vec::new();
        // The group can be removed between the open and the read, which then fails with `ENODEV`:
        // a removed group holds no process.
        if let Err(error) = File::from(events)
            .take(EVENTS_BYTES)
            .read_to_end(&mut bytes)
        {
            return match Errno::from_io_error(&error) {
                Some(errno) if group_vanished(errno) => Ok(ReconcileOutcome::Killed),
                _ => Err(RegistryRefusal::Cleanup),
            };
        }
        match probe_group::parse_populated(&bytes) {
            Some(false) => return Ok(ReconcileOutcome::Killed),
            Some(true) => {}
            None => return Err(RegistryRefusal::Cleanup),
        }
        if Instant::now() >= deadline {
            return Err(RegistryRefusal::Cleanup);
        }
        thread::sleep(POLL);
    }
}

/// Whether `error`, from a cgroup file, means that the file's group was removed: `ENOENT` from a
/// removed directory or a dead group, and `ENODEV` from a file whose node was removed.
fn group_vanished(error: Errno) -> bool {
    matches!(error, Errno::NOENT | Errno::NODEV)
}

/// Settles a group whose `cgroup.kill` failed in a way that a removal explains. A fresh lookup of
/// `name` beneath `parent` that finds nothing means that the group is `Gone`. A group that is still
/// there has no usable `cgroup.kill`, so it cannot be ended here and is refused.
///
/// # Errors
/// Returns `Cleanup` when the group is still there, and when the lookup fails for another reason.
fn settle_vanished(parent: &OwnedFd, name: &str) -> Result<ReconcileOutcome, RegistryRefusal> {
    match rustix::fs::openat2(parent, name, CGROUP_DIRECTORY, Mode::empty(), RESOLVE) {
        Err(Errno::NOENT) => Ok(ReconcileOutcome::Gone),
        _ => Err(RegistryRefusal::Cleanup),
    }
}

#[cfg(test)]
mod tests;
