//! Behavioral tests of the tool-root manifest (decisions 0042 rule 3 and 0056 rule 6). They cover
//! acceptance of a matching root, each content, structure and bound refusal, and the admission
//! order that checks the manifest after the named launcher and fixture and before delegation.
//! Each test works in a temporary tree and starts no process.

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use super::super::admission::{AdmissionRoots, admit_with};
use super::super::{AdmissionRefusal, InspectionProfile, launcher};
use super::{MANIFEST_FILE, verify};

/// The SHA-256 of `abc`, computed with `sha256sum` outside the code under test.
const DIGEST_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
/// The SHA-256 of the empty file, computed with `sha256sum` outside the code under test.
const DIGEST_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// The SHA-256 of `z`, computed with `sha256sum` outside the code under test.
const DIGEST_Z: &str = "594e519ae499312b29433b7dd8a97ff068defcba9755b6d5d00e84c524d67b06";
/// The SHA-256 of the exact bytes of `base_manifest`, computed with `sha256sum`.
const BASE_MANIFEST_SHA256: &str =
    "96f16f6bcf6ff8b90e2f61fd62f58fb2151f064134910a60d4c5782e0a246658";

/// Numbers the scratch directories of this process, so that no two tests share a name.
static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The baseline tree: three regular files, listed in byte order by `base_manifest`.
const BASE_FILES: &[(&str, &[u8])] = &[("bin/a", b"abc"), ("bin/b", b""), ("lib/x/y", b"z")];

/// A change to a tool root, applied after the root is built.
type Edit = fn(&Path) -> io::Result<()>;

/// A temporary directory for one test, removed when the value is dropped.
#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// Creates an empty directory whose name no other scratch directory of this process uses.
    fn new() -> io::Result<Self> {
        let name = format!(
            "artifree-manifest-{}-{}",
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
        let removed = fs::remove_dir_all(&self.root);
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

/// The manifest of `BASE_FILES`: one line per file, with the digest and length of its contents.
fn base_manifest() -> String {
    format!("{DIGEST_ABC} 3 bin/a\n{DIGEST_EMPTY} 0 bin/b\n{DIGEST_Z} 1 lib/x/y\n")
}

/// The base manifest with its first line replaced by `first`.
fn base_with_first_line(first: &str) -> Vec<u8> {
    let manifest = base_manifest();
    let rest = manifest.split_once('\n').map_or("", |(_, rest)| rest);
    format!("{first}\n{rest}").into_bytes()
}

/// Writes each `(path, contents)` pair below `root`, creating missing parent directories.
fn write_files(root: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
    for &(relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    Ok(())
}

/// Creates `scratch/root` holding `files`, and `manifest` as its `MANIFEST` when it is given.
fn make_root(
    scratch: &Scratch,
    files: &[(&str, &[u8])],
    manifest: Option<&[u8]>,
) -> io::Result<PathBuf> {
    let root = scratch.path().join("root");
    fs::create_dir(&root)?;
    write_files(&root, files)?;
    if let Some(bytes) = manifest {
        fs::write(root.join(MANIFEST_FILE), bytes)?;
    }
    Ok(root)
}

/// The baseline root: `BASE_FILES` with the manifest that lists them.
fn base_root(scratch: &Scratch) -> io::Result<PathBuf> {
    make_root(scratch, BASE_FILES, Some(base_manifest().as_bytes()))
}

#[test]
fn accepts_a_matching_root() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let root = base_root(&scratch)?;
    let identity = verify(&root)?;
    assert_eq!(
        identity.sha256(),
        BASE_MANIFEST_SHA256,
        "the identity must be the SHA-256 of the manifest's exact bytes"
    );
    Ok(())
}

#[test]
fn refuses_a_changed_byte_of_the_same_length() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let root = base_root(&scratch)?;
    write_files(&root, &[("bin/a", b"abd")])?;
    assert_eq!(
        verify(&root).map(|_| ()),
        Err(AdmissionRefusal::ToolRootMismatch),
        "a file of the listed length with other contents must differ by digest"
    );
    Ok(())
}

#[test]
fn refuses_a_changed_length() -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new()?;
    let root = base_root(&scratch)?;
    write_files(&root, &[("bin/a", b"abcd")])?;
    assert_eq!(
        verify(&root).map(|_| ()),
        Err(AdmissionRefusal::ToolRootMismatch),
        "a file longer than its listed length must be refused"
    );
    Ok(())
}

