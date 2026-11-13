//! Unit tests of the probe table, the bytes of its programs, the writer and the table's sync with
//! `docs/linux-syscall-mapping.md` (decision 0056, amendment of 2026-11-13, "the syscall mapping").
//! No test here starts a probe: a generated program runs only in the enforcement job.

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process;

#[cfg(target_arch = "x86_64")]
use super::super::policy::{ALLOWED, CLONE, CLONE3, NAMESPACE_FLAGS, QUERY_ONLY};
use super::{
    AUDIT_ARCH_I386, AUDIT_ARCH_X86_64, Entry, Expect, PROBES, Probe, code, program,
    write_probe_programs,
};

type TestResult = Result<(), Box<dyn Error>>;

/// The mapping classes, which are the section headings of `docs/linux-syscall-mapping.md`, in the
/// order that the probe table and the document both use.
const CLASSES: [&str; 13] = [
    "controls",
    "default",
    "namespaces",
    "mount",
    "privilege",
    "tracing",
    "kernel",
    "host-global",
    "keyrings",
    "async",
    "limits",
    "network",
    "abi",
];

/// `mov edi, 0x400094; mov esi, 64; mov eax, 435; syscall; exit_group(-eax)`: the `clone3` probe's
/// code before its structure. The structure's address is the entry plus these 28 bytes.
const CLONE3_HEAD: [u8; 28] = [
    0xBF, 0x94, 0x00, 0x40, 0x00, // mov edi, 0x400094
    0xBE, 0x40, 0x00, 0x00, 0x00, // mov esi, 64
    0xB8, 0xB3, 0x01, 0x00, 0x00, // mov eax, 435
    0x0F, 0x05, // syscall
    0x89, 0xC7, 0xF7, 0xDF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F, 0x05, // exit_group(-eax)
];

/// `clone(SIGCHLD)`: five argument loads, `mov eax, 56` and `syscall`. The fork's head is 34 bytes.
const FORK_HEAD: [u8; 34] = [
    0xBF, 0x11, 0x00, 0x00, 0x00, // mov edi, 17 (SIGCHLD)
    0xBE, 0x00, 0x00, 0x00, 0x00, // mov esi, 0
    0xBA, 0x00, 0x00, 0x00, 0x00, // mov edx, 0
    0x41, 0xBA, 0x00, 0x00, 0x00, 0x00, // mov r10d, 0
    0x41, 0xB8, 0x00, 0x00, 0x00, 0x00, // mov r8d, 0
    0xB8, 0x38, 0x00, 0x00, 0x00, // mov eax, 56
    0x0F, 0x05, // syscall
];

/// `test eax, eax; jnz +18`: the jump skips the child part, which is 18 bytes long.
const FORK_JUMP: [u8; 4] = [0x85, 0xC0, 0x75, 0x12];

/// The child part of `ok_fork_child_gettid`: `gettid`, then the exit with its result.
const FORK_CHILD: [u8; 18] = [
    0xB8, 0xBA, 0x00, 0x00, 0x00, // mov eax, 186
    0x0F, 0x05, // syscall
    0x89, 0xC7, 0xF7, 0xDF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F, 0x05, // exit_group(-eax)
];

/// The parent part of `ok_fork_child_gettid`: `wait4(-1)`, then `exit_group(0)`.
const FORK_PARENT: [u8; 37] = [
    0xBF, 0xFF, 0xFF, 0xFF, 0xFF, // mov edi, -1
    0xBE, 0x00, 0x00, 0x00, 0x00, // mov esi, 0
    0xBA, 0x00, 0x00, 0x00, 0x00, // mov edx, 0
    0x41, 0xBA, 0x00, 0x00, 0x00, 0x00, // mov r10d, 0
    0xB8, 0x3D, 0x00, 0x00, 0x00, // mov eax, 61
    0x0F, 0x05, // syscall
    0x31, 0xFF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F, 0x05, // xor edi, edi; exit_group(0)
];

/// The probe of the given name, or an error when the table has no such probe.
fn probe_named(name: &str) -> Result<Probe, Box<dyn Error>> {
    PROBES
        .iter()
        .copied()
        .find(|probe| probe.name == name)
        .ok_or_else(|| format!("no probe named {name}").into())
}

