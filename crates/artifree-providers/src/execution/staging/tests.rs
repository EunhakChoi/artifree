//! Tests of the staged input (0056, amendment of 2026-11-13, input and output; decision 0042
//! rules 3 and 5): the listing digest, the refusals of `verify`, and the read-only tree that
//! `stage` leaves, its removal and its refusals. Each test works in a temporary directory that is
//! restored and removed when the test ends, whether it passed or not.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use artifree_domain::package::{FilePackage, PackagePath, PathError};
use sha2::{Digest, Sha256};

use super::{InputListing, InputRefusal, VerifiedInput};

/// Parses a package path literal.
fn package_path(text: &str) -> Result<PackagePath, PathError> {
    text.parse()
}

/// The package every test stages: a page and a script in a subdirectory.
fn sample_package() -> Result<FilePackage, Box<dyn Error>> {
    let files = vec![
        (package_path("index.html")?, b"<p>a</p>".to_vec()),
        (package_path("assets/app.js")?, b"x".to_vec()),
    ];
    Ok(FilePackage::new(files)?)
}

/// The SHA-256 of `bytes`, computed here rather than taken from the code under test.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The lowercase hexadecimal of `bytes`, written out here rather than taken from the code under
/// test.
fn hex_of(bytes: &[u8]) -> Result<String, fmt::Error> {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}")?;
    }
    Ok(text)
}

/// Turns a refusal into a test error that names the step that was refused.
fn accepted<T>(result: Result<T, InputRefusal>, step: &str) -> Result<T, String> {
    result.map_err(|refusal| format!("{step} refused with {refusal:?}"))
}

/// Sets the permission bits of `path`.
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// The permission bits of `path`, without the file type.
fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o7777)
}

/// The names of the entries of `directory`, sorted.
fn entry_names(directory: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// A temporary directory that one test owns. Dropping it makes every directory beneath it owner
/// writable and removes it, so a failed test leaves no read-only tree behind.
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    /// Creates an empty, owner-only directory named for this process and `label`.
    fn new(label: &str) -> io::Result<Self> {
        let path = env::temp_dir().join(format!("artifree-staging-{}-{label}", process::id()));
        discard(&path);
        fs::create_dir(&path)?;
        set_mode(&path, 0o700)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        discard(&self.path);
    }
}

/// Removes `path` if it exists, after making its read-only directories owner-writable. Errors are
/// ignored: a drop has no one to report them to, and `TempTree::new` reports a failed create.
fn discard(path: &Path) {
    make_owner_writable_tree(path).ok();
    fs::remove_dir_all(path).ok();
}

/// Makes `path` and every directory beneath it owner-writable, without following links.
fn make_owner_writable_tree(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return Ok(());
    }
    set_mode(path, 0o700)?;
    for entry in fs::read_dir(path)? {
        make_owner_writable_tree(&entry?.path())?;
    }
    Ok(())
}

/// How the listing of a table row differs from the listing of its package.
#[derive(Clone, Copy, Debug)]
enum Change {
    Unchanged,
    Path,
    Length,
    Checksum,
    Missing,
    Extra,
}

/// Applies `change` to the first entry of `listing`, or drops or adds an entry.
fn change_listing(listing: &mut InputListing, change: Change) -> Result<(), Box<dyn Error>> {
    match change {
        Change::Unchanged => {}
        Change::Path => {
            let entry = listing.0.first_mut().ok_or("the sample listing is empty")?;
            entry.0 = package_path("assets/other.js")?;
        }
        Change::Length => {
            let entry = listing.0.first_mut().ok_or("the sample listing is empty")?;
            entry.1 += 1;
        }
        Change::Checksum => {
            let byte = listing
                .0
                .first_mut()
                .and_then(|entry| entry.2.first_mut())
                .ok_or("the sample listing has no checksum byte")?;
            *byte ^= 0xff;
        }
        Change::Missing => {
            listing.0.pop().ok_or("the sample listing is empty")?;
        }
        Change::Extra => {
            listing.0.push((package_path("z.txt")?, 0, sha256(b"")));
        }
    }
    Ok(())
}

