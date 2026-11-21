//! Validation for UndoOS's root-owned system recovery storage.
//!
//! This module intentionally does not create, modify, or delete recovery data.
//! The systemd-tmpfiles rule owns directory creation; the service only verifies
//! that the fixed storage path has the expected ownership and permissions.

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

const SYSTEM_STORAGE_PATH: &str = "/var/lib/lifeline/system";
const ROOT_UID: u32 = 0;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

/// Verify a directory without following a symlink at its final path component.
fn verify_directory(path: &Path, expected_uid: u32, exact_mode: Option<u32>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;

    if metadata.file_type().is_symlink() {
        return Err(format!("{} must not be a symbolic link", path.display()));
    }
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if metadata.uid() != expected_uid {
        return Err(format!(
            "{} is owned by UID {}, expected UID {expected_uid}",
            path.display(),
            metadata.uid()
        ));
    }

    let mode = metadata.permissions().mode() & 0o777;
    if let Some(expected) = exact_mode {
        if mode != expected {
            return Err(format!(
                "{} has mode {:04o}, expected {:04o}",
                path.display(),
                mode,
                expected
            ));
        }
    } else if mode & 0o022 != 0 {
        return Err(format!(
            "{} is writable by group or others (mode {:04o})",
            path.display(),
            mode
        ));
    }

    Ok(())
}

/// Confirm that every fixed path component is a root-owned directory and
/// that the two UndoOS-managed directories are private (0700).
pub fn verify() -> Result<PathBuf, String> {
    let components = [
        (Path::new("/var"), None),
        (Path::new("/var/lib"), None),
        (Path::new("/var/lib/lifeline"), Some(PRIVATE_DIRECTORY_MODE)),
        (Path::new(SYSTEM_STORAGE_PATH), Some(PRIVATE_DIRECTORY_MODE)),
    ];

    for (path, exact_mode) in components {
        verify_directory(path, ROOT_UID, exact_mode)?;
    }

    Ok(PathBuf::from(SYSTEM_STORAGE_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("lifeline-storage-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn accepts_a_private_directory_with_expected_owner() {
        let path = temporary_directory("valid");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(verify_directory(&path, unsafe { libc::geteuid() }, Some(0o700)).is_ok());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_group_or_world_writable_parent_directories() {
        let path = temporary_directory("writable");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();

        let result = verify_directory(&path, unsafe { libc::geteuid() }, None);
        assert!(result.is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_a_directory_with_the_wrong_private_mode() {
        let path = temporary_directory("wrong-mode");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let result = verify_directory(&path, unsafe { libc::geteuid() }, Some(0o700));
        assert!(result.is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_symbolic_links() {
        let directory = temporary_directory("symlink-target");
        let link = directory.with_extension("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();

        let result = verify_directory(&link, unsafe { libc::geteuid() }, Some(0o700));
        assert!(result.is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
