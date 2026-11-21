//! Narrow, internal system-file capture coordinator.
//!
//! Capture is restricted to an explicitly supplied policy root. This module is
//! not reachable through the IPC protocol; callers must choose the allowed
//! root themselves. The initial implementation is intended for controlled
//! fixtures and future allowlisted system paths, not broad /etc or / access.

#![allow(dead_code)]

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use super::{system_catalog, system_store};

const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

fn same_source_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.mode() == after.mode()
        && before.uid() == after.uid()
        && before.gid() == after.gid()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn open_source(allowed_root: &Path, requested_path: &Path) -> Result<(PathBuf, File, fs::Metadata), String> {
    // Inspect the caller-supplied final component before canonicalization:
    // canonicalize alone would follow a symbolic link and hide that fact.
    let requested_metadata = fs::symlink_metadata(requested_path)
        .map_err(|error| format!("cannot inspect requested system file: {error}"))?;
    if requested_metadata.file_type().is_symlink() {
        return Err("symbolic-link capture requests are not permitted".to_string());
    }

    let root = fs::canonicalize(allowed_root)
        .map_err(|error| format!("cannot resolve capture policy root: {error}"))?;
    let source = fs::canonicalize(requested_path)
        .map_err(|error| format!("cannot resolve requested system file: {error}"))?;

    if source == root || !source.starts_with(&root) {
        return Err("requested file is outside the permitted capture root".to_string());
    }

    let path_metadata = fs::symlink_metadata(&source)
        .map_err(|error| format!("cannot inspect requested system file: {error}"))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err("only regular, non-symbolic-link files can be captured".to_string());
    }
    if path_metadata.len() > MAX_CAPTURE_BYTES {
        return Err("system file exceeds the 64 MiB capture limit".to_string());
    }

    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(&source)
        .map_err(|error| format!("cannot safely open requested system file: {error}"))?;
    let descriptor_metadata = file.metadata()
        .map_err(|error| format!("cannot inspect opened system file: {error}"))?;

    if !descriptor_metadata.is_file()
        || !same_source_state(&path_metadata, &descriptor_metadata)
    {
        return Err("system file changed while it was being opened".to_string());
    }

    Ok((source, file, descriptor_metadata))
}

/// Capture one regular file beneath the explicitly supplied policy root.
///
/// If catalog registration fails after the object is durable, the object is
/// deliberately retained as an unreferenced orphan. Retention is safer than
/// deletion when the catalog's durability outcome is uncertain.
pub fn capture(
    storage_root: &Path,
    allowed_root: &Path,
    requested_path: &Path,
) -> Result<system_catalog::CatalogRecord, String> {
    let (source_path, mut file, initial_metadata) = open_source(allowed_root, requested_path)?;

    let mut bytes = Vec::with_capacity(initial_metadata.len() as usize);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek system file: {error}"))?;
    (&mut file).take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read system file: {error}"))?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err("system file grew beyond the 64 MiB capture limit".to_string());
    }

    let final_metadata = file.metadata()
        .map_err(|error| format!("cannot recheck opened system file: {error}"))?;
    if !same_source_state(&initial_metadata, &final_metadata)
        || bytes.len() as u64 != initial_metadata.len()
    {
        return Err("system file changed while it was being captured".to_string());
    }

    let object_id = system_store::put(storage_root, &bytes)?;
    let verified = system_store::get(storage_root, object_id.as_str())
        .map_err(|error| format!("saved recovery object could not be verified: {error}"))?;
    if verified != bytes {
        return Err("saved recovery object does not match the captured file".to_string());
    }

    system_catalog::append(storage_root, object_id.as_str(), &source_path, &initial_metadata)
        .map_err(|error| {
            format!(
                "recovery object {} was stored but catalog registration failed; the object was retained safely: {error}",
                object_id.as_str()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn temporary_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lifeline-capture-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn captures_and_verifies_a_regular_file() {
        let base = temporary_root("roundtrip");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let source = policy.join("example.conf");
        let contents = b"safe system configuration";
        fs::write(&source, contents).unwrap();

        let record = capture(&storage, &policy, &source).unwrap();
        assert_eq!(record.original_path, source);
        assert_eq!(record.original_size, contents.len() as u64);
        assert_eq!(system_store::get(&storage, &record.object_id).unwrap(), contents);
        assert_eq!(system_catalog::read_all(&storage).unwrap(), vec![record]);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_paths_outside_the_policy_root() {
        let base = temporary_root("outside");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let outside = base.join("outside.conf");
        fs::write(&outside, b"not allowed").unwrap();

        assert!(capture(&storage, &policy, &outside).unwrap_err().contains("outside"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_symbolic_links() {
        let base = temporary_root("symlink");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let target = policy.join("target.conf");
        let link = policy.join("link.conf");
        fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(capture(&storage, &policy, &link).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_directories_and_fifos() {
        let base = temporary_root("special");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let directory = policy.join("directory");
        fs::create_dir(&directory).unwrap();
        let fifo = policy.join("pipe");
        let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

        assert!(capture(&storage, &policy, &directory).is_err());
        assert!(capture(&storage, &policy, &fifo).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_files_larger_than_the_capture_limit() {
        let base = temporary_root("size");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let source = policy.join("large");
        let file = File::create(&source).unwrap();
        file.set_len(MAX_CAPTURE_BYTES + 1).unwrap();

        assert!(capture(&storage, &policy, &source).unwrap_err().contains("64 MiB"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn detects_source_metadata_changes() {
        let base = temporary_root("metadata");
        let source = base.join("source");
        fs::write(&source, b"before").unwrap();
        let before = fs::metadata(&source).unwrap();
        fs::write(&source, b"after with a different length").unwrap();
        let after = fs::metadata(&source).unwrap();

        assert!(!same_source_state(&before, &after));
        fs::remove_dir_all(base).unwrap();
    }
}