#[test]
fn listing_digest_is_the_sha256_of_its_canonical_text() -> Result<(), Box<dyn Error>> {
    let package = sample_package()?;
    let listing = InputListing::of(&package);
    let expected: Vec<(PackagePath, u64, [u8; 32])> = vec![
        (package_path("assets/app.js")?, 1, sha256(b"x")),
        (package_path("index.html")?, 8, sha256(b"<p>a</p>")),
    ];
    assert_eq!(
        listing.0, expected,
        "the listing must hold each file's path, length and digest, in path order"
    );
    let canonical = format!(
        "artifree-input-listing-v1\nassets/app.js\t1\t{}\nindex.html\t8\t{}\n",
        hex_of(&sha256(b"x"))?,
        hex_of(&sha256(b"<p>a</p>"))?,
    );
    assert_eq!(
        listing.digest(),
        sha256(canonical.as_bytes()),
        "the digest must be the SHA-256 of this canonical text:\n{canonical}"
    );
    Ok(())
}

#[test]
fn verify_refuses_a_listing_that_differs() -> Result<(), Box<dyn Error>> {
    let rows = [
        (Change::Unchanged, Ok(())),
        (Change::Path, Err(InputRefusal::ListingMismatch)),
        (Change::Length, Err(InputRefusal::ListingMismatch)),
        (Change::Checksum, Err(InputRefusal::ListingMismatch)),
        (Change::Missing, Err(InputRefusal::ListingMismatch)),
        (Change::Extra, Err(InputRefusal::ListingMismatch)),
    ];
    let mut failures = Vec::new();
    for (change, expected) in rows {
        let package = sample_package()?;
        let mut listing = InputListing::of(&package);
        change_listing(&mut listing, change)?;
        let actual = VerifiedInput::verify(package, listing).map(|_| ());
        if actual != expected {
            failures.push(format!("{change:?}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "verify disagreed with the listing in {} row(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

#[test]
fn stage_writes_read_only_files_and_directories() -> Result<(), Box<dyn Error>> {
    let tree = TempTree::new("stage")?;
    let package = sample_package()?;
    let listing = InputListing::of(&package);
    let verified = accepted(VerifiedInput::verify(package, listing.clone()), "verify")?;
    let staged = accepted(verified.stage(tree.path()), "stage")?;
    let input = tree.path().join("input");
    let page = input.join("index.html");
    let script = input.join("assets").join("app.js");
    assert_eq!(
        staged.path(),
        input.as_path(),
        "the staging directory must be execution_dir/input"
    );
    assert_eq!(
        fs::read(&page)?,
        b"<p>a</p>".to_vec(),
        "index.html must hold the package's bytes"
    );
    assert_eq!(
        fs::read(&script)?,
        b"x".to_vec(),
        "assets/app.js must hold the package's bytes"
    );
    assert_eq!(mode_of(&page)?, 0o444, "a staged file must be read only");
    assert_eq!(mode_of(&script)?, 0o444, "a staged file must be read only");
    assert_eq!(
        mode_of(&input.join("assets"))?,
        0o555,
        "a filled subdirectory must be read and search only"
    );
    assert_eq!(
        mode_of(&input)?,
        0o555,
        "a filled staging directory must be read and search only"
    );
    assert_eq!(
        staged.listing(),
        &listing,
        "the staged listing must be the one the files were checked against"
    );
    assert_eq!(
        staged.digest(),
        listing.digest(),
        "the staged digest must be the digest of the listing"
    );
    assert_eq!(
        staged.remove(),
        Ok(()),
        "remove must succeed on a tree the test created"
    );
    assert!(
        !input.exists(),
        "remove must take the staging directory away"
    );
    Ok(())
}

#[test]
fn a_leftover_with_read_only_directories_is_removed_before_staging() -> Result<(), Box<dyn Error>> {
    let tree = TempTree::new("leftover")?;
    let input = tree.path().join("input");
    fs::create_dir_all(input.join("old"))?;
    fs::write(input.join("old").join("stale.txt"), b"stale")?;
    set_mode(&input.join("old"), 0o555)?;
    set_mode(&input, 0o555)?;
    let package = sample_package()?;
    let listing = InputListing::of(&package);
    let verified = accepted(VerifiedInput::verify(package, listing), "verify")?;
    let staged = accepted(verified.stage(tree.path()), "staging over the leftover")?;
    assert_eq!(
        entry_names(&input)?,
        vec!["assets", "index.html"],
        "the leftover tree must be removed, and only the staged files must remain"
    );
    assert_eq!(
        fs::read(input.join("index.html"))?,
        b"<p>a</p>".to_vec(),
        "index.html must hold the new bytes, not the leftover's"
    );
    assert_eq!(
        staged.remove(),
        Ok(()),
        "remove must succeed after staging over a leftover"
    );
    Ok(())
}

/// Makes a leftover at the staging path (the second argument); the first is a directory it may
/// point to.
type LeftoverStep = fn(&Path, &Path) -> io::Result<()>;

#[test]
fn a_leftover_file_or_link_is_unlinked_before_staging() -> Result<(), Box<dyn Error>> {
    let tree = TempTree::new("leftover-entries")?;
    let target = tree.path().join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("kept.txt"), b"kept")?;
    let input = tree.path().join("input");
    let leftovers: [(&str, LeftoverStep); 2] = [
        ("a file", |_, input| fs::write(input, b"stale")),
        ("a link to a directory", |target, input| {
            std::os::unix::fs::symlink(target, input)
        }),
    ];
    for (kind, make) in leftovers {
        make(&target, &input)?;
        let package = sample_package()?;
        let listing = InputListing::of(&package);
        let verified = accepted(VerifiedInput::verify(package, listing), "verify")?;
        let staged = accepted(verified.stage(tree.path()), kind)?;
        assert_eq!(
            entry_names(&input)?,
            vec!["assets", "index.html"],
            "{kind} at the staging path must be replaced by the staged files"
        );
        assert_eq!(staged.remove(), Ok(()), "remove must succeed after {kind}");
    }
    assert_eq!(
        fs::read(target.join("kept.txt"))?,
        b"kept".to_vec(),
        "the link's target must be untouched"
    );
    Ok(())
}

#[test]
fn dropping_a_staged_input_removes_it() -> Result<(), Box<dyn Error>> {
    let tree = TempTree::new("drop")?;
    let package = sample_package()?;
    let listing = InputListing::of(&package);
    let verified = accepted(VerifiedInput::verify(package, listing), "verify")?;
    let staged = accepted(verified.stage(tree.path()), "stage")?;
    let input = tree.path().join("input");
    assert!(
        input.is_dir(),
        "the staging directory must exist before the drop"
    );
    drop(staged);
    assert!(
        !input.exists(),
        "dropping an unremoved StagedInput must remove the staging directory"
    );
    Ok(())
}

#[test]
fn a_leftover_that_cannot_be_removed_refuses_staging() -> Result<(), Box<dyn Error>> {
    let tree = TempTree::new("locked")?;
    let input = tree.path().join("input");
    fs::create_dir(&input)?;
    fs::write(input.join("stale.txt"), b"stale")?;
    fs::write(tree.path().join("probe.txt"), b"")?;
    set_mode(tree.path(), 0o555)?;
    let owner_can_unlink = fs::remove_file(tree.path().join("probe.txt")).is_ok();
    assert!(
        !owner_can_unlink,
        "the owner must not unlink in a 0555 directory; this test must not run as root"
    );
    let package = sample_package()?;
    let listing = InputListing::of(&package);
    let verified = accepted(VerifiedInput::verify(package, listing), "verify")?;
    let outcome = verified.stage(tree.path()).map(|_| ());
    set_mode(tree.path(), 0o700)?;
    assert_eq!(
        outcome,
        Err(InputRefusal::LeftoverNotRemoved),
        "a leftover that cannot be removed must refuse the staging"
    );
    Ok(())
}