/// The `N` bytes at `offset`, or an error when the file is shorter than that.
fn bytes_at<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], Box<dyn Error>> {
    let window = bytes
        .get(offset..)
        .and_then(|rest| rest.get(..N))
        .ok_or("the program is shorter than its headers")?;
    Ok(<[u8; N]>::try_from(window)?)
}

/// The little-endian `u16` at `offset`.
fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Box<dyn Error>> {
    Ok(u16::from_le_bytes(bytes_at(bytes, offset)?))
}

/// The little-endian `u32` at `offset`.
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn Error>> {
    Ok(u32::from_le_bytes(bytes_at(bytes, offset)?))
}

/// The little-endian `u64` at `offset`.
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, Box<dyn Error>> {
    Ok(u64::from_le_bytes(bytes_at(bytes, offset)?))
}

/// Strips `prefix` from the front of `bytes`, or names the part whose bytes differ.
fn expect_prefix<'a>(
    bytes: &'a [u8],
    prefix: &[u8],
    part: &str,
) -> Result<&'a [u8], Box<dyn Error>> {
    bytes
        .strip_prefix(prefix)
        .ok_or_else(|| format!("{part} differs: expected {prefix:02X?} at this point").into())
}

/// Whether a probe name uses only the characters the file names may have.
fn is_plain_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
}

/// The probe name in the first cell of a table row written as `| `name` |`, or `None` for any
/// other line.
fn table_row_name(line: &str) -> Option<&str> {
    let cells: Vec<&str> = line
        .trim_start()
        .strip_prefix('|')?
        .split('|')
        .map(str::trim)
        .collect();
    let first = *cells.first()?;
    first.strip_prefix('`')?.strip_suffix('`')
}

/// The number of the `socket` system call on x86-64.
#[cfg(target_arch = "x86_64")]
const SOCKET_NUMBER: u32 = 41;

/// The number of the `setsockopt` system call on x86-64.
#[cfg(target_arch = "x86_64")]
const SETSOCKOPT_NUMBER: u32 = 54;

/// The mask of the socket type. The close-on-exec and non-blocking flags lie outside it.
#[cfg(target_arch = "x86_64")]
const SOCK_TYPE_MASK: u32 = 0xF;

/// The socket level, which `setsockopt` takes as its second argument.
#[cfg(target_arch = "x86_64")]
const SOL_SOCKET: u32 = 1;

/// `SO_REUSEADDR`, which std's `TcpListener::bind` sets at the socket level.
#[cfg(target_arch = "x86_64")]
const SO_REUSEADDR: u32 = 2;

/// The socket forms that the filter allows, as (domain, type, protocol) with the type already
/// masked: `AF_UNIX` (1), `AF_INET` (2) and `AF_INET6` (10), each with `SOCK_STREAM` (1) or
/// `SOCK_DGRAM` (2). The protocol is zero, except TCP (6) for streams and UDP (17) for datagrams.
#[cfg(target_arch = "x86_64")]
const SOCKET_FORMS: [(u32, u32, u32); 10] = [
    (1, 1, 0),
    (1, 2, 0),
    (2, 1, 0),
    (2, 1, 6),
    (2, 2, 0),
    (2, 2, 17),
    (10, 1, 0),
    (10, 1, 6),
    (10, 2, 0),
    (10, 2, 17),
];

/// Whether the call is a `socket` of one of the forms in `SOCKET_FORMS`: its domain and protocol
/// must match exactly, and its type must match once `SOCK_TYPE_MASK` leaves out the flags.
#[cfg(target_arch = "x86_64")]
fn is_socket_form(probe: &Probe) -> bool {
    if probe.number != SOCKET_NUMBER {
        return false;
    }
    let form = match (
        probe.arguments.first(),
        probe.arguments.get(1),
        probe.arguments.get(2),
    ) {
        (Some(domain), Some(kind), Some(protocol)) => (*domain, *kind & SOCK_TYPE_MASK, *protocol),
        _ => return false,
    };
    SOCKET_FORMS.contains(&form)
}