#[test]
fn refuses_an_extra_file_a_missing_file_and_an_unlisted_symlink() -> Result<(), Box<dyn Error>> {
    let edits: [(&str, Edit); 3] = [
        ("an extra file", |root| {
            fs::write(root.join("bin/extra"), b"")
        }),
        ("a missing file", |root| fs::remove_file(root.join("bin/b"))),
        ("an unlisted symlink", |root| {
            symlink("a", root.join("bin/link"))
        }),
    ];
    for (description, edit) in edits {
        let scratch = Scratch::new()?;
        let root = base_root(&scratch)?;
        edit(&root)?;
        assert_eq!(
            verify(&root).map(|_| ()),
            Err(AdmissionRefusal::ToolRootMismatch),
            "{description} must be refused"
        );
    }
    Ok(())
}

#[test]
fn refuses_a_listed_symlink() -> Result<(), Box<dyn Error>> {
    // The link to `bin/a` has the length of its target string, `a`, and the digest of the
    // contents it resolves to. Only the entry-type check refuses the second row, because the
    // first row's length already differs from the listed zero.
    let linked_manifest =
        format!("{DIGEST_ABC} 3 bin/a\n{DIGEST_ABC} 1 bin/b\n{DIGEST_Z} 1 lib/x/y\n");
    let cases = [
        ("a listed line that the link cannot match", base_manifest()),
        ("a listed line that the link matches", linked_manifest),
    ];
    for (description, manifest) in cases {
        let scratch = Scratch::new()?;
        let root = make_root(&scratch, BASE_FILES, Some(manifest.as_bytes()))?;
        fs::remove_file(root.join("bin/b"))?;
        symlink("a", root.join("bin/b"))?;
        assert_eq!(
            verify(&root).map(|_| ()),
            Err(AdmissionRefusal::ToolRootMismatch),
            "{description} must be refused"
        );
    }
    Ok(())
}

#[test]
fn refuses_a_missing_or_malformed_manifest() -> Result<(), Box<dyn Error>> {
    let unsorted = format!("{DIGEST_EMPTY} 0 bin/b\n{DIGEST_ABC} 3 bin/a\n{DIGEST_Z} 1 lib/x/y\n");
    let duplicate = format!("{DIGEST_ABC} 3 bin/a\n{DIGEST_ABC} 3 bin/a\n{DIGEST_EMPTY} 0 bin/b\n");
    let mut invalid_utf8 = base_manifest().into_bytes();
    invalid_utf8.insert(0, 0xff);
    // The padded last path is otherwise valid, so only the size bound can refuse it: without the
    // bound the root would differ from its listed paths.
    let mut oversized = base_manifest().trim_end_matches('\n').as_bytes().to_vec();
    oversized.resize(65_536, b' ');
    oversized.push(b'\n');
    // Each of the 513 lines is valid on its own; only the line bound refuses them.
    let too_many_lines = empty_files_manifest(513).into_bytes();
    let rows = [
        ("no MANIFEST", None),
        ("an empty MANIFEST", Some(Vec::new())),
        (
            "an uppercase digest",
            Some(base_with_first_line(&format!(
                "{} 3 bin/a",
                DIGEST_ABC.to_uppercase()
            ))),
        ),
        (
            "a digest of 63 characters",
            Some(base_with_first_line(&format!(
                "{} 3 bin/a",
                DIGEST_ABC.get(1..).unwrap_or_default()
            ))),
        ),
        (
            "a length with a leading zero",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 01 bin/a"))),
        ),
        (
            "a length with a non-digit",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3x bin/a"))),
        ),
        (
            "a line with no path",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3"))),
        ),
        (
            "a line with an empty path",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 "))),
        ),
        (
            "an absolute path",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 /bin/a"))),
        ),
        (
            "a `..` segment",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 bin/../a"))),
        ),
        (
            "an empty segment",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 bin//a"))),
        ),
        (
            "a `.` segment",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 ./bin/a"))),
        ),
        (
            "a line listing MANIFEST",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 MANIFEST"))),
        ),
        (
            "a control character in a path",
            Some(base_with_first_line(&format!("{DIGEST_ABC} 3 bin/\ta"))),
        ),
        ("two lines in unsorted order", Some(unsorted.into_bytes())),
        ("a duplicate line", Some(duplicate.into_bytes())),
        (
            "no final newline",
            Some(base_manifest().trim_end_matches('\n').as_bytes().to_vec()),
        ),
        ("invalid UTF-8", Some(invalid_utf8)),
        ("a manifest of 65,537 bytes", Some(oversized)),
        ("513 lines", Some(too_many_lines)),
    ];
    for (description, manifest) in rows {
        let scratch = Scratch::new()?;
        let root = make_root(&scratch, BASE_FILES, manifest.as_deref())?;
        assert_eq!(
            verify(&root).map(|_| ()),
            Err(AdmissionRefusal::MissingToolManifest),
            "{description} must be refused as a missing or malformed manifest"
        );
    }
    Ok(())
}

