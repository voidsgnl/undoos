//! Narrow, internal system-file capture coordinator.
//!
//! Capture is restricted to an explicitly supplied policy root and a
//! relative path beneath it. Every path component is opened relative to an
//! already-open directory descriptor with O_NOFOLLOW, preventing symlink
//! traversal and path-prefix races. This module is not exposed over IPC.

#![allow(dead_code)]

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

use super::{system_catalog, system_store};

const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

/// Exact-path policy for the internal capture adapter. The root is resolved
/// once when the policy is built; only listed relative paths can be captured.
#[derive(Clone, Debug)]
pub struct CapturePolicy {
    root: PathBuf,
    allowed_paths: Vec<PathBuf>,
}

impl CapturePolicy {
    pub fn new(root: &Path, allowed_paths: Vec<PathBuf>) -> Result<Self, String> {
        let root = fs::canonicalize(root)
            .map_err(|error| format!("cannot resolve capture policy root: {error}"))?;
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| format!("cannot inspect capture policy root: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("capture policy root must be a real directory".to_string());
        }
        if allowed_paths.is_empty() {
            return Err("capture policy must contain at least one allowed path".to_string());
        }
        for path in &allowed_paths {
            validate_relative_path(path)?;
        }
        Ok(Self {
            root,
            allowed_paths,
        })
    }

    fn permits(&self, path: &Path) -> bool {
        self.allowed_paths.iter().any(|allowed| allowed == path)
    }
}

fn validate_relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("capture path must be a non-empty relative path".to_string());
    }
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("capture path must contain only ordinary relative path components".to_string());
    }
    Ok(())
}

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

fn component_name(component: &std::ffi::OsStr) -> Result<CString, String> {
    CString::new(component.as_bytes()).map_err(|_| "capture path contains a NUL byte".to_string())
}

