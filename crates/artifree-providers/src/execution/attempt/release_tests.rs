//! The release of a record whose attempt ended before its group existed (decision 0057 rules 10
//! and 19): the record is removed at once, so its job can be settled, only when no group of its
//! name exists and the staged input is gone. No launch and no cgroup: temporary directories stand
//! for the execution directory and the delegated parent.

use std::error::Error;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process;

use super::super::registry::{Identity, Record, Registry};
use super::super::staging::INPUT_DIR;
use super::{Registered, release_ungrouped};

/// The result of a test body: a setup failure ends the test with its error.
type TestResult = Result<(), Box<dyn Error>>;

/// A private temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    /// Makes a fresh directory whose name holds `label` and this process.
    fn new(label: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("artifree-release-{label}-{}", process::id()));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// An execution directory with a staged input, a delegated parent, and the registered record of
/// one attempt, all under `root`.
fn setup(root: &Scratch) -> Result<(PathBuf, PathBuf, Registered), Box<dyn Error>> {
    let execution = root.0.join("execution");
    DirBuilder::new().mode(0o700).create(&execution)?;
    fs::create_dir(execution.join(INPUT_DIR))?;
    let delegated = root.0.join("delegated");
    fs::create_dir(&delegated)?;
    let identity = Identity([7; 16]);
    let registry = Registry::open(&execution)?;
    registry.write(&Record {
        identity,
        machine: "0123456789abcdef0123456789abcdef".to_owned(),
        boot: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
        cgroupns: "cgroup:[4026531835]".to_owned(),
        parent: "system.slice/x.service".to_owned(),
        group: identity.group_name(),
        run: None,
    })?;
    let registered = Registered {
        registry,
        identity,
        removed: false,
        #[cfg(feature = "enforcement-fixture")]
        stderr: Vec::new(),
    };
    Ok((execution, delegated, registered))
}

/// Whether the registry of `registered` lists no entry.
fn registry_empty(registered: &Registered) -> io::Result<bool> {
    Ok(registered.registry.entries()?.is_empty())
}

#[test]
fn a_record_whose_group_never_existed_is_released() -> TestResult {
    let root = Scratch::new("no-group")?;
    let (execution, delegated, mut registered) = setup(&root)?;
    release_ungrouped(&mut registered, &delegated, &execution);
    assert!(registered.removed, "the attempt may be settled at once");
    assert!(registry_empty(&registered)?, "the record is gone");
    assert!(
        !Path::exists(&execution.join(INPUT_DIR)),
        "the staged input is gone before the record"
    );
    Ok(())
}

#[test]
fn a_record_whose_group_exists_stays_for_reconciliation() -> TestResult {
    let root = Scratch::new("group")?;
    let (execution, delegated, mut registered) = setup(&root)?;
    fs::create_dir(delegated.join(registered.identity.group_name()))?;
    release_ungrouped(&mut registered, &delegated, &execution);
    assert!(
        !registered.removed,
        "an attempt whose group may hold processes is not settled"
    );
    assert!(!registry_empty(&registered)?, "the record stays");
    Ok(())
}
