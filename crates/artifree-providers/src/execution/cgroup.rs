//! Cgroup v2 checks below the provider's own delegated group (decision 0056 rule 8).

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{AdmissionRefusal, Controller, read_bounded, write_control};

/// The child group the provider moves itself into (0056 rule 8: "a child group named supervisor").
pub(super) const SUPERVISOR_GROUP: &str = "supervisor";

/// Largest cgroup control file read here; a longer file counts as unreadable.
const CONTROL_FILE_BYTES: u64 = 4096;

/// Shared helper: reads a bounded control file as UTF-8, or `None` when it cannot be read.
fn read_control_text(path: &Path) -> Option<String> {
    let bytes = read_bounded(path, CONTROL_FILE_BYTES).ok()?;
    String::from_utf8(bytes).ok()
}

/// Facility 2: finds the provider's cgroup v2 group from the `0::` line of `/proc/self/cgroup`.
pub(super) fn discover_own_group(
    proc_self_cgroup: &Path,
    cgroup_mount: &Path,
) -> Result<PathBuf, AdmissionRefusal> {
    let text = read_control_text(proc_self_cgroup).ok_or(AdmissionRefusal::CgroupV2Unavailable)?;
    let unified: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect();
    let [value] = unified.as_slice() else {
        return Err(AdmissionRefusal::CgroupV2Unavailable);
    };
    let Some(relative) = value.strip_prefix('/') else {
        return Err(AdmissionRefusal::CgroupV2Unavailable);
    };
    let mut group = cgroup_mount.to_path_buf();
    let segments = relative.split('/').filter(|segment| !segment.is_empty());
    for segment in segments {
        if matches!(segment, "." | "..") {
            return Err(AdmissionRefusal::CgroupV2Unavailable);
        }
        group.push(segment);
    }
    // A unified hierarchy has this file in every group. Without it the line names a group
    // that is not under this mount, as on a hybrid host.
    let controllers = fs::symlink_metadata(group.join("cgroup.controllers"));
    if !controllers.is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err(AdmissionRefusal::CgroupV2Unavailable);
    }
    // The path is returned as found: a process already inside `supervisor` gets that path.
    Ok(group)
}

/// Facility 2: the provider's cgroup has a `cgroup.kill` file.
pub(super) fn check_kill_support(group: &Path) -> Result<(), AdmissionRefusal> {
    let kill_file = fs::symlink_metadata(group.join("cgroup.kill"));
    if kill_file.is_ok_and(|metadata| metadata.file_type().is_file()) {
        Ok(())
    } else {
        Err(AdmissionRefusal::MissingKillSupport)
    }
}

/// Facility 5: reads the worker's uid as the owner of `/proc/self`, since std has no `getuid`.
pub(super) fn own_uid(proc_self: &Path) -> Result<u32, AdmissionRefusal> {
    let metadata = fs::metadata(proc_self).map_err(|_| AdmissionRefusal::UndelegatedCgroup)?;
    Ok(metadata.uid())
}

/// Facility 5 helper: whether `path` exists, is not followed as a symlink, and is owned by `uid`.
fn owned_by(path: &Path, uid: u32) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.uid() == uid)
}

/// Facility 5: the group is a directory, and it and its two control files belong to `own_uid`.
pub(super) fn check_delegation(group: &Path, own_uid: u32) -> Result<(), AdmissionRefusal> {
    let is_directory =
        fs::symlink_metadata(group).is_ok_and(|metadata| metadata.file_type().is_dir());
    let owned = owned_by(group, own_uid)
        && owned_by(&group.join("cgroup.procs"), own_uid)
        && owned_by(&group.join("cgroup.subtree_control"), own_uid);
    if is_directory && owned {
        Ok(())
    } else {
        Err(AdmissionRefusal::UndelegatedCgroup)
    }
}

/// Facility 6: the first required controller that is not a whole token of `listed`.
pub(super) fn first_missing_controller(listed: &str) -> Option<Controller> {
    let tokens: Vec<&str> = listed.split_ascii_whitespace().collect();
    Controller::REQUIRED
        .into_iter()
        .find(|controller| !tokens.contains(&controller.name()))
}

/// Facility 6: the group lists every required controller; an unreadable list names memory.
pub(super) fn check_controllers(group: &Path) -> Result<String, AdmissionRefusal> {
    let text = read_control_text(&group.join("cgroup.controllers"))
        .ok_or(AdmissionRefusal::MissingController(Controller::Memory))?;
    if let Some(controller) = first_missing_controller(&text) {
        return Err(AdmissionRefusal::MissingController(controller));
    }
    Ok(text.trim_end().to_owned())
}

/// Facility 7, before the move: `own_pid` must be the only member; unreadable counts as occupied.
pub(super) fn check_sole_occupant(group: &Path, own_pid: u32) -> Result<(), AdmissionRefusal> {
    let text =
        read_control_text(&group.join("cgroup.procs")).ok_or(AdmissionRefusal::OccupiedCgroup)?;
    let mut members: Vec<u32> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let member: u32 = line.parse().map_err(|_| AdmissionRefusal::OccupiedCgroup)?;
        members.push(member);
    }
    if members == [own_pid] {
        Ok(())
    } else {
        Err(AdmissionRefusal::OccupiedCgroup)
    }
}

/// Facility 7 for `EBUSY` (another process holds the group), facility 5 for other write errors.
pub(super) fn subtree_write_refusal(error: &io::Error) -> AdmissionRefusal {
    if error.kind() == io::ErrorKind::ResourceBusy {
        AdmissionRefusal::OccupiedCgroup
    } else {
        AdmissionRefusal::UndelegatedCgroup
    }
}

/// Facilities 5 to 7: moves into `supervisor` and enables controllers for the group's children.
pub(super) fn enter_supervisor(group: &Path, own_pid: u32) -> Result<(), AdmissionRefusal> {
    // 0056 rule 8 fixes this order: the kernel refuses the subtree write with `EBUSY` while
    // another process still sits directly in the group, so the move must come first.
    let supervisor = group.join(SUPERVISOR_GROUP);
    let created = fs::create_dir(&supervisor);
    if created.is_err_and(|error| error.kind() != io::ErrorKind::AlreadyExists) {
        return Err(AdmissionRefusal::UndelegatedCgroup);
    }
    write_control(&supervisor.join("cgroup.procs"), &own_pid.to_string())
        .map_err(|_| AdmissionRefusal::UndelegatedCgroup)?;
    write_control(&group.join("cgroup.subtree_control"), "+memory +pids +cpu")
        .map_err(|error| subtree_write_refusal(&error))?;
    let subtree = read_control_text(&group.join("cgroup.subtree_control"))
        .ok_or(AdmissionRefusal::MissingController(Controller::Memory))?;
    if let Some(controller) = first_missing_controller(&subtree) {
        return Err(AdmissionRefusal::MissingController(controller));
    }
    Ok(())
}