/// Whether the call is a `setsockopt` of `SO_REUSEADDR` at the socket level.
#[cfg(target_arch = "x86_64")]
fn is_reuse_option(probe: &Probe) -> bool {
    probe.number == SETSOCKOPT_NUMBER
        && probe.arguments.get(1) == Some(&SOL_SOCKET)
        && probe.arguments.get(2) == Some(&SO_REUSEADDR)
}

/// Whether the allowlist allows the call of `probe` with its arguments: its number is listed, or
/// it is `clone` without namespace flags, or it is `prlimit64` as a query (third argument zero),
/// or it is a `socket` of one of the forms in `SOCKET_FORMS`, or it is a `setsockopt` of
/// `SO_REUSEADDR` at the socket level.
#[cfg(target_arch = "x86_64")]
fn allowlist_allows(probe: &Probe) -> bool {
    let number = i64::from(probe.number);
    let listed = ALLOWED.iter().any(|(_, allowed)| *allowed == number);
    let plain_clone = number == CLONE.1
        && probe
            .arguments
            .first()
            .is_some_and(|flags| u64::from(*flags) & NAMESPACE_FLAGS == 0);
    let query = number == QUERY_ONLY.1 && probe.arguments.get(2) == Some(&0);
    listed || plain_clone || query || is_socket_form(probe) || is_reuse_option(probe)
}

/// Whether the allowlist refuses the call of `probe` with its arguments: its number is not listed
/// and is not one whose allowance depends on its arguments, or it is `clone` with a namespace
/// flag, or it is `prlimit64` as a setter (third argument non-zero), or it is a `socket` of a form
/// outside `SOCKET_FORMS`, or it is a `setsockopt` other than `SO_REUSEADDR` at the socket level.
#[cfg(target_arch = "x86_64")]
fn allowlist_refuses(probe: &Probe) -> bool {
    let number = i64::from(probe.number);
    let listed = ALLOWED.iter().any(|(_, allowed)| *allowed == number);
    let conditional = [CLONE.1, QUERY_ONLY.1].contains(&number)
        || probe.number == SOCKET_NUMBER
        || probe.number == SETSOCKOPT_NUMBER;
    let unlisted = !listed && !conditional;
    let namespace_clone = number == CLONE.1
        && probe
            .arguments
            .first()
            .is_some_and(|flags| u64::from(*flags) & NAMESPACE_FLAGS != 0);
    let setter = number == QUERY_ONLY.1 && probe.arguments.get(2).is_some_and(|value| *value != 0);
    let other_socket_form = probe.number == SOCKET_NUMBER && !is_socket_form(probe);
    let other_option = probe.number == SETSOCKOPT_NUMBER && !is_reuse_option(probe);
    unlisted || namespace_clone || setter || other_socket_form || other_option
}

/// Writes the probes into `directory` and checks the result: one regular file per probe with
/// mode `0755`, nothing else, and the bytes of each program.
fn check_written_directory(directory: &Path) -> TestResult {
    write_probe_programs(directory)?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            entry.file_type()?.is_file(),
            "{name} must be a regular file"
        );
        let mode = entry.metadata()?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "{name} must have mode 0755");
        names.insert(name);
    }
    let expected: BTreeSet<String> = PROBES.iter().map(|probe| probe.name.to_owned()).collect();
    assert_eq!(
        names, expected,
        "the directory must hold one file per probe and nothing else"
    );
    for probe in &PROBES {
        let written = fs::read(directory.join(probe.name))?;
        assert_eq!(
            written,
            program(probe)?,
            "{}: the file must hold the probe's program",
            probe.name
        );
    }
    Ok(())
}

#[test]
fn probe_names_are_unique_and_plain() {
    let mut names = BTreeSet::new();
    for probe in &PROBES {
        assert!(
            !probe.name.is_empty() && probe.name.bytes().all(is_plain_byte),
            "a probe name is ASCII [a-z0-9_], got {:?}",
            probe.name
        );
        assert!(
            names.insert(probe.name),
            "two probes are named {:?}",
            probe.name
        );
    }
}

#[test]
fn every_probe_has_a_known_class_and_at_most_six_arguments() {
    for probe in &PROBES {
        assert!(
            CLASSES.contains(&probe.class),
            "{} has the unknown class {:?}",
            probe.name,
            probe.class
        );
        assert!(
            probe.arguments.len() <= 6,
            "{} passes {} arguments; a system call takes at most six",
            probe.name,
            probe.arguments.len()
        );
    }
}

