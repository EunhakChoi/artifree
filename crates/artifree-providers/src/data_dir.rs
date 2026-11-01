//! Creating the data directory that holds the database, its WAL, and the
//! access token.

use std::fs::{self, DirBuilder};
use std::io;
use std::path::Path;

/// Creates the data directory for the current user only, or checks that an
/// existing one is. It holds the database, its WAL, and the access token, so
/// anyone who can enter it can read every artifact without the token. An
/// existing directory that is open to group or other users is refused rather
/// than changed, because it may be shared on purpose. On other platforms the
/// directory inherits its parent's access control.
///
/// # Errors
///
/// A message when the directory cannot be created or inspected, or is open to
/// other users (Unix).
pub fn prepare_data_dir(path: &Path) -> Result<(), String> {
    let cannot_create = |error: io::Error| format!("cannot create {}: {error}", path.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder()
            .recursive(true)
            .create(path)
            .map_err(cannot_create)?;
        let metadata = fs::metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} is accessible to other users (mode {mode:03o}); run chmod 700 on it or choose another --data-dir",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path).map_err(cannot_create)?;
    Ok(())
}

/// Creates a data directory that must not exist yet, with the access policy of
/// [`prepare_data_dir`]. Missing parent directories are created the same way
/// and are not removed again by a caller that fails later.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] when anything is at `path`, also a
/// dangling symbolic link; other errors when it cannot be created.
pub fn create_new_data_dir(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        builder().recursive(true).create(parent)?;
    }
    builder().create(path)
}

#[cfg(unix)]
fn builder() -> DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
}

#[cfg(not(unix))]
fn builder() -> DirBuilder {
    DirBuilder::new()
}

/// Creates one directory that only the current user can enter (Unix), failing
/// if it exists. Its parent must exist.
///
/// # Errors
///
/// When the directory cannot be created.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    builder().create(path)
}
