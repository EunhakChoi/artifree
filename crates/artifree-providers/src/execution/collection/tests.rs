//! Behavioral tests of output collection (decision 0042, rule 7; decision 0056, amendment of
//! 2026-11-13, input and output): a valid tree and its manifest, each hostile entry refused by its
//! reason, each bound exact, a device refused by its type, the swaps between classification and
//! open, and a same-size replacement read through its descriptor.
//!
//! Every tree is built in a fresh directory under the system temporary directory, which these
//! tests need to be case-sensitive. The tests run in the normal `cargo test`; they need no job
//! environment and no privileges.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use artifree_domain::package::MAX_FILES;

use super::*;

/// The result of a collection: the collected output, or the first refusal found.
type Outcome = Result<CollectedOutput, CollectionRefusal>;

/// The result of a test body: a setup or I/O failure ends the test with its error.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// A step that writes into a directory: it builds a tree there, or replaces one of its entries.
type Step = fn(&Path) -> io::Result<()>;

/// The hand-written header of the expected canonical text: the version tag, then the binding's
/// lines in their fixed order. The digests are the literal values that `binding` sets.
const HEADER: &str = concat!(
    "artifree-output-manifest-v1\n",
    "profile\tartifree-fixture-inspection-v1\n",
    "profile-manifest\t0101010101010101010101010101010101010101010101010101010101010101\n",
    "tool-root\tabababababababababababababababababababababababababababababababab\n",
    "filter\tcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n",
    "input\t0202020202020202020202020202020202020202020202020202020202020202\n",
);

/// The entry document of the valid trees. Its eight bytes appear in the expected text.
const INDEX_HTML: &[u8] = b"<p>i</p>";

/// The binding that every test collects under.
fn binding() -> ManifestBinding {
    ManifestBinding {
        profile: "artifree-fixture-inspection-v1",
        profile_manifest_sha256: [1; 32],
        tool_root_sha256: "ab".repeat(32),
        filter_sha256: "cd".repeat(32),
        input_digest: [2; 32],
    }
}

/// An empty directory under the system temporary directory, removed when it is dropped.
struct Scratch {
    /// The directory's path.
    path: PathBuf,
}

impl Scratch {
    /// Creates a new empty directory. The name holds the process id and a counter, so parallel
    /// tests never share a directory.
    fn new(label: &str) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "artifree-collect-{}-{index}-{label}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    /// The directory's path.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Removal is best effort: a leftover directory must not hide the assertion that failed.
        fs::remove_dir_all(&self.path).ok();
    }
}

