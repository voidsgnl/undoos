//! Immutable, integrity-checked objects for protected system recovery data.
//!
//! Objects are written to a temporary file, synced, and linked into place
//! without replacing an existing object. The store is not exposed over IPC yet.

#![allow(dead_code)] // The object API will be consumed by the typed recovery protocol.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const OBJECT_DIRECTORY: &str = "objects";
const OBJECT_MAGIC: &[u8; 8] = b"LFOBJ001";
const HEADER_LENGTH: usize = 20;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const ID_BYTES: usize = 16;
const MAX_CREATE_ATTEMPTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn generate() -> Result<Self, String> {
        let mut bytes = [0_u8; ID_BYTES];
        let mut filled = 0;
        while filled < bytes.len() {
            let result = unsafe {
                libc::getrandom(
                    bytes[filled..].as_mut_ptr() as *mut libc::c_void,
                    bytes.len() - filled,
                    0,
                )
            };
            if result > 0 {
                filled += result as usize;
            } else if result == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            } else {
                return Err(format!(
                    "cannot generate a recovery object ID: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        let mut value = String::with_capacity(ID_BYTES * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(Self(value))
    }

    fn parse(value: &str) -> Result<Self, String> {
        if value.len() != ID_BYTES * 2
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid recovery object ID".to_string());
        }
        Ok(Self(value.to_string()))
    }
}

fn objects_directory(root: &Path) -> PathBuf {
    root.join(OBJECT_DIRECTORY)
}

fn verify_private_directory(path: &Path, expected_uid: u32) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{} is not a real directory", path.display()));
    }
    if metadata.uid() != expected_uid {
        return Err(format!("{} has an unexpected owner", path.display()));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != PRIVATE_DIRECTORY_MODE {
        return Err(format!(
            "{} has mode {:04o}, expected {:04o}",
            path.display(),
            mode,
            PRIVATE_DIRECTORY_MODE
        ));
    }
    Ok(())
}

/// Create the private object directory if absent, then verify its boundary.
pub fn initialize(root: &Path) -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    verify_private_directory(root, uid)?;

    let objects = objects_directory(root);
    match fs::create_dir(&objects) {
        Ok(()) => {
            fs::set_permissions(&objects, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
                .map_err(|error| format!("cannot secure {}: {error}", objects.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("cannot create {}: {error}", objects.display()));
        }
    }
    verify_private_directory(&objects, uid)
}

/// A read-only inventory of published objects and interrupted temporary files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreInventory {
    pub object_ids: Vec<String>,
    pub temporary_ids: Vec<String>,
}

/// Enumerate the object directory without deleting or modifying anything.
/// Unknown names, symlinks, and unsafe file metadata fail closed.
pub fn inventory(root: &Path) -> Result<StoreInventory, String> {
    initialize(root)?;
    let directory = objects_directory(root);
    let expected_uid = unsafe { libc::geteuid() };
    let mut inventory = StoreInventory::default();

    for entry in fs::read_dir(&directory)
        .map_err(|error| format!("cannot enumerate recovery objects: {error}"))?
    {
        let entry = entry.map_err(|error| format!("cannot read recovery object entry: {error}"))?;
        let name = entry.file_name();
        let name = name.to_str()
            .ok_or_else(|| "recovery object directory contains a non-UTF-8 entry name".to_string())?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect recovery object entry: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("recovery object entry {name} is not a regular file"));
        }
        if metadata.uid() != expected_uid
            || metadata.permissions().mode() & 0o777 != PRIVATE_FILE_MODE
        {
            return Err(format!("recovery object entry {name} has unsafe ownership or permissions"));
        }

        if let Ok(id) = ObjectId::parse(name) {
            if metadata.len() < HEADER_LENGTH as u64
                || metadata.len() > MAX_OBJECT_BYTES + HEADER_LENGTH as u64
            {
                return Err(format!("recovery object {name} has an invalid size"));
            }
            inventory.object_ids.push(id.as_str().to_string());
        } else if let Some(value) = name.strip_prefix(".tmp-") {
            let id = ObjectId::parse(value)
                .map_err(|_| format!("unknown entry in recovery object directory: {name}"))?;
            inventory.temporary_ids.push(id.as_str().to_string());
        } else {
            return Err(format!("unknown entry in recovery object directory: {name}"));
        }
    }

    inventory.object_ids.sort();
    inventory.temporary_ids.sort();
    Ok(inventory)
}

/// CRC-32/ISO-HDLC detects accidental corruption of stored recovery bytes.
/// Root-only ownership and permissions provide the access-control boundary.
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

fn validate_object_length(length: u64) -> Result<(), String> {
    if length > MAX_OBJECT_BYTES {
        return Err("recovery object exceeds the 1 GiB storage limit".to_string());
    }
    Ok(())
}

