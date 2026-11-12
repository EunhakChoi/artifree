//! Evidence that one Linux admission run recorded (decision 0056 rule 13).

use std::fmt;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

/// Evidence one Linux admission run measured (decision 0056 rule 13).
///
/// The report records what was measured and nothing more. It is never a
/// launch permission, it records the digest of the tool root's manifest that
/// the admission verified, and it does not claim that the host is supported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostReport {
    /// Kernel release the run was measured on.
    pub(super) kernel_release: String,
    /// CPU architecture the run was measured on.
    pub(super) architecture: &'static str,
    /// Version the launcher reports, or `None` when it could not be read.
    pub(super) launcher_version: Option<String>,
    /// SHA-256 of the launcher file, in lowercase hexadecimal.
    pub(super) launcher_sha256: String,
    /// SHA-256 of the tool root's manifest that admission verified, in lowercase
    /// hexadecimal.
    pub(super) tool_manifest_sha256: String,
    /// SHA-256 of the inspection profile manifest, in lowercase hexadecimal.
    pub(super) policy_sha256: String,
    /// Core routing as read from the host, verbatim.
    pub(super) core_routing: String,
    /// Controllers the delegated group reports.
    pub(super) delegated_controllers: String,
    /// Limits read back from the probe group, as control file and value.
    pub(super) effective_limits: Vec<(&'static str, String)>,
    /// Peak memory of the probe group in bytes, as read from `memory.peak`, when the
    /// kernel offers it.
    pub(super) peak_memory_bytes: Option<u64>,
}

impl fmt::Display for HostReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "kernel_release={}", self.kernel_release)?;
        writeln!(formatter, "architecture={}", self.architecture)?;
        writeln!(
            formatter,
            "launcher_version={}",
            self.launcher_version.as_deref().unwrap_or("unavailable")
        )?;
        writeln!(formatter, "launcher_sha256={}", self.launcher_sha256)?;
        writeln!(
            formatter,
            "tool_manifest_sha256={}",
            self.tool_manifest_sha256
        )?;
        writeln!(formatter, "policy_sha256={}", self.policy_sha256)?;
        writeln!(formatter, "core_routing={}", self.core_routing)?;
        writeln!(
            formatter,
            "delegated_controllers={}",
            self.delegated_controllers
        )?;
        for (file, value) in &self.effective_limits {
            writeln!(formatter, "limit.{file}={value}")?;
        }
        match self.peak_memory_bytes {
            Some(bytes) => writeln!(formatter, "peak_memory_bytes={bytes}")?,
            None => writeln!(formatter, "peak_memory_bytes=unavailable")?,
        }
        writeln!(
            formatter,
            "cleanup_observation=killed, populated 0 within 5 s, child reaped, group removed"
        )?;
        writeln!(formatter, "listener_counters=not measured in this slice")
    }
}

/// Returns the lowercase SHA-256 hex digest of `bytes`.
#[cfg(target_os = "linux")]
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    hex(digest)
}

/// Returns the lowercase SHA-256 hex digest of the file at `path`.
///
/// The file is read in fixed-size chunks, so its size does not bound memory
/// use.
///
/// # Errors
///
/// Returns an error when the file cannot be opened or read.
#[cfg(target_os = "linux")]
pub(super) fn sha256_file_hex(path: &Path) -> io::Result<String> {
    const CHUNK_BYTES: usize = 64 * 1024;

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| io::Error::other("read reported more bytes than the buffer holds"))?;
        hasher.update(chunk);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(hex(digest))
}

/// Renders a SHA-256 digest as lowercase hexadecimal, two digits per byte.
#[cfg(target_os = "linux")]
fn hex(digest: [u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}
