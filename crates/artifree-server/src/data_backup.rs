//! Backup and restore of a data directory (decision 0027).
//!
//! The database file holds every persisted domain, so a backup is one file.
//! The access token file is not part of it: a restored directory gets a new
//! token the first time it serves, and the token records in the database
//! (managed tokens) keep working.

use std::fs;
use std::path::Path;

use artifree_providers::backup::{self, BackupSummary, SCRATCH_PREFIX};
use artifree_providers::storage::DATABASE_FILE;

use crate::serve::prepare_data_dir;

/// Writes a consistent snapshot of the data directory's database to the new
/// file `output`. A server may be running on the directory.
///
/// # Errors
///
/// When the directory has no database, `output` exists, the database is
/// damaged or of a newer schema, or a file operation fails.
pub fn backup(data_dir: &Path, output: &Path) -> Result<BackupSummary, String> {
    backup::create_backup(&data_dir.join(DATABASE_FILE), output)
        .map_err(|error| format!("backup failed: {error}"))
}

/// Creates the data directory `data_dir` from the backup file `input`. The
/// directory must not exist or must be empty; nothing in it is replaced.
///
/// # Errors
///
/// When the directory holds anything, the backup is damaged or of a newer
/// schema, or a file operation fails. A failure removes the directory this call
/// created, but not parent directories it had to create on the way.
pub fn restore(input: &Path, data_dir: &Path) -> Result<BackupSummary, String> {
    let created = match fs::read_dir(data_dir) {
        Ok(mut entries) => {
            if let Some(first) = entries.next() {
                let leftover = std::iter::once(first)
                    .chain(entries)
                    .filter_map(Result::ok)
                    .any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(SCRATCH_PREFIX)
                    });
                let hint = if leftover {
                    format!(
                        "; {SCRATCH_PREFIX}* directories are leftovers of an interrupted backup or restore and are safe to delete"
                    )
                } else {
                    String::new()
                };
                return Err(format!(
                    "restore failed: {} is not empty; restore into a new or empty directory{hint}",
                    data_dir.display()
                ));
            }
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(format!(
                "restore failed: cannot read {}: {error}",
                data_dir.display()
            ));
        }
    };
    prepare_data_dir(data_dir)?;
    let result = backup::restore_backup(input, &data_dir.join(DATABASE_FILE))
        .map_err(|error| format!("restore failed: {error}"));
    if result.is_err() && created {
        // Only an empty directory this call made; remove_dir refuses anything else.
        let _ = fs::remove_dir(data_dir);
    }
    result
}