/// Store one immutable recovery object and return its opaque identifier.
pub fn put(root: &Path, bytes: &[u8]) -> Result<ObjectId, String> {
    validate_object_length(bytes.len() as u64)?;
    initialize(root)?;
    let directory = objects_directory(root);

    for _ in 0..MAX_CREATE_ATTEMPTS {
        let id = ObjectId::generate()?;
        let final_path = directory.join(id.as_str());
        let temporary_path = directory.join(format!(".tmp-{}", id.as_str()));

        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(PRIVATE_FILE_MODE);
        let mut file = match options.open(&temporary_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("cannot create recovery object: {error}"));
            }
        };
        if let Err(error) = file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE)) {
            let _ = fs::remove_file(&temporary_path);
            return Err(format!("cannot secure recovery object: {error}"));
        }

        let write_result = (|| -> std::io::Result<()> {
            file.write_all(OBJECT_MAGIC)?;
            file.write_all(&(bytes.len() as u64).to_le_bytes())?;
            file.write_all(&checksum(bytes).to_le_bytes())?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        drop(file);

        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary_path);
            return Err(format!("cannot write recovery object: {error}"));
        }

        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => {
                fs::remove_file(&temporary_path)
                    .map_err(|error| format!("cannot finalize recovery object: {error}"))?;
                File::open(&directory)
                    .and_then(|directory_file| directory_file.sync_all())
                    .map_err(|error| format!("cannot sync recovery object directory: {error}"))?;
                return Ok(id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary_path);
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary_path);
                return Err(format!("cannot publish recovery object: {error}"));
            }
        }
    }

    Err("could not allocate a unique recovery object ID".to_string())
}

/// Load an object only when its file type, owner, mode, length, and checksum
/// all match the format written by this module.
pub fn get(root: &Path, value: &str) -> Result<Vec<u8>, String> {
    initialize(root)?;
    let id = ObjectId::parse(value)?;
    let path = objects_directory(root).join(id.as_str());
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect recovery object: {error}"))?;

    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("recovery object is not a regular file".to_string());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("recovery object has an unexpected owner".to_string());
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != PRIVATE_FILE_MODE {
        return Err(format!("recovery object has unsafe mode {:04o}", mode));
    }
    if metadata.len() < HEADER_LENGTH as u64
        || metadata.len() > MAX_OBJECT_BYTES + HEADER_LENGTH as u64
    {
        return Err("recovery object has an invalid size".to_string());
    }

    let mut file = File::open(&path)
        .map_err(|error| format!("cannot open recovery object: {error}"))?;
    let mut header = [0_u8; HEADER_LENGTH];
    file.read_exact(&mut header)
        .map_err(|error| format!("cannot read recovery object header: {error}"))?;
    if &header[..8] != OBJECT_MAGIC {
        return Err("recovery object has an unknown format".to_string());
    }

    let expected_length = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let expected_checksum = u32::from_le_bytes(header[16..20].try_into().unwrap());
    if expected_length != metadata.len() - HEADER_LENGTH as u64 {
        return Err("recovery object length does not match its header".to_string());
    }

    let capacity = usize::try_from(expected_length)
        .map_err(|_| "recovery object is too large for this system".to_string())?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read recovery object: {error}"))?;
    if bytes.len() != capacity {
        return Err("recovery object was truncated while reading".to_string());
    }
    if checksum(&bytes) != expected_checksum {
        return Err("recovery object integrity check failed".to_string());
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_store(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lifeline-object-store-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn stores_and_reads_an_immutable_object() {
        let root = temporary_store("roundtrip");
        let original = b"system configuration before change";
        let id = put(&root, original).unwrap();

        assert_eq!(get(&root, id.as_str()).unwrap(), original);
        assert!(put(&root, b"new state").is_ok());
        assert_eq!(get(&root, id.as_str()).unwrap(), original);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_object_identifiers() {
        let root = temporary_store("invalid-id");
        initialize(&root).unwrap();
        assert!(get(&root, "../outside").is_err());
        assert!(get(&root, "ABCDEF").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_object_corruption() {
        let root = temporary_store("corruption");
        let id = put(&root, b"original bytes").unwrap();
        let object = objects_directory(&root).join(id.as_str());
        let mut bytes = fs::read(&object).unwrap();
        *bytes.last_mut().unwrap() ^= 0xff;
        fs::write(&object, bytes).unwrap();

        assert!(get(&root, id.as_str())
            .unwrap_err()
            .contains("integrity check failed"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_symlinked_object_directory() {
        let root = temporary_store("symlink-directory");
        let target = temporary_store("symlink-target");
        std::os::unix::fs::symlink(&target, objects_directory(&root)).unwrap();

        assert!(initialize(&root).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn rejects_object_lengths_larger_than_the_limit() {
        assert!(validate_object_length(MAX_OBJECT_BYTES + 1).is_err());
        assert!(validate_object_length(MAX_OBJECT_BYTES).is_ok());
    }
}
