//! Durable deletion events for the narrowly scoped system recovery observer.
//!
//! Deletion events reference the last known content object. They do not create
//! empty-file snapshots and are stored separately to preserve LFCAT001 history.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},

};

const LOG_NAME: &str = "deletions.log";
const PREFIX: &str = "LFDEL002";
const LEGACY_PREFIX: &str = "LFDEL001";
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionRecord {
    pub event_id: String,
    pub deleted_at_ns: u128,
    pub previous_object_id: String,
}

fn path(root: &Path) -> PathBuf {
    root.join(LOG_NAME)
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

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_line(line: &str) -> Result<DeletionRecord, String> {
    let fields = line.split('|').collect::<Vec<_>>();
    let (event_id, timestamp_field, object_field, checksum_field, payload) = match fields.as_slice() {
        [prefix, timestamp, object_id, checksum_field] if *prefix == LEGACY_PREFIX => (
            (*object_id).to_string(), *timestamp, *object_id, *checksum_field,
            format!("{LEGACY_PREFIX}|{timestamp}|{object_id}"),
        ),
        [prefix, event_id, timestamp, object_id, checksum_field] if *prefix == PREFIX && valid_id(event_id) => (
            (*event_id).to_string(), *timestamp, *object_id, *checksum_field,
            format!("{PREFIX}|{event_id}|{timestamp}|{object_id}"),
        ),
        _ => return Err("deletion record has an unknown format".to_string()),
    };
    let timestamp = timestamp_field.parse::<u128>()
        .map_err(|_| "deletion record has an invalid timestamp")?;
    if !valid_id(object_field) {
        return Err("deletion record has an invalid preceding object ID".to_string());
    }
    let expected = u32::from_str_radix(checksum_field, 16)
        .map_err(|_| "deletion record has an invalid checksum")?;
    if checksum(payload.as_bytes()) != expected {
        return Err("deletion record checksum does not match".to_string());
    }
    Ok(DeletionRecord {
        event_id,
        deleted_at_ns: timestamp,
        previous_object_id: object_field.to_string(),
    })
}

fn read_locked(file: &mut File) -> Result<Vec<DeletionRecord>, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek deletion history: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read deletion history: {error}"))?;
    if bytes.len() as u64 > MAX_LOG_BYTES {
        return Err("deletion history exceeds its size limit".to_string());
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err("deletion history has an incomplete final record".to_string());
    }
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let line = std::str::from_utf8(line)
            .map_err(|_| "deletion history contains invalid text")?;
        records.push(parse_line(line)?);
    }
    Ok(records)
}

fn open_log(root: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true).mode(0o600);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options
        .open(path(root))
        .map_err(|error| format!("cannot open deletion history: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect deletion history: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > MAX_LOG_BYTES
    {
        return Err("deletion history has unsafe metadata".to_string());
    }
    Ok(file)
}

pub fn read_all(root: &Path) -> Result<Vec<DeletionRecord>, String> {
    let log_path = path(root);
    match fs::symlink_metadata(&log_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot inspect deletion history: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("deletion history must not be a symbolic link".to_string())
        }
        Ok(_) => {}
    }
    let mut file = open_log(root)?;
    let lock = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
    if lock != 0 {
        return Err(format!("cannot lock deletion history: {}", std::io::Error::last_os_error()));
    }
    let result = read_locked(&mut file);
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN); }
    result
}

pub fn append(root: &Path, previous_object_id: &str) -> Result<DeletionRecord, String> {
    if !valid_id(previous_object_id) {
        return Err("invalid preceding recovery object ID".to_string());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_nanos();
    let mut random_id = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut random_id))
        .map_err(|error| format!("cannot generate a deletion event ID: {error}"))?;
    let event_id = random_id.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let payload = format!("{PREFIX}|{event_id}|{timestamp}|{previous_object_id}");
    let line = format!("{payload}|{:08x}\n", checksum(payload.as_bytes()));
    let mut file = open_log(root)?;
    let lock = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if lock != 0 {
        return Err(format!("cannot lock deletion history: {}", std::io::Error::last_os_error()));
    }
    let result = (|| {
        let records = read_locked(&mut file)?;
        if records.last().is_some_and(|record| record.previous_object_id == previous_object_id) {
            // Repeated delete notifications must not create duplicate tombstones.
            return Ok(records.last().cloned().expect("last record exists"));
        }
        let end = file.seek(SeekFrom::End(0))
            .map_err(|error| format!("cannot seek deletion history: {error}"))?;
        if end.saturating_add(line.len() as u64) > MAX_LOG_BYTES {
            return Err("deletion history reached its size limit".to_string());
        }
        file.write_all(line.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist deletion event: {error}"))?;
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync deletion history directory: {error}"))?;
        Ok(DeletionRecord { event_id, deleted_at_ns: timestamp, previous_object_id: previous_object_id.to_string() })
    })();
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("lifeline-deletions-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn appends_and_reads_a_checksummed_deletion_event() {
        let root = root("roundtrip");
        let id = "0123456789abcdef0123456789abcdef";
        let event = append(&root, id).unwrap();
        assert_eq!(read_all(&root).unwrap(), vec![event.clone()]);
        assert_eq!(event.event_id.len(), 32);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_corrupt_deletion_records() {
        let root = root("corrupt");
        append(&root, "0123456789abcdef0123456789abcdef").unwrap();
        fs::write(path(&root), b"LFDEL001|1|0123456789abcdef0123456789abcdef|00000000\n").unwrap();
        assert!(read_all(&root).unwrap_err().contains("checksum"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_preceding_object_ids() {
        let root = root("invalid");
        assert!(append(&root, "../etc/passwd").is_err());
        assert!(!path(&root).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
