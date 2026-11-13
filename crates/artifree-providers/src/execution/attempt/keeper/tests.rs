//! Unit tests for the keeper's mount check (decision 0056, amendment of 2026-11-13, input and
//! output: the mount-id comparison).

use super::*;

/// The filesystem type that `fstatfs` reports for a tmpfs, the kernel's `TMPFS_MAGIC`.
const TMPFS_SUPER_MAGIC: i64 = 0x0102_1994;

/// The filesystem type that `fstatfs` reports for a procfs, the kernel's `PROC_SUPER_MAGIC`.
const PROC_SUPER_MAGIC: i64 = 0x9fa0;

/// The filesystem type that `fstatfs` reports for an ext4 volume, the kernel's `EXT4_SUPER_MAGIC`.
const EXT4_SUPER_MAGIC: i64 = 0xEF53;

/// The mount id that the observation read from the scratch line.
const SCRATCH_MOUNT_ID: u64 = 270;

#[test]
fn the_mount_id_and_the_filesystem_must_match() {
    // Each row is (filesystem type of the descriptor, its mount id, whether the keeper accepts it).
    let rows: [(i64, u64, bool); 4] = [
        (TMPFS_SUPER_MAGIC, SCRATCH_MOUNT_ID, true),
        (TMPFS_SUPER_MAGIC, SCRATCH_MOUNT_ID + 1, false),
        (PROC_SUPER_MAGIC, SCRATCH_MOUNT_ID, false),
        (EXT4_SUPER_MAGIC, SCRATCH_MOUNT_ID, false),
    ];
    let mut mismatches = Vec::new();
    for (filesystem_type, mount_id, expected) in rows {
        let actual = is_checked_scratch(filesystem_type, mount_id, SCRATCH_MOUNT_ID);
        if actual != expected {
            mismatches.push(format!(
                "({filesystem_type:#x}, {mount_id}) returned {actual}, expected {expected}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "is_checked_scratch disagrees with the contract for scratch mount id \
         {SCRATCH_MOUNT_ID} on these rows (filesystem type, mount id): {}",
        mismatches.join("; ")
    );
}
