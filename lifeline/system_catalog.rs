//! Append-only metadata catalog for protected system recovery objects.
//!
//! The catalog records what an object represents without embedding file content.
//! It is private to the system service and is not exposed over IPC yet.

#![allow(dead_code)] // The catalog API will be consumed by the future recovery protocol.

use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const CATALOG_FILE: &str = "catalog.log";
const RECORD_PREFIX: &str = "LFCAT001";
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRecord {
    pub object_id: String,
    pub captured_at_ns: u128,
    pub original_size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub device: u64,
    pub inode: u64,
    pub original_path: PathBuf,
}

fn catalog_path(root: &Path) -> PathBuf {
    root.join(CATALOG_FILE)
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

fn valid_object_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("catalog path contains invalid hexadecimal data".to_string());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => unreachable!(),
        };
        bytes.push((digit(pair[0]) << 4) | digit(pair[1]));
    }
    Ok(bytes)
}

fn validate_original_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("catalog paths must be absolute".to_string());
    }
    if path.as_os_str().as_bytes().len() > MAX_PATH_BYTES {
        return Err("original path exceeds the catalog path limit".to_string());
    }
    if path.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err("catalog paths must not contain parent-directory components".to_string());
    }
    Ok(())
}

fn timestamp_ns() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))
}

fn record_payload(record: &CatalogRecord) -> String {
    format!(
        "{RECORD_PREFIX}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        record.object_id,
        record.captured_at_ns,
        record.original_size,
        record.mode,
        record.uid,
        record.gid,
        record.device,
        record.inode,
        encode_hex(record.original_path.as_os_str().as_bytes())
    )
}

fn parse_record(line: &str) -> Result<CatalogRecord, String> {
    let mut fields = line.split('|');
    if fields.next() != Some(RECORD_PREFIX) {
        return Err("catalog record has an unknown format".to_string());
    }
    let object_id = fields.next().ok_or("catalog record is missing object ID")?;
    if !valid_object_id(object_id) {
        return Err("catalog record has an invalid object ID".to_string());
    }
    let captured_at_ns = fields.next().ok_or("catalog record is missing timestamp")?
        .parse::<u128>().map_err(|_| "catalog record has an invalid timestamp")?;
    let original_size = fields.next().ok_or("catalog record is missing size")?
        .parse::<u64>().map_err(|_| "catalog record has an invalid size")?;
    let mode = fields.next().ok_or("catalog record is missing mode")?
        .parse::<u32>().map_err(|_| "catalog record has an invalid mode")?;
    let uid = fields.next().ok_or("catalog record is missing UID")?
        .parse::<u32>().map_err(|_| "catalog record has an invalid UID")?;
    let gid = fields.next().ok_or("catalog record is missing GID")?
        .parse::<u32>().map_err(|_| "catalog record has an invalid GID")?;
    let device = fields.next().ok_or("catalog record is missing device")?
        .parse::<u64>().map_err(|_| "catalog record has an invalid device")?;
    let inode = fields.next().ok_or("catalog record is missing inode")?
        .parse::<u64>().map_err(|_| "catalog record has an invalid inode")?;
    let path_hex = fields.next().ok_or("catalog record is missing path")?;
    let checksum_hex = fields.next().ok_or("catalog record is missing checksum")?;
    if fields.next().is_some() {
        return Err("catalog record has unexpected fields".to_string());
    }

    let payload_end = line.rfind('|').ok_or("catalog record is incomplete")?;
    let expected_checksum = u32::from_str_radix(checksum_hex, 16)
        .map_err(|_| "catalog record has an invalid checksum")?;
    if checksum(line[..payload_end].as_bytes()) != expected_checksum {
        return Err("catalog record checksum does not match".to_string());
    }

    let path_bytes = decode_hex(path_hex)?;
    let original_path = PathBuf::from(std::ffi::OsString::from_vec(path_bytes));
    validate_original_path(&original_path)?;

    Ok(CatalogRecord {
        object_id: object_id.to_string(),
        captured_at_ns,
        original_size,
        mode,
        uid,
        gid,
        device,
        inode,
        original_path,
    })
}

fn metadata_record(object_id: &str, original_path: &Path, metadata: &Metadata) -> Result<CatalogRecord, String> {
    if !valid_object_id(object_id) {
        return Err("invalid recovery object ID".to_string());
    }
    validate_original_path(original_path)?;
    Ok(CatalogRecord {
        object_id: object_id.to_string(),
        captured_at_ns: timestamp_ns()?,
        original_size: metadata.len(),
        mode: metadata.permissions().mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
        device: metadata.dev(),
        inode: metadata.ino(),
        original_path: original_path.to_path_buf(),
    })
}