/// Each class of `PROBES` is one contiguous block, and the blocks come in `CLASSES` order, so the
/// `network` block sits between `limits` and `abi`, and `abi` stays last. The block names, with
/// repeats of the same class in a row removed, must equal `CLASSES`.
#[test]
fn classes_appear_in_order_and_abi_is_last() {
    let mut blocks: Vec<&str> = Vec::new();
    for probe in &PROBES {
        if blocks.last() != Some(&probe.class) {
            blocks.push(probe.class);
        }
    }
    assert_eq!(
        blocks, CLASSES,
        "each class must be one contiguous block, in CLASSES order with abi last"
    );
}

#[test]
fn expectations_match_the_entry_and_number() {
    for probe in &PROBES {
        if let Expect::Refused { arch, number } = probe.expect {
            let entry_arch = match probe.entry {
                Entry::Native => AUDIT_ARCH_X86_64,
                Entry::I386 => AUDIT_ARCH_I386,
            };
            assert_eq!(
                arch, entry_arch,
                "{}: the record must carry the audit architecture of its entry",
                probe.name
            );
            assert_eq!(
                number, probe.number,
                "{}: the record must carry the probe's number",
                probe.name
            );
        }
        assert_eq!(
            probe.class == "controls",
            probe.expect == Expect::Allowed,
            "{}: class controls and Expect::Allowed must agree",
            probe.name
        );
    }
    let enosys: Vec<&str> = PROBES
        .iter()
        .filter(|probe| probe.expect == Expect::Enosys)
        .map(|probe| probe.name)
        .collect();
    assert_eq!(
        enosys,
        ["ns_clone3"],
        "only the clone3 probe expects ENOSYS"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn probes_agree_with_the_policy_entries() {
    for probe in &PROBES {
        match (probe.entry, probe.expect) {
            (_, Expect::Allowed) => assert!(
                allowlist_allows(probe),
                "{}: the allowlist must allow number {} with these arguments",
                probe.name,
                probe.number
            ),
            (Entry::Native, Expect::Refused { .. }) => assert!(
                allowlist_refuses(probe),
                "{}: the allowlist must refuse number {} with these arguments",
                probe.name,
                probe.number
            ),
            // An i386 call is refused by the architecture check, before its number is read.
            (Entry::I386, Expect::Refused { .. }) => {}
            (_, Expect::Enosys) => assert_eq!(
                i64::from(probe.number),
                CLONE3.1,
                "{}: the ENOSYS probe must be clone3",
                probe.name
            ),
        }
    }
}

#[test]
fn elf_header_fields_parse_from_the_generated_bytes() -> TestResult {
    for probe in &PROBES {
        let file = program(probe)?;
        let body = code(probe)?;
        let name = probe.name;
        assert_eq!(bytes_at::<4>(&file, 0)?, *b"\x7FELF", "{name}: the magic");
        assert_eq!(bytes_at::<1>(&file, 4)?, [2], "{name}: ELFCLASS64");
        assert_eq!(bytes_at::<1>(&file, 5)?, [1], "{name}: little endian");
        assert_eq!(u16_at(&file, 16)?, 2, "{name}: e_type is ET_EXEC");
        assert_eq!(u16_at(&file, 18)?, 0x3E, "{name}: e_machine is x86-64");
        assert_eq!(
            u64_at(&file, 24)?,
            0x40_0078,
            "{name}: e_entry is the first byte after the two headers"
        );
        assert_eq!(
            u64_at(&file, 32)?,
            64,
            "{name}: e_phoff follows the file header"
        );
        assert_eq!(u16_at(&file, 54)?, 56, "{name}: e_phentsize");
        assert_eq!(u16_at(&file, 56)?, 1, "{name}: exactly one program header");
        assert_eq!(u32_at(&file, 64)?, 1, "{name}: p_type is PT_LOAD");
        assert_eq!(u32_at(&file, 68)?, 5, "{name}: p_flags is PF_R | PF_X");
        assert_eq!(u64_at(&file, 80)?, 0x40_0000, "{name}: p_vaddr");
        let length = u64::try_from(file.len())?;
        assert_eq!(
            u64_at(&file, 96)?,
            length,
            "{name}: p_filesz covers the file"
        );
        assert_eq!(
            u64_at(&file, 104)?,
            length,
            "{name}: p_memsz covers the file"
        );
        assert_eq!(
            file.get(0x78..),
            Some(body.as_slice()),
            "{name}: the code starts at offset 0x78"
        );
    }
    Ok(())
}

#[test]
fn single_probe_code_is_exact() -> TestResult {
    assert_eq!(
        code(&probe_named("ns_unshare")?)?,
        [
            0xBF, 0x00, 0x00, 0x02, 0x00, // mov edi, 0x20000 (CLONE_NEWNS)
            0xB8, 0x10, 0x01, 0x00, 0x00, // mov eax, 272
            0x0F, 0x05, // syscall
            0x89, 0xC7, 0xF7, 0xDF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F,
            0x05, // exit_group(-eax)
        ],
        "unshare(CLONE_NEWNS), then the exit with its result"
    );
    assert_eq!(
        code(&probe_named("abi_i386_exit")?)?,
        [
            0xBB, 0x00, 0x00, 0x00, 0x00, // mov ebx, 0
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
            0xCD, 0x80, // int 0x80
            0x89, 0xC7, 0xF7, 0xDF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F,
            0x05, // exit_group(-eax)
        ],
        "the i386 exit through int 0x80, then the exit with its result"
    );
    Ok(())
}

#[test]
fn forked_child_jump_skips_exactly_the_child_part() -> TestResult {
    let fork = probe_named("ok_fork_child_gettid")?;
    let body = code(&fork)?;
    let rest = expect_prefix(&body, &FORK_HEAD, "the clone(SIGCHLD) head")?;
    let rest = expect_prefix(
        rest,
        &FORK_JUMP,
        "test eax, eax, then jnz past the 18-byte child part",
    )?;
    let rest = expect_prefix(rest, &FORK_CHILD, "the child part")?;
    assert_eq!(
        rest,
        FORK_PARENT.as_slice(),
        "the parent part must be wait4(-1), then exit_group(0)"
    );
    Ok(())
}

#[test]
fn clone3_structure_follows_the_code_and_is_invalid() -> TestResult {
    let clone3 = probe_named("ns_clone3")?;
    let body = code(&clone3)?;
    assert_eq!(
        body.get(..28),
        Some(CLONE3_HEAD.as_slice()),
        "the clone3 call must point at the structure 28 bytes past the entry"
    );
    let structure = body
        .get(28..)
        .ok_or("the code ends before the clone_args structure")?;
    let exit_signal = u64_at(structure, 32)?;
    assert!(
        exit_signal > 64,
        "clone_args.exit_signal must not be a signal (above 64), got {exit_signal}: a valid one lets the kernel create the process"
    );
    let expected: Vec<u8> = [0x1000_0000_u64, 0, 0, 0, 0x100, 0, 0, 0]
        .iter()
        .flat_map(|field| field.to_le_bytes())
        .collect();
    assert_eq!(
        structure,
        expected.as_slice(),
        "clone_args: CLONE_NEWUSER, the zero fields, the invalid exit signal, then the zero stack fields"
    );
    Ok(())
}

#[test]
fn write_probe_programs_writes_one_executable_file_per_probe() -> TestResult {
    let root = env::temp_dir().join(format!("artifree-probe-programs-{}", process::id()));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    // The probes directory does not exist yet, so the writer must create it.
    let outcome = check_written_directory(&root.join("probes"));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    outcome
}

#[test]
fn mapping_document_lists_exactly_the_probe_table() -> TestResult {
    let document = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/linux-syscall-mapping.md"),
    )?;
    let mut rows: Vec<&str> = document.lines().filter_map(table_row_name).collect();
    rows.sort_unstable();
    let mut names: Vec<&str> = PROBES.iter().map(|probe| probe.name).collect();
    names.sort_unstable();
    assert_eq!(
        rows, names,
        "the first cell of each table row must name each probe exactly once"
    );
    let headings: BTreeSet<&str> = document
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .collect();
    for probe in &PROBES {
        assert!(
            headings.contains(probe.class),
            "docs/linux-syscall-mapping.md has no `## {}` section",
            probe.class
        );
    }
    Ok(())
}