/// Opens the directory at `path` as a handle, as the keeper holds the output root.
fn open_directory(path: &Path) -> TestResult<OwnedFd> {
    Ok(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// Collects the directory at `path` through `collect`, under the test binding.
fn collect_dir(path: &Path) -> TestResult<Outcome> {
    let root = open_directory(path)?;
    Ok(collect(&root, &binding()))
}

/// Collects the directory at `root`, running `swap` once on `page.txt` between its classification
/// and its open. Fails when the replacement fails or when the walk never reaches `page.txt`, so a
/// passing test has seen the swap happen.
fn collect_replacing_page(root: &Path, swap: Step) -> TestResult<Outcome> {
    let handle = open_directory(root)?;
    let replaced = Cell::new(false);
    let failure: RefCell<Option<io::Error>> = RefCell::new(None);
    let outcome = collect_with(&handle, &binding(), &mut |_: &OwnedFd, name: &str| {
        if name == "page.txt"
            && !replaced.replace(true)
            && let Err(error) = swap(root)
        {
            *failure.borrow_mut() = Some(error);
        }
    });
    if let Some(error) = failure.into_inner() {
        return Err(error.into());
    }
    if !replaced.get() {
        return Err("the walk never reached page.txt".into());
    }
    Ok(outcome)
}

/// The collected output, or a test error that names the refusal.
fn expect_collected(outcome: Outcome) -> TestResult<CollectedOutput> {
    outcome.map_err(|refusal| Box::<dyn Error>::from(format!("collection refused: {refusal:?}")))
}

/// Whether the package of `collected` holds a file at `path`.
fn holds(collected: &CollectedOutput, path: &str) -> bool {
    collected
        .package
        .files()
        .any(|(name, _)| name.as_str() == path)
}

/// A short description of an outcome, for an assertion line.
fn describe(outcome: &Outcome) -> String {
    match outcome {
        Ok(collected) => format!("a package of {} files", collected.package.files().len()),
        Err(refusal) => format!("refusal {refusal:?}"),
    }
}

/// What collection must report for one tree.
#[derive(Clone, Copy, Debug)]
enum Expected {
    /// The tree is refused with this reason.
    Refused(CollectionRefusal),
    /// The tree is collected, and its package holds a file at this path.
    Collected(&'static str),
}

impl Expected {
    /// Describes how `outcome` differs from this expectation, or returns `None` when they agree.
    fn mismatch(self, outcome: &Outcome) -> Option<String> {
        match (self, outcome) {
            (Self::Refused(expected), Err(actual)) if *actual == expected => None,
            (Self::Collected(path), Ok(collected)) if holds(collected, path) => None,
            (Self::Refused(expected), _) => Some(format!(
                "expected refusal {expected:?}, got {}",
                describe(outcome)
            )),
            (Self::Collected(path), _) => Some(format!(
                "expected a package holding {path}, got {}",
                describe(outcome)
            )),
        }
    }
}

/// One tree: how to build it in an empty directory, and what collection must report for it.
struct Tree {
    /// The row's name. It names the scratch directory and the mismatch line, so it is a slug.
    label: &'static str,
    /// Writes the tree into the empty directory it is given.
    build: Step,
    /// The outcome that collection must report.
    expected: Expected,
}

impl Tree {
    /// A tree that collection must refuse with `reason`.
    fn refused(label: &'static str, build: Step, reason: CollectionRefusal) -> Self {
        Self {
            label,
            build,
            expected: Expected::Refused(reason),
        }
    }

    /// A tree that collection must collect, with a package that holds a file at `path`.
    fn collected(label: &'static str, build: Step, path: &'static str) -> Self {
        Self {
            label,
            build,
            expected: Expected::Collected(path),
        }
    }
}

/// Builds and collects every tree, and returns one line for each whose outcome differs from its
/// expectation. Every mismatch is reported, not only the first.
fn mismatches(trees: &[Tree]) -> TestResult<Vec<String>> {
    let mut found = Vec::new();
    for tree in trees {
        let scratch = Scratch::new(tree.label)?;
        (tree.build)(scratch.path())?;
        let outcome = collect_dir(scratch.path())?;
        if let Some(problem) = tree.expected.mismatch(&outcome) {
            found.push(format!("{}: {problem}", tree.label));
        }
    }
    Ok(found)
}

/// Writes the valid entry document, `index.html`.
fn write_index(root: &Path) -> io::Result<()> {
    fs::write(root.join("index.html"), INDEX_HTML)
}

/// Writes `length` bytes of one value to `path`.
fn write_filled(path: &Path, length: usize) -> io::Result<()> {
    fs::write(path, vec![b'z'; length])
}

/// Writes `count` one-byte files, `f0.txt` and so on.
fn write_small_files(root: &Path, count: usize) -> io::Result<()> {
    for index in 0..count {
        fs::write(root.join(format!("f{index}.txt")), b"x")?;
    }
    Ok(())
}

/// Creates `count` empty directories, `d0` and so on.
fn write_directories(root: &Path, count: usize) -> io::Result<()> {
    for index in 0..count {
        fs::create_dir(root.join(format!("d{index}")))?;
    }
    Ok(())
}

/// Creates the directories `d1` to `d<levels>`, each inside the one before, and returns the
/// deepest of them.
fn nest(root: &Path, levels: usize) -> io::Result<PathBuf> {
    let mut deepest = root.to_path_buf();
    for level in 1..=levels {
        deepest.push(format!("d{level}"));
    }
    fs::create_dir_all(&deepest)?;
    Ok(deepest)
}

/// Creates a FIFO at `path`, owner-readable and owner-writable.
fn fifo(path: &Path) -> io::Result<()> {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        FileType::Fifo,
        Mode::RUSR | Mode::WUSR,
        0,
    )?;
    Ok(())
}

/// The length of the fifth file that brings a package to exactly `MAX_PACKAGE_BYTES`, with the
/// entry document and four files of `MAX_FILE_BYTES`.
fn fifth_file_length() -> usize {
    MAX_PACKAGE_BYTES - 4 * MAX_FILE_BYTES - INDEX_HTML.len()
}

/// Writes the entry document, four files of `MAX_FILE_BYTES` and a fifth of `last` bytes.
fn write_package_files(root: &Path, last: usize) -> io::Result<()> {
    write_index(root)?;
    for index in 0..4 {
        write_filled(&root.join(format!("b{index}.txt")), MAX_FILE_BYTES)?;
    }
    write_filled(&root.join("b4.txt"), last)
}

fn tree_symlink(root: &Path) -> io::Result<()> {
    write_index(root)?;
    symlink("index.html", root.join("link.txt"))
}

fn tree_socket(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The listener is dropped at once; the socket file stays, and that file is the entry.
    let listener = UnixListener::bind(root.join("s.txt"))?;
    drop(listener);
    Ok(())
}

fn tree_fifo(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fifo(&root.join("f.txt"))
}

fn tree_hard_link(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::hard_link(root.join("index.html"), root.join("h.txt"))
}

fn tree_too_many_entries(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The entry document and 2,048 directories make 2,049 entries, one more than the bound.
    write_directories(root, 2048)
}

fn tree_too_deep(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // A directory at level 8 is one level deeper than the bound allows.
    nest(root, 8)?;
    Ok(())
}

fn tree_file_too_large(root: &Path) -> io::Result<()> {
    write_index(root)?;
    write_filled(&root.join("big.txt"), MAX_FILE_BYTES + 1)
}

fn tree_package_too_large(root: &Path) -> io::Result<()> {
    write_package_files(root, MAX_FILE_BYTES)
}

fn tree_one_byte_over_package(root: &Path) -> io::Result<()> {
    write_package_files(root, fifth_file_length() + 1)
}

fn tree_hidden_name(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::write(root.join(".hidden.txt"), b"x")
}

fn tree_executable_name(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::write(root.join("x.exe"), b"x")
}

fn tree_case_collision(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::write(root.join("A.txt"), b"upper")?;
    fs::write(root.join("a.txt"), b"lower")?;
    // A case-insensitive directory would hold one entry for both names, and the row would test
    // nothing, so the build fails there instead.
    if fs::read_dir(root)?.count() != 3 {
        return Err(io::Error::other(
            "the temporary directory must be case-sensitive",
        ));
    }
    Ok(())
}

fn tree_hidden_directory(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::create_dir(root.join(".hidden"))
}

fn tree_directory_case_collision(root: &Path) -> io::Result<()> {
    write_index(root)?;
    fs::create_dir(root.join("Docs"))?;
    fs::create_dir(root.join("docs"))?;
    if fs::read_dir(root)?.count() != 3 {
        return Err(io::Error::other(
            "the temporary directory must be case-sensitive",
        ));
    }
    Ok(())
}

fn tree_too_many_files(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The entry document and MAX_FILES more files make one file more than the bound.
    write_small_files(root, MAX_FILES)
}

fn tree_missing_entry(root: &Path) -> io::Result<()> {
    fs::write(root.join("page.txt"), b"page")
}

fn tree_max_entries(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The entry document and 2,047 directories make the 2,048 entries that the bound allows.
    write_directories(root, 2047)
}

fn tree_deepest_file(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The file sits at level 7, the deepest directory the bound allows; its path has 8 segments.
    let deepest = nest(root, 7)?;
    fs::write(deepest.join("x.txt"), b"x")
}

fn tree_max_file(root: &Path) -> io::Result<()> {
    write_index(root)?;
    write_filled(&root.join("big.txt"), MAX_FILE_BYTES)
}

fn tree_max_files(root: &Path) -> io::Result<()> {
    write_index(root)?;
    // The entry document and MAX_FILES - 1 more files make exactly MAX_FILES files.
    write_small_files(root, MAX_FILES - 1)
}

fn tree_exact_package(root: &Path) -> io::Result<()> {
    write_package_files(root, fifth_file_length())
}

/// Replaces `page.txt` with a FIFO.
fn swap_for_fifo(root: &Path) -> io::Result<()> {
    fs::remove_file(root.join("page.txt"))?;
    fifo(&root.join("page.txt"))
}

/// Replaces `page.txt` with a symbolic link to the entry document.
fn swap_for_symlink(root: &Path) -> io::Result<()> {
    fs::remove_file(root.join("page.txt"))?;
    symlink("index.html", root.join("page.txt"))
}

/// Replaces `page.txt` with an empty directory.
fn swap_for_directory(root: &Path) -> io::Result<()> {
    fs::remove_file(root.join("page.txt"))?;
    fs::create_dir(root.join("page.txt"))
}

/// Replaces `page.txt` with a file of the same length and different bytes.
fn replace_with_same_size(root: &Path) -> io::Result<()> {
    fs::remove_file(root.join("page.txt"))?;
    fs::write(root.join("page.txt"), b"BBBB")
}

#[test]
fn a_valid_tree_is_collected_with_its_manifest() -> TestResult {
    let scratch = Scratch::new("valid")?;
    write_index(scratch.path())?;
    fs::create_dir(scratch.path().join("assets"))?;
    fs::write(scratch.path().join("assets/app.js"), b"x")?;
    let collected = expect_collected(collect_dir(scratch.path())?)?;

    let files: Vec<(String, Vec<u8>)> = collected
        .package
        .files()
        .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
        .collect();
    assert_eq!(
        files,
        [
            ("assets/app.js".to_owned(), b"x".to_vec()),
            ("index.html".to_owned(), INDEX_HTML.to_vec()),
        ],
        "the package holds the written bytes, in path order"
    );

    let expected = format!(
        "{HEADER}file\tassets/app.js\t1\t2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881\nfile\tindex.html\t8\t804a2503db0467dbff55deb3ac76d5cc3115702299af82510697647d0b1260d4\n"
    );
    assert_eq!(
        collected.manifest.canonical_text(),
        expected,
        "the canonical text is the hand-written text"
    );
    let expected_digest: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    assert_eq!(
        collected.manifest.digest(),
        expected_digest,
        "the digest is the SHA-256 of the canonical text"
    );
    Ok(())
}

#[test]
fn hostile_trees_are_refused_by_reason() -> TestResult {
    let trees = [
        Tree::refused("symlink", tree_symlink, CollectionRefusal::Link),
        Tree::refused("socket", tree_socket, CollectionRefusal::Socket),
        Tree::refused("fifo", tree_fifo, CollectionRefusal::Fifo),
        Tree::refused("hard-link", tree_hard_link, CollectionRefusal::HardLink),
        Tree::refused(
            "too-many-entries",
            tree_too_many_entries,
            CollectionRefusal::TooManyEntries,
        ),
        Tree::refused("too-deep", tree_too_deep, CollectionRefusal::TooDeep),
        Tree::refused(
            "file-too-large",
            tree_file_too_large,
            CollectionRefusal::FileTooLarge,
        ),
        Tree::refused(
            "package-too-large",
            tree_package_too_large,
            CollectionRefusal::TooLarge,
        ),
        Tree::refused(
            "one-byte-over-package",
            tree_one_byte_over_package,
            CollectionRefusal::TooLarge,
        ),
        Tree::refused(
            "hidden-name",
            tree_hidden_name,
            CollectionRefusal::InvalidPath,
        ),
        Tree::refused(
            "executable-name",
            tree_executable_name,
            CollectionRefusal::InvalidPath,
        ),
        Tree::refused(
            "case-collision",
            tree_case_collision,
            CollectionRefusal::Package(PackageReason::CaseCollision),
        ),
        Tree::refused(
            "hidden-empty-directory",
            tree_hidden_directory,
            CollectionRefusal::InvalidPath,
        ),
        Tree::refused(
            "empty-directories-colliding-in-case",
            tree_directory_case_collision,
            CollectionRefusal::Package(PackageReason::CaseCollision),
        ),
        Tree::refused(
            "too-many-files",
            tree_too_many_files,
            CollectionRefusal::Package(PackageReason::TooManyFiles),
        ),
        Tree::refused(
            "missing-entry",
            tree_missing_entry,
            CollectionRefusal::Package(PackageReason::MissingEntry),
        ),
    ];
    let problems = mismatches(&trees)?;
    assert!(
        problems.is_empty(),
        "hostile trees differ from their refusals:\n{}",
        problems.join("\n")
    );
    Ok(())
}

#[test]
fn bounds_are_exact() -> TestResult {
    let trees = [
        Tree::collected("max-entries", tree_max_entries, "index.html"),
        Tree::collected(
            "deepest-file",
            tree_deepest_file,
            "d1/d2/d3/d4/d5/d6/d7/x.txt",
        ),
        Tree::collected("max-file", tree_max_file, "big.txt"),
        Tree::collected("max-files", tree_max_files, "index.html"),
        Tree::collected("max-package", tree_exact_package, "b4.txt"),
    ];
    let problems = mismatches(&trees)?;
    assert!(
        problems.is_empty(),
        "bounds that must pass were refused:\n{}",
        problems.join("\n")
    );
    Ok(())
}

#[test]
fn a_device_is_refused_by_type() -> TestResult {
    let dev = open_directory(Path::new("/dev"))?;
    assert_eq!(
        classify(&dev, c"null"),
        Err(CollectionRefusal::Device),
        "/dev/null is a character device, refused by type"
    );
    Ok(())
}

#[test]
fn a_file_swapped_after_classification_is_refused() -> TestResult {
    let swaps: [(&str, Step, CollectionRefusal); 3] = [
        ("fifo", swap_for_fifo, CollectionRefusal::Changed),
        ("symlink", swap_for_symlink, CollectionRefusal::Unreadable),
        ("directory", swap_for_directory, CollectionRefusal::Changed),
    ];
    let mut problems = Vec::new();
    for (label, swap, refusal) in swaps {
        let scratch = Scratch::new(label)?;
        write_index(scratch.path())?;
        fs::write(scratch.path().join("page.txt"), b"page")?;
        let outcome = collect_replacing_page(scratch.path(), swap)?;
        if let Some(problem) = Expected::Refused(refusal).mismatch(&outcome) {
            problems.push(format!("{label}: {problem}"));
        }
    }
    assert!(
        problems.is_empty(),
        "swapped entries differ from their refusals:\n{}",
        problems.join("\n")
    );
    Ok(())
}

#[test]
fn a_same_size_replacement_is_collected_from_the_descriptor() -> TestResult {
    let scratch = Scratch::new("replacement")?;
    write_index(scratch.path())?;
    fs::write(scratch.path().join("page.txt"), b"AAAA")?;
    let collected = expect_collected(collect_replacing_page(
        scratch.path(),
        replace_with_same_size,
    )?)?;

    let files: Vec<(String, Vec<u8>)> = collected
        .package
        .files()
        .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
        .collect();
    assert_eq!(
        files,
        [
            ("index.html".to_owned(), INDEX_HTML.to_vec()),
            ("page.txt".to_owned(), b"BBBB".to_vec()),
        ],
        "the package holds the replacement bytes, not the first file's"
    );

    let expected = format!(
        "{HEADER}file\tindex.html\t8\t804a2503db0467dbff55deb3ac76d5cc3115702299af82510697647d0b1260d4\nfile\tpage.txt\t4\t4a8d8134f29b0b7b60c126f5532bc9f5d9bb73037373cf6fb872d81f1dcefdfd\n"
    );
    assert_eq!(
        collected.manifest.canonical_text(),
        expected,
        "the manifest hashes the bytes that were read through the descriptor"
    );
    let expected_digest: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    assert_eq!(collected.manifest.digest(), expected_digest);
    Ok(())
}