fn open_catalog(root: &Path) -> Result<File, String> {
    let path = catalog_path(root);
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true).mode(PRIVATE_FILE_MODE);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(&path)
        .map_err(|error| format!("cannot open system recovery catalog: {error}"))?;
    let metadata = file.metadata()
        .map_err(|error| format!("cannot inspect system recovery catalog: {error}"))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("system recovery catalog is not a single regular file".to_string());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("system recovery catalog has an unexpected owner".to_string());
    }
    if metadata.permissions().mode() & 0o777 != PRIVATE_FILE_MODE {
        return Err("system recovery catalog must have mode 0600".to_string());
    }
    if metadata.len() > MAX_CATALOG_BYTES {
        return Err("system recovery catalog reached its size limit".to_string());
    }
    Ok(file)
}

/// Append durable metadata for an object after its content has been stored.
pub fn append(root: &Path, object_id: &str, original_path: &Path, metadata: &Metadata) -> Result<CatalogRecord, String> {
    let record = metadata_record(object_id, original_path, metadata)?;
    let payload = record_payload(&record);
    let line = format!("{payload}|{:08x}\n", checksum(payload.as_bytes()));

    let mut file = open_catalog(root)?;
    let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if lock_result != 0 {
        return Err(format!("cannot lock system recovery catalog: {}", std::io::Error::last_os_error()));
    }

    let result = (|| -> Result<(), String> {
        let current_size = file.metadata()
            .map_err(|error| format!("cannot inspect catalog size: {error}"))?.len();
        if current_size.saturating_add(line.len() as u64) > MAX_CATALOG_BYTES {
            return Err("system recovery catalog reached its size limit".to_string());
        }
        file.write_all(line.as_bytes())
            .map_err(|error| format!("cannot append system recovery catalog: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync system recovery catalog: {error}"))?;
        Ok(())
    })();

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN); }
    result?;
    Ok(record)
}

/// Read and validate every complete catalog record. A partial final line is
/// ignored because a crash may interrupt the final append; malformed complete
/// records fail closed instead of silently hiding catalog damage.
pub fn read_all(root: &Path) -> Result<Vec<CatalogRecord>, String> {
    let path = catalog_path(root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot inspect system recovery catalog: {error}")),
    };
    if metadata.file_type().is_symlink() {
        return Err("system recovery catalog must not be a symbolic link".to_string());
    }

    let mut file = open_catalog(root)?;
    let mut bytes = Vec::new();
    file.take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read system recovery catalog: {error}"))?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err("system recovery catalog exceeds its size limit".to_string());
    }

    let mut records = Vec::new();
    let mut lines = bytes.split(|byte| *byte == b'\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        if lines.peek().is_none() && !bytes.ends_with(b"\n") {
            break;
        }
        let line = std::str::from_utf8(line)
            .map_err(|_| "system recovery catalog contains invalid text".to_string())?;
        records.push(parse_record(line)?);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary_store(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lifeline-catalog-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn appends_and_reads_catalog_metadata() {
        let root = temporary_store("roundtrip");
        let source = root.join("example.conf");
        fs::write(&source, b"configuration").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let record = append(&root, "0123456789abcdef0123456789abcdef", &source, &metadata).unwrap();

        let records = read_all(&root).unwrap();
        assert_eq!(records, vec![record]);
        assert_eq!(records[0].original_path, source);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_relative_and_parent_traversal_paths() {
        assert!(validate_original_path(Path::new("etc/example.conf")).is_err());
        assert!(validate_original_path(Path::new("/etc/../shadow")).is_err());
    }

    #[test]
    fn rejects_corrupt_complete_records() {
        let root = temporary_store("corrupt");
        let source = root.join("example");
        fs::write(&source, b"x").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        append(&root, "0123456789abcdef0123456789abcdef", &source, &metadata).unwrap();

        let catalog = catalog_path(&root);
        let mut contents = fs::read(&catalog).unwrap();
        let index = contents.iter().position(|byte| *byte == b'|').unwrap();
        contents[index + 1] ^= 1;
        fs::write(&catalog, contents).unwrap();

        assert!(read_all(&root).unwrap_err().contains("checksum"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_a_partial_final_record() {
        let root = temporary_store("partial");
        let source = root.join("example");
        fs::write(&source, b"x").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let record = append(&root, "0123456789abcdef0123456789abcdef", &source, &metadata).unwrap();
        use std::io::Write as _;
        OpenOptions::new().append(true).open(catalog_path(&root)).unwrap()
            .write_all(b"LFCAT001|incomplete").unwrap();

        assert_eq!(read_all(&root).unwrap(), vec![record]);
        fs::remove_dir_all(root).unwrap();
    }
}