/// Creates `scratch/root` with `d/` holding `count` empty files and the manifest that lists them.
fn tree_of_empty_files(scratch: &Scratch, count: usize) -> io::Result<PathBuf> {
    let root = scratch.path().join("root");
    fs::create_dir_all(root.join("d"))?;
    for index in 0..count {
        fs::write(root.join(format!("d/{index:04}")), b"")?;
    }
    fs::write(root.join(MANIFEST_FILE), empty_files_manifest(count))?;
    Ok(root)
}

/// The manifest that lists the `count` empty files of `tree_of_empty_files`, in byte order.
fn empty_files_manifest(count: usize) -> String {
    (0..count)
        .map(|index| format!("{DIGEST_EMPTY} 0 d/{index:04}\n"))
        .collect::<Vec<String>>()
        .concat()
}

#[test]
fn walk_bound_on_entries() -> Result<(), Box<dyn Error>> {
    // The walk counts MANIFEST and `d/` as entries as well, so `d/` may hold 510 files: 1 + 1 +
    // 510 is exactly the 512-entry bound. Each larger tree is refused by the walk.
    let cases = [
        (510, Ok(())),
        (511, Err(AdmissionRefusal::ToolRootMismatch)),
        (512, Err(AdmissionRefusal::ToolRootMismatch)),
    ];
    for (count, expected) in cases {
        let scratch = Scratch::new()?;
        let root = tree_of_empty_files(&scratch, count)?;
        assert_eq!(
            verify(&root).map(|_| ()),
            expected,
            "{count} files under d/, with MANIFEST and d/ itself"
        );
    }
    Ok(())
}

#[test]
fn walk_bound_on_depth() -> Result<(), Box<dyn Error>> {
    // The root's children are at depth 1, so `g/file` is at depth 8 and `h/file` at depth 9.
    let cases = [
        ("a file at depth 8", "a/b/c/d/e/f/g/file", Ok(())),
        (
            "a file at depth 9",
            "a/b/c/d/e/f/g/h/file",
            Err(AdmissionRefusal::ToolRootMismatch),
        ),
    ];
    for (description, relative, expected) in cases {
        let scratch = Scratch::new()?;
        let manifest = format!("{DIGEST_EMPTY} 0 {relative}\n");
        let root = make_root(&scratch, &[(relative, b"")], Some(manifest.as_bytes()))?;
        assert_eq!(verify(&root).map(|_| ()), expected, "{description}");
    }
    Ok(())
}

