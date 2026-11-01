//! Fixtures shared by the backup and schema upgrade tests.
#![allow(dead_code, reason = "Each test target uses a different subset.")]

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_domain::package::{FilePackage, PackagePath};
use sha2::{Digest, Sha256};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A unique directory removed on drop.
pub struct TempDir {
    directory: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", directory.display()));
        Self { directory }
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

pub fn package(files: &[(&str, &str)]) -> FilePackage {
    FilePackage::new(
        files
            .iter()
            .map(|(path, content)| (parsed::<PackagePath>(path), content.as_bytes().to_vec()))
            .collect(),
    )
    .unwrap_or_else(|error| panic!("invalid fixture package: {error}"))
}

pub fn sha256(content: &str) -> [u8; 32] {
    Sha256::digest(content).into()
}