fn open_directory_at(parent: &File, name: &std::ffi::OsStr) -> Result<File, String> {
    let name = component_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot safely open a capture path directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Open a relative path without following symlinks in any component.
/// The policy root is resolved once and then held open for the full traversal.
fn open_source(
    allowed_root: &Path,
    relative_path: &Path,
) -> Result<(PathBuf, File, fs::Metadata), String> {
    if relative_path.as_os_str().is_empty() || relative_path.is_absolute() {
        return Err("capture path must be a non-empty relative path".to_string());
    }

    let components = relative_path.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("capture path must contain only ordinary relative path components".to_string());
    }

    validate_relative_path(relative_path)?;
    let root = allowed_root;
    let mut root_options = OpenOptions::new();
    root_options.read(true);
    root_options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY);
    let mut current = root_options
        .open(&root)
        .map_err(|error| format!("cannot safely open capture policy root: {error}"))?;
    if !current
        .metadata()
        .map_err(|error| format!("cannot inspect capture policy root: {error}"))?
        .is_dir()
    {
        return Err("capture policy root is not a directory".to_string());
    }

    for component in &components[..components.len() - 1] {
        let Component::Normal(name) = component else {
            return Err("capture path contains an unsupported component".to_string());
        };
        current = open_directory_at(&current, name)?;
    }

    let Component::Normal(final_name) = components[components.len() - 1] else {
        return Err("capture path has an invalid final component".to_string());
    };
    let final_name = component_name(final_name)?;
    let fd = unsafe {
        libc::openat(
            current.as_raw_fd(),
            final_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot safely open requested system file: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect opened system file: {error}"))?;
    if !metadata.is_file() {
        return Err("only regular files can be captured".to_string());
    }
    if metadata.len() > MAX_CAPTURE_BYTES {
        return Err("system file exceeds the 64 MiB capture limit".to_string());
    }

    Ok((root.join(relative_path), file, metadata))
}

/// Capture one regular file beneath the explicitly supplied policy root.
///
/// If catalog registration fails after the object is durable, the object is
/// deliberately retained as an unreferenced orphan. Retention is safer than
/// deletion when the catalog's durability outcome is uncertain.
pub fn capture(
    storage_root: &Path,
    policy: &CapturePolicy,
    relative_path: &Path,
) -> Result<system_catalog::CatalogRecord, String> {
    if !policy.permits(relative_path) {
        return Err("requested file is not listed in the capture policy".to_string());
    }
    let (source_path, mut file, initial_metadata) = open_source(&policy.root, relative_path)?;

    let mut bytes = Vec::with_capacity(initial_metadata.len() as usize);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek system file: {error}"))?;
    (&mut file)
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read system file: {error}"))?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err("system file grew beyond the 64 MiB capture limit".to_string());
    }

    let final_metadata = file
        .metadata()
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
        let root =
            std::env::temp_dir().join(format!("lifeline-capture-{label}-{}", std::process::id()));
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
        fs::set_permissions(&storage, fs::Permissions::from_mode(0o700)).unwrap();
        let source = policy.join("example.conf");
        let contents = b"safe system configuration";
        fs::write(&source, contents).unwrap();

        let capture_policy =
            CapturePolicy::new(&policy, vec![PathBuf::from("example.conf")]).unwrap();
        let record = capture(&storage, &capture_policy, Path::new("example.conf")).unwrap();
        assert_eq!(record.original_path, source);
        assert_eq!(record.original_size, contents.len() as u64);
        assert_eq!(
            system_store::get(&storage, &record.object_id).unwrap(),
            contents
        );
        assert_eq!(system_catalog::read_all(&storage).unwrap(), vec![record]);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_absolute_and_parent_traversal_policy_entries() {
        assert!(validate_relative_path(Path::new("/etc/passwd")).is_err());
        assert!(validate_relative_path(Path::new("../etc/passwd")).is_err());
    }

    #[test]
    fn refuses_files_not_explicitly_allowlisted() {
        let base = temporary_root("not-allowlisted");
        let policy_root = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy_root).unwrap();
        fs::create_dir(&storage).unwrap();
        fs::write(policy_root.join("other.conf"), b"not selected").unwrap();
        let policy =
            CapturePolicy::new(&policy_root, vec![PathBuf::from("approved.conf")]).unwrap();

        assert!(capture(&storage, &policy, Path::new("other.conf"))
            .unwrap_err()
            .contains("not listed"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_symbolic_links_in_every_path_component() {
        let base = temporary_root("symlink");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        let real_dir = policy.join("real");
        fs::create_dir(&real_dir).unwrap();
        let target = real_dir.join("target.conf");
        fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&real_dir, policy.join("linked-dir")).unwrap();
        std::os::unix::fs::symlink(&target, policy.join("linked-file")).unwrap();

        let capture_policy = CapturePolicy::new(
            &policy,
            vec![
                PathBuf::from("linked-dir/target.conf"),
                PathBuf::from("linked-file"),
            ],
        )
        .unwrap();
        assert!(capture(
            &storage,
            &capture_policy,
            Path::new("linked-dir/target.conf")
        )
        .is_err());
        assert!(capture(&storage, &capture_policy, Path::new("linked-file")).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_directories_and_fifos_without_blocking() {
        let base = temporary_root("special");
        let policy = base.join("allowed");
        let storage = base.join("storage");
        fs::create_dir(&policy).unwrap();
        fs::create_dir(&storage).unwrap();
        fs::create_dir(policy.join("directory")).unwrap();
        let fifo = policy.join("pipe");
        let fifo_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

        let capture_policy = CapturePolicy::new(
            &policy,
            vec![PathBuf::from("directory"), PathBuf::from("pipe")],
        )
        .unwrap();
        assert!(capture(&storage, &capture_policy, Path::new("directory")).is_err());
        assert!(capture(&storage, &capture_policy, Path::new("pipe")).is_err());
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

        let capture_policy = CapturePolicy::new(&policy, vec![PathBuf::from("large")]).unwrap();
        assert!(capture(&storage, &capture_policy, Path::new("large"))
            .unwrap_err()
            .contains("64 MiB"));
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
