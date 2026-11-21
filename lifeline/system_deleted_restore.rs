//! Crash-safe recreation of the approved hostname file after a recorded deletion.
//!
//! A durable transaction records the event and staging name before any file is
//! created. Once the staged inode is durable, its device/inode identity is
//! recorded before a no-replace rename publishes it at /etc/hostname. Startup
//! rollback removes only that exact inode, never a file recreated by another
//! process.

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use super::{system_catalog, system_deletion, system_store};

const TARGET: &str = "hostname";
const PENDING_NAME: &str = "hostname-create.pending";
const PENDING_PREFIX: &str = "LFCRE001";
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    event_id: String,
    object_id: String,
    staged: bool,
    device: u64,
    inode: u64,
}

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_NAME)
}

fn stage_name(event_id: &str) -> String {
    format!(".lifeline-hostname-restore-{event_id}")
}

fn encode_pending(pending: &Pending) -> String {
    let phase = if pending.staged { "S" } else { "P" };
    let payload = format!(
        "{PENDING_PREFIX}|{}|{}|{phase}|{}|{}",
        pending.event_id, pending.object_id, pending.device, pending.inode
    );
    format!("{payload}|{:08x}\n", checksum(payload.as_bytes()))
}

fn write_pending(root: &Path, pending: &Pending) -> Result<(), String> {
    let path = pending_path(root);
    let temporary = root.join(".hostname-create.pending.tmp");
    if let Ok(metadata) = fs::symlink_metadata(&temporary) {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err("temporary deletion-restore journal has unsafe metadata".to_string());
        }
        fs::remove_file(&temporary)
            .map_err(|error| format!("cannot clear stale journal temporary: {error}"))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = options.open(&temporary)
        .map_err(|error| format!("cannot create deletion-restore journal temporary: {error}"))?;
    file.write_all(encode_pending(pending).as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot persist deletion-restore journal: {error}"))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("cannot publish deletion-restore journal: {error}"))?;
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync deletion-restore journal directory: {error}"))
}

fn read_pending(root: &Path) -> Result<Option<Pending>, String> {
    let path = pending_path(root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect deletion-restore journal: {error}")),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > 256
    {
        return Err("deletion-restore journal has unsafe metadata".to_string());
    }
    let mut file = OpenOptions::new().read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("cannot open deletion-restore journal: {error}"))?;
    let mut line = String::new();
    file.read_to_string(&mut line)
        .map_err(|error| format!("cannot read deletion-restore journal: {error}"))?;
    let line = line.strip_suffix('\n').ok_or("deletion-restore journal is incomplete")?;
    let fields = line.split('|').collect::<Vec<_>>();
    if fields.len() != 7 || fields[0] != PENDING_PREFIX
        || !valid_id(fields[1]) || !valid_id(fields[2])
    {
        return Err("deletion-restore journal has an invalid format".to_string());
    }
    let payload_end = line.rfind('|').ok_or("deletion-restore journal is incomplete")?;
    let expected = u32::from_str_radix(fields[6], 16)
        .map_err(|_| "deletion-restore journal has an invalid checksum")?;
    if checksum(line[..payload_end].as_bytes()) != expected {
        return Err("deletion-restore journal checksum does not match".to_string());
    }
    let (staged, device, inode) = match fields[3] {
        "P" => (false, 0, 0),
        "S" => (
            true,
            fields[4].parse::<u64>().map_err(|_| "invalid staged device number")?,
            fields[5].parse::<u64>().map_err(|_| "invalid staged inode number")?,
        ),
        _ => return Err("deletion-restore journal has an invalid phase".to_string()),
    };
    Ok(Some(Pending {
        event_id: fields[1].to_string(),
        object_id: fields[2].to_string(),
        staged,
        device,
        inode,
    }))
}

fn open_etc() -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let directory = options.open("/etc")
        .map_err(|error| format!("cannot safely open /etc: {error}"))?;
    let metadata = directory.metadata()
        .map_err(|error| format!("cannot inspect /etc: {error}"))?;
    if !metadata.is_dir() || metadata.uid() != 0 {
        return Err("/etc must be a root-owned directory".to_string());
    }
    Ok(directory)
}

fn stat_at(directory: &File, name: &str) -> Result<Option<libc::stat>, String> {
    let name = CString::new(name).map_err(|_| "invalid internal filename".to_string())?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(directory.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW)
    };
    if result == 0 {
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(format!("cannot inspect /etc entry: {error}"))
    }
}

