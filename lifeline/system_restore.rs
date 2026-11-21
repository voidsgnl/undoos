//! Crash-recoverable restore adapter for the single approved system file.
//!
//! Restoration writes the existing regular file in place so its inode,
//! ownership, mode, and hard-link identity remain unchanged. A durable pending
//! record points to a freshly captured pre-restore object. If the service is
//! interrupted during the write, startup restores that preimage before
//! accepting another request.
#![allow(dead_code)]

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}},
    path::{Path, PathBuf},
};

use super::{system_capture, system_catalog, system_store};

const TARGET: &str = "/etc/hostname";
const PENDING_NAME: &str = "hostname-restore.pending";
const PENDING_PREFIX: &str = "LFRST001";
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_NAME)
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn open_target() -> Result<File, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(TARGET)
        .map_err(|error| format!("cannot safely open {TARGET}: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {TARGET}: {error}"))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(format!("{TARGET} must be a single regular file"));
    }
    if metadata.uid() != 0 {
        return Err(format!("{TARGET} must be owned by root"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("{TARGET} exceeds the 64 MiB restore limit"));
    }
    let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if lock_result != 0 {
        return Err(format!(
            "cannot obtain an exclusive restore lock for {TARGET}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

fn same_file_generation(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.permissions().mode() & 0o7777 == right.permissions().mode() & 0o7777
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

fn write_target(
    file: &mut File,
    expected: &system_catalog::CatalogRecord,
    bytes: &[u8],
    expected_live: Option<&[u8]>,
) -> Result<(), String> {
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes.len() as u64 != expected.original_size {
        return Err("restore content length does not match its catalog record".to_string());
    }
    let before = file
        .metadata()
        .map_err(|error| format!("cannot inspect {TARGET} before restore: {error}"))?;
    if before.dev() != expected.device
        || before.ino() != expected.inode
        || before.uid() != expected.uid
        || before.gid() != expected.gid
        || before.permissions().mode() & 0o7777 != expected.mode
        || !before.is_file()
        || before.nlink() != 1
    {
        return Err(format!(
            "{TARGET} no longer matches the captured file identity and metadata; refusing to overwrite it"
        ));
    }

    // When beginning a user-requested restore, verify that the live file
    // still contains the exact preimage captured immediately beforehand. This
    // prevents overwriting edits that happened while the safety snapshot was
    // being created. Startup rollback deliberately omits this check because
    // the file may contain a partial write from the interrupted operation.
    if let Some(expected_live) = expected_live {
        if before.len() != expected_live.len() as u64 {
            return Err(format!(
                "{TARGET} changed after its safety snapshot; refusing to overwrite it"
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot inspect current {TARGET} contents: {error}"))?;
        let mut actual = Vec::with_capacity(expected_live.len());
        (&mut *file)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut actual)
            .map_err(|error| format!("cannot verify current {TARGET} contents: {error}"))?;
        if actual != expected_live {
            return Err(format!(
                "{TARGET} changed after its safety snapshot; refusing to overwrite it"
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot seek {TARGET} for restore: {error}"))?;
    }

    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| file.write_all(bytes))
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot write and sync {TARGET}: {error}"))?;

    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot verify restored {TARGET}: {error}"))?;
    let mut actual = Vec::with_capacity(bytes.len());
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut actual)
        .map_err(|error| format!("cannot read back restored {TARGET}: {error}"))?;
    if actual != bytes {
        return Err(format!(
            "restored {TARGET} does not match the selected recovery object"
        ));
    }
    Ok(())
}

fn write_pending(root: &Path, backup_id: &str) -> Result<(), String> {
    if backup_id.len() != 32
        || !backup_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid pre-restore recovery object ID".to_string());
    }
    let payload = format!("{PENDING_PREFIX}|{backup_id}");
    let line = format!("{payload}|{:08x}\n", checksum(payload.as_bytes()));
    let path = pending_path(root);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = options
        .open(&path)
        .map_err(|error| format!("cannot create restore transaction record: {error}"))?;
    file.write_all(line.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot persist restore transaction record: {error}"))?;
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync restore transaction directory: {error}"))
}

fn read_pending(root: &Path) -> Result<Option<String>, String> {
    let path = pending_path(root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect restore transaction record: {error}")),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > 256
    {
        return Err("restore transaction record has unsafe metadata".to_string());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("cannot open restore transaction record: {error}"))?;
    let mut line = String::new();
    file.read_to_string(&mut line)
        .map_err(|error| format!("cannot read restore transaction record: {error}"))?;
    let line = line.strip_suffix('\n').ok_or("restore transaction record is incomplete")?;
    let fields = line.split('|').collect::<Vec<_>>();
    if fields.len() != 3 || fields[0] != PENDING_PREFIX {
        return Err("restore transaction record has an unknown format".to_string());
    }
    let id = fields[1];
    if id.len() != 32
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("restore transaction record has an invalid object ID".to_string());
    }
    let payload = format!("{PENDING_PREFIX}|{id}");
    let expected = u32::from_str_radix(fields[2], 16)
        .map_err(|_| "restore transaction record has an invalid checksum")?;
    if checksum(payload.as_bytes()) != expected {
        return Err("restore transaction record checksum does not match".to_string());
    }
    Ok(Some(id.to_string()))
}

fn remove_pending(root: &Path) -> Result<(), String> {
    fs::remove_file(pending_path(root))
        .map_err(|error| format!("cannot clear completed restore transaction: {error}"))?;
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync completed restore transaction: {error}"))
}

fn backup_record(root: &Path, backup_id: &str) -> Result<system_catalog::CatalogRecord, String> {
    system_catalog::read_all(root)?
        .into_iter()
        .find(|record| {
            record.object_id == backup_id && record.original_path == Path::new(TARGET)
        })
        .ok_or_else(|| "pre-restore recovery object has no matching hostname catalog record".to_string())
}

fn restore_backup(root: &Path, backup_id: &str) -> Result<(), String> {
    let record = backup_record(root, backup_id)?;
    let bytes = system_store::get(root, backup_id)?;
    let mut target = open_target()?;
    write_target(&mut target, &record, &bytes, None)
}

/// Roll back an interrupted hostname restore before the service accepts IPC.
pub fn recover_interrupted_restore(root: &Path) -> Result<(), String> {
    let Some(backup_id) = read_pending(root)? else {
        return Ok(());
    };
    restore_backup(root, &backup_id)?;
    remove_pending(root)?;
    eprintln!("lifeline-systemd: recovered interrupted /etc/hostname restore from object {backup_id}");
    Ok(())
}

/// Restore one catalogued hostname version, preserving a durable preimage so
/// an interrupted in-place write is rolled back on the next service start.
pub fn restore_hostname(root: &Path, object_id: &str) -> Result<String, String> {
    let selected = backup_record(root, object_id)?;
    let selected_bytes = system_store::get(root, object_id)?;
    if selected_bytes.len() as u64 != selected.original_size {
        return Err("selected recovery object length does not match its catalog record".to_string());
    }

    let mut target = open_target()?;
    let current_metadata = target
        .metadata()
        .map_err(|error| format!("cannot inspect current {TARGET}: {error}"))?;

    let policy = system_capture::CapturePolicy::new(
        Path::new("/etc"),
        vec![PathBuf::from("hostname")],
    )?;
    let backup = system_capture::capture(root, &policy, Path::new("hostname"))?;
    let after_capture = target
        .metadata()
        .map_err(|error| format!("cannot recheck current {TARGET}: {error}"))?;
    if !same_file_generation(&current_metadata, &after_capture)
        || backup.device != current_metadata.dev()
        || backup.inode != current_metadata.ino()
    {
        return Err(format!(
            "{TARGET} changed while its pre-restore recovery point was being captured"
        ));
    }

    let backup_bytes = system_store::get(root, &backup.object_id)?;
    if backup_bytes.len() as u64 != backup.original_size {
        return Err("pre-restore recovery object length does not match its catalog record".to_string());
    }

    write_pending(root, &backup.object_id)?;
    if let Err(error) = write_target(
        &mut target,
        &selected,
        &selected_bytes,
        Some(&backup_bytes),
    ) {
        match restore_backup(root, &backup.object_id) {
            Ok(()) => {
                remove_pending(root)?;
                return Err(format!("restore failed; the previous hostname was recovered: {error}"));
            }
            Err(rollback_error) => {
                return Err(format!(
                    "restore failed ({error}); automatic rollback also failed ({rollback_error}). The durable recovery record was retained for service-startup recovery."
                ));
            }
        }
    }
    remove_pending(root)?;
    Ok(backup.object_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lifeline-system-restore-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn persists_reads_and_clears_a_pending_restore_record() {
        let root = temporary_root("pending-roundtrip");
        let object_id = "0123456789abcdef0123456789abcdef";
        write_pending(&root, object_id).unwrap();
        assert_eq!(read_pending(&root).unwrap().as_deref(), Some(object_id));
        remove_pending(&root).unwrap();
        assert_eq!(read_pending(&root).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_a_corrupt_pending_restore_record() {
        let root = temporary_root("pending-corrupt");
        write_pending(&root, "0123456789abcdef0123456789abcdef").unwrap();
        fs::write(pending_path(&root), b"LFRST001|0123456789abcdef0123456789abcdef|00000000\\n")
            .unwrap();
        assert!(read_pending(&root)
            .unwrap_err()
            .contains("checksum does not match"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_invalid_pending_object_ids() {
        let root = temporary_root("pending-invalid-id");
        assert!(write_pending(&root, "../etc/passwd").is_err());
        assert!(!pending_path(&root).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