/// The manifest of the two tool files that `make_host` writes, both empty, in byte order.
fn tool_manifest() -> Vec<u8> {
    format!(
        "{DIGEST_EMPTY} 0 {}\n{DIGEST_EMPTY} 0 {}\n",
        launcher::FIXTURE_FILE,
        launcher::LAUNCHER
    )
    .into_bytes()
}

/// The inputs that `admit_with` reads, with `tool_root` as the tool root.
struct Host {
    proc_self: PathBuf,
    osrelease: PathBuf,
    core_pattern: PathBuf,
    cgroup_mount: PathBuf,
    tool_root: PathBuf,
}

impl Host {
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
}

/// Builds a host whose tool root holds the launcher and the fixture, changed by `edit`, with
/// `manifest` as its `MANIFEST` when it is given. The provider's group holds only the provider,
/// so no variation here can pass delegation by accident.
fn make_host(scratch: &Scratch, edit: Edit, manifest: Option<Vec<u8>>) -> io::Result<Host> {
    let proc_self = scratch.dir("proc-self")?;
    fs::write(proc_self.join("cgroup"), "0::/provider.service\n")?;
    let osrelease = scratch.path().join("osrelease");
    fs::write(&osrelease, "7.0.0-test\n")?;
    let core_pattern = scratch.path().join("core_pattern");
    fs::write(&core_pattern, "core\n")?;
    let cgroup_mount = scratch.dir("mount")?;
    let group = cgroup_mount.join("provider.service");
    fs::create_dir(&group)?;
    fs::write(group.join("cgroup.kill"), "")?;
    fs::write(group.join("cgroup.subtree_control"), "")?;
    fs::write(group.join("cgroup.controllers"), "cpu io memory pids\n")?;
    fs::write(group.join("cgroup.procs"), format!("{}\n", process::id()))?;
    scratch.dir("tool-root/bin")?;
    let tool_root = scratch.path().join("tool-root");
    for relative in [launcher::LAUNCHER, launcher::FIXTURE_FILE] {
        let path = tool_root.join(relative);
        fs::write(&path, "")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    edit(&tool_root)?;
    if let Some(bytes) = manifest {
        fs::write(tool_root.join(MANIFEST_FILE), bytes)?;
    }
    Ok(Host {
        proc_self,
        osrelease,
        core_pattern,
        cgroup_mount,
        tool_root,
    })
}

#[test]
fn manifest_comes_after_the_named_files_and_before_delegation() -> Result<(), Box<dyn Error>> {
    // Facility 4 checks the launcher and the fixture first, so a missing one keeps its own
    // refusal. The manifest then runs before the delegated group is checked, so a valid manifest
    // reaches the first delegation refusal, which a worker uid that owns nothing triggers.
    let cases: [(&str, Edit, Option<Vec<u8>>, AdmissionRefusal); 5] = [
        (
            "a valid manifest reaches delegation",
            |_| Ok(()),
            Some(tool_manifest()),
            AdmissionRefusal::UndelegatedCgroup,
        ),
        (
            "no manifest with both named files present",
            |_| Ok(()),
            None,
            AdmissionRefusal::MissingToolManifest,
        ),
        (
            "no launcher and no manifest",
            |root| fs::remove_file(root.join(launcher::LAUNCHER)),
            None,
            AdmissionRefusal::MissingLauncher,
        ),
        (
            "no fixture and no manifest",
            |root| fs::remove_file(root.join(launcher::FIXTURE_FILE)),
            None,
            AdmissionRefusal::MissingFixture,
        ),
        (
            "a valid manifest and an extra tool file",
            |root| fs::write(root.join("bin/extra"), b""),
            Some(tool_manifest()),
            AdmissionRefusal::ToolRootMismatch,
        ),
    ];
    for (description, edit, manifest, expected) in cases {
        let scratch = Scratch::new()?;
        let host = make_host(&scratch, edit, manifest)?;
        let outcome = admit_with(
            InspectionProfile::fixture(),
            &host.roots(),
            process::id(),
            &mut None,
            || Ok(u32::MAX),
        )
        .map(|_| ());
        assert_eq!(outcome, Err(expected), "{description}");
    }
    Ok(())
}