fn unlink_if_same(directory: &File, name: &str, device: u64, inode: u64) -> Result<(), String> {
    if let Some(stat) = stat_at(directory, name)? {
        if stat.st_dev as u64 != device || stat.st_ino as u64 != inode
            || (stat.st_mode & libc::S_IFMT) != libc::S_IFREG
            || stat.st_nlink != 1
        {
            return Err(format!("refusing to remove /etc/{name}: its identity does not match the interrupted restore"));
        }
        let name = CString::new(name).map_err(|_| "invalid internal filename".to_string())?;
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(format!("cannot remove interrupted /etc/{name:?}: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn remove_pending(root: &Path) -> Result<(), String> {
    fs::remove_file(pending_path(root))
        .map_err(|error| format!("cannot clear deletion-restore journal: {error}"))?;
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync deletion-restore journal removal: {error}"))
}

/// Roll back a partially published file, but only when its inode matches the
/// durable transaction record. A concurrently recreated hostname is untouched.
pub fn recover_interrupted_restore(root: &Path) -> Result<(), String> {
    let Some(pending) = read_pending(root)? else { return Ok(()); };
    let directory = open_etc()?;
    let stage = stage_name(&pending.event_id);
    if pending.staged {
        // If the published path has since been replaced, preserve that newer
        // file. The transaction's inode is no longer at the target.
        if let Some(stat) = stat_at(&directory, TARGET)? {
            if stat.st_dev as u64 == pending.device && stat.st_ino as u64 == pending.inode {
                unlink_if_same(&directory, TARGET, pending.device, pending.inode)?;
            }
        }
        unlink_if_same(&directory, &stage, pending.device, pending.inode)?;
    } else if let Some(stat) = stat_at(&directory, &stage)? {
        if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG || stat.st_uid != 0 || stat.st_nlink != 1 {
            return Err("unpublished hostname staging file has unsafe metadata".to_string());
        }
        let name = CString::new(stage).unwrap();
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(format!("cannot remove unpublished hostname staging file: {}", std::io::Error::last_os_error()));
        }
    }
    directory.sync_all()
        .map_err(|error| format!("cannot sync /etc after interrupted deletion restore: {error}"))?;
    remove_pending(root)?;
    eprintln!("lifeline-systemd: rolled back interrupted recreation of /etc/hostname");
    Ok(())
}

fn verify_published(directory: &File, record: &system_catalog::CatalogRecord, bytes: &[u8], device: u64, inode: u64) -> Result<(), String> {
    let name = CString::new(TARGET).unwrap();
    let fd = unsafe {
        libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
    };
    if fd < 0 { return Err(format!("cannot open restored /etc/hostname: {}", std::io::Error::last_os_error())); }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|error| format!("cannot inspect restored hostname: {error}"))?;
    if !metadata.is_file() || metadata.dev() != device || metadata.ino() != inode
        || metadata.uid() != record.uid || metadata.gid() != record.gid
        || metadata.permissions().mode() & 0o7777 != record.mode
        || metadata.nlink() != 1
    {
        return Err("restored hostname metadata does not match the captured file".to_string());
    }
    let mut actual = Vec::with_capacity(bytes.len());
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut actual)
        .map_err(|error| format!("cannot verify restored hostname contents: {error}"))?;
    if actual != bytes { return Err("restored hostname contents do not match the recovery object".to_string()); }
    Ok(())
}

/// Recreate /etc/hostname from the content object referenced by a deletion event.
pub fn restore_deleted_hostname(root: &Path, event_id: &str) -> Result<(), String> {
    if !valid_id(event_id) { return Err("invalid deletion event ID".to_string()); }
    if read_pending(root)?.is_some() { return Err("another hostname recreation transaction is pending".to_string()); }
    let event = system_deletion::read_all(root)?.into_iter()
        .find(|event| event.event_id == event_id)
        .ok_or_else(|| "no matching hostname deletion event was found".to_string())?;
    let record = system_catalog::read_all(root)?.into_iter()
        .find(|record| record.object_id == event.previous_object_id && record.original_path == Path::new("/etc/hostname"))
        .ok_or_else(|| "deletion event has no matching hostname recovery object".to_string())?;
    if record.original_size > MAX_FILE_BYTES || record.uid != 0 {
        return Err("captured hostname metadata is outside the safe restore policy".to_string());
    }
    let bytes = system_store::get(root, &record.object_id)?;
    if bytes.len() as u64 != record.original_size {
        return Err("captured hostname content length does not match its catalog record".to_string());
    }

    let directory = open_etc()?;
    if stat_at(&directory, TARGET)?.is_some() {
        return Err("/etc/hostname already exists; refusing to overwrite a recreated file".to_string());
    }
    let mut pending = Pending {
        event_id: event_id.to_string(),
        object_id: record.object_id.clone(),
        staged: false,
        device: 0,
        inode: 0,
    };
    write_pending(root, &pending)?;

    let stage = stage_name(event_id);
    let stage_c = CString::new(stage.as_str()).unwrap();
    let fd = unsafe {
        libc::openat(directory.as_raw_fd(), stage_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600)
    };
    if fd < 0 {
        return Err(format!("cannot create protected hostname staging file: {}", std::io::Error::last_os_error()));
    }
    let mut staged = unsafe { File::from_raw_fd(fd) };
    staged.write_all(&bytes).map_err(|error| format!("cannot write hostname staging file: {error}"))?;
    if unsafe { libc::fchown(staged.as_raw_fd(), record.uid, record.gid) } != 0 {
        return Err(format!("cannot set restored hostname ownership: {}", std::io::Error::last_os_error()));
    }
    if unsafe { libc::fchmod(staged.as_raw_fd(), record.mode as libc::mode_t) } != 0 {
        return Err(format!("cannot set restored hostname permissions: {}", std::io::Error::last_os_error()));
    }
    staged.sync_all().map_err(|error| format!("cannot sync hostname staging file: {error}"))?;
    let metadata = staged.metadata().map_err(|error| format!("cannot inspect hostname staging file: {error}"))?;
    if !metadata.is_file() || metadata.uid() != record.uid || metadata.gid() != record.gid
        || metadata.permissions().mode() & 0o7777 != record.mode || metadata.nlink() != 1
    {
        return Err("hostname staging file metadata failed verification".to_string());
    }
    pending.staged = true;
    pending.device = metadata.dev();
    pending.inode = metadata.ino();
    write_pending(root, &pending)?;
    drop(staged);

    let target_c = CString::new(TARGET).unwrap();
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory.as_raw_fd(), stage_c.as_ptr(),
            directory.as_raw_fd(), target_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        // renameat2 failed, so publication did not occur. Remove only our
        // staged inode and leave any concurrently recreated hostname alone.
        let cleanup = unlink_if_same(&directory, &stage, pending.device, pending.inode)
            .and_then(|()| directory.sync_all().map_err(|sync_error| format!("cannot sync staging cleanup: {sync_error}")))
            .and_then(|()| remove_pending(root));
        if let Err(cleanup_error) = cleanup {
            return Err(format!("cannot publish restored /etc/hostname without overwriting another file: {error}; staging cleanup remains pending: {cleanup_error}"));
        }
        return Err(format!("cannot publish restored /etc/hostname without overwriting another file: {error}"));
    }
    directory.sync_all().map_err(|error| format!("cannot sync restored /etc/hostname directory entry: {error}"))?;
    if let Err(error) = verify_published(&directory, &record, &bytes, pending.device, pending.inode) {
        let rollback = recover_interrupted_restore(root);
        return match rollback {
            Ok(()) => Err(format!("restored hostname verification failed and the new file was rolled back: {error}")),
            Err(rollback_error) => Err(format!("restored hostname verification failed ({error}); rollback remains pending ({rollback_error})")),
        };
    }
    remove_pending(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lifeline-deleted-restore-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn persists_and_reads_both_transaction_phases() {
        let root = temporary_root("journal-roundtrip");
        let pending = Pending {
            event_id: "abcdef0123456789abcdef0123456789".to_string(),
            object_id: "0123456789abcdef0123456789abcdef".to_string(),
            staged: false,
            device: 0,
            inode: 0,
        };
        write_pending(&root, &pending).unwrap();
        assert_eq!(read_pending(&root).unwrap(), Some(pending.clone()));

        let staged = Pending { staged: true, device: 7, inode: 42, ..pending.clone() };
        write_pending(&root, &staged).unwrap();
        assert_eq!(read_pending(&root).unwrap(), Some(staged));
        remove_pending(&root).unwrap();
        assert_eq!(read_pending(&root).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_corrupt_transaction_journals() {
        let root = temporary_root("journal-corrupt");
        let pending = Pending {
            event_id: "abcdef0123456789abcdef0123456789".to_string(),
            object_id: "0123456789abcdef0123456789abcdef".to_string(),
            staged: true,
            device: 7,
            inode: 42,
        };
        write_pending(&root, &pending).unwrap();
        fs::write(pending_path(&root), b"LFCRE001|bad\\n").unwrap();
        assert!(read_pending(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_removes_only_the_recorded_inode() {
        let root = temporary_root("inode-cleanup");
        let directory = File::open(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, b"owned by this transaction").unwrap();
        let metadata = fs::metadata(&target).unwrap();
        unlink_if_same(&directory, "target", metadata.dev(), metadata.ino()).unwrap();
        assert!(!target.exists());

        fs::write(&target, b"concurrent replacement").unwrap();
        let metadata = fs::metadata(&target).unwrap();
        assert!(unlink_if_same(&directory, "target", metadata.dev(), metadata.ino() + 1).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"concurrent replacement");
        fs::remove_dir_all(root).unwrap();
    }
}
