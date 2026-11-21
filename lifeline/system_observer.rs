//! Narrow automatic observer for the persistent hostname configuration file.
//!
//! This observer watches the /etc directory but reacts only to completed
//! writes or atomic replacements of the exact "hostname" entry. It stores
//! immutable content versions; it does not restore files or execute commands.

use std::{
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

#[path = "system_storage.rs"]
mod system_storage;
#[path = "system_store.rs"]
mod system_store;
#[path = "system_catalog.rs"]
mod system_catalog;
#[path = "system_capture.rs"]
mod system_capture;
#[path = "system_reconcile.rs"]
mod system_reconcile;
#[path = "system_deletion.rs"]
mod system_deletion;

const TARGET: &str = "/etc/hostname";
const ROOT: &str = "/etc";
const NAME: &[u8] = b"hostname";
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const EVENT_BUFFER_BYTES: usize = 16 * 1024;
const RECONCILE_INTERVAL_MS: i32 = 30_000;
const WATCH_MASK: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_ATTRIB
    | libc::IN_DELETE
    | libc::IN_MOVE_SELF
    | libc::IN_DELETE_SELF;

fn same_generation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
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

/// Read the fixed target without following a symbolic link.
fn read_current() -> Result<Option<Vec<u8>>, String> {
    let directory_path = std::ffi::CString::new(ROOT).expect("fixed path has no NUL");
    let directory_fd = unsafe {
        libc::open(
            directory_path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    if directory_fd < 0 {
        return Err(format!(
            "cannot safely open {ROOT}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: open returned a new owned directory descriptor.
    let directory = unsafe { File::from_raw_fd(directory_fd) };
    let directory_metadata = directory
        .metadata()
        .map_err(|error| format!("cannot inspect {ROOT}: {error}"))?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != 0
        || directory_metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "{ROOT} is not a root-owned directory with safe permissions"
        ));
    }
    let name = std::ffi::CString::new(NAME).expect("fixed filename has no NUL");
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(format!("cannot safely open {TARGET}: {error}"));
    }
    // SAFETY: open returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let before = file
        .metadata()
        .map_err(|error| format!("cannot inspect {TARGET}: {error}"))?;
    if !before.is_file()
        || before.uid() != 0
        || before.nlink() != 1
        || before.len() > MAX_FILE_BYTES
    {
        return Err(format!(
            "{TARGET} is not a supported root-owned regular file"
        ));
    }

    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {TARGET}: {error}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{TARGET} exceeds the 64 MiB observer limit"));
    }
    let after = file
        .metadata()
        .map_err(|error| format!("cannot recheck {TARGET}: {error}"))?;
    if !same_generation(&before, &after) || bytes.len() as u64 != before.len() {
        return Err(format!("{TARGET} changed while the observer read it"));
    }
    Ok(Some(bytes))
}

fn latest_hostname_record(
    storage_root: &Path,
) -> Result<Option<system_catalog::CatalogRecord>, String> {
    Ok(system_catalog::read_all(storage_root)?
        .into_iter()
        .filter(|record| record.original_path == Path::new(TARGET))
        .last())
}

/// Store a baseline on first start, then append a version only when the
/// current file content differs from the latest saved hostname version.
fn observe_current(storage_root: &Path) -> Result<bool, String> {
    let current = read_current()?;
    let previous = latest_hostname_record(storage_root)?;
    let deletions = system_deletion::read_all(storage_root)?;
    let latest_deletion = deletions.last();

    let Some(current) = current else {
        let Some(previous) = previous else {
            // There is no preceding content object to recover yet.
            return Ok(false);
        };
        if latest_deletion.is_some_and(|event| event.deleted_at_ns > previous.captured_at_ns) {
            return Ok(false);
        }
        let event = system_deletion::append(storage_root, &previous.object_id)?;
        eprintln!(
            "lifeline-system-observer: recorded deletion of {TARGET}; preceding object {}",
            event.previous_object_id
        );
        return Ok(true);
    };

    if let Some(previous) = previous {
        let deletion_is_newer = latest_deletion
            .is_some_and(|event| event.deleted_at_ns > previous.captured_at_ns);
        if !deletion_is_newer {
            let previous_bytes = system_store::get(storage_root, &previous.object_id)?;
            if previous_bytes == current {
                return Ok(false);
            }
        }
    }

    let policy = system_capture::CapturePolicy::new(
        Path::new(ROOT),
        vec![PathBuf::from("hostname")],
    )?;
    let record = system_capture::capture(storage_root, &policy, Path::new("hostname"))?;
    eprintln!(
        "lifeline-system-observer: recorded {TARGET}: {} bytes, object {}",
        record.original_size, record.object_id
    );
    Ok(true)
}

/// Parse a sequence of Linux inotify_event records without casting an
/// unaligned byte buffer to a Rust struct.
fn relevant_event(buffer: &[u8]) -> Result<bool, String> {
    let mut offset = 0;
    let mut relevant = false;
    while offset < buffer.len() {
        if buffer.len() - offset < 16 {
            return Err("inotify returned a truncated event header".to_string());
        }
        let mask = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
        if mask & (libc::IN_IGNORED | libc::IN_MOVE_SELF | libc::IN_DELETE_SELF) != 0 {
            return Err("the /etc watch was invalidated; restarting the observer".to_string());
        }
        let name_len =
            u32::from_ne_bytes(buffer[offset + 12..offset + 16].try_into().unwrap()) as usize;
        offset += 16;
        if name_len > buffer.len() - offset {
            return Err("inotify returned a truncated event name".to_string());
        }
        let name_field = &buffer[offset..offset + name_len];
        let name = name_field.split(|byte| *byte == 0).next().unwrap_or_default();
        if mask & libc::IN_Q_OVERFLOW != 0
            || (name == NAME
                && mask
                    & (libc::IN_CLOSE_WRITE
                        | libc::IN_MOVED_TO
                        | libc::IN_ATTRIB
                        | libc::IN_DELETE)
                    != 0)
        {
            relevant = true;
        }
        offset += name_len;
    }
    Ok(relevant)
}


fn poll_readable(fd: i32, timeout_ms: i32) -> Result<bool, String> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("cannot wait for filesystem events: {error}"));
        }
        if result == 0 {
            return Ok(false);
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err("inotify descriptor reported an unrecoverable poll condition".to_string());
        }
        return Ok(descriptor.revents & libc::POLLIN != 0);
    }
}

fn watch_etc() -> Result<File, String> {
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "cannot initialize inotify: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: inotify_init1 returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let directory = std::ffi::CString::new(ROOT).expect("fixed path has no NUL");
    let watch = unsafe {
        libc::inotify_add_watch(
            file.as_raw_fd(),
            directory.as_ptr(),
            WATCH_MASK | libc::IN_DONT_FOLLOW | libc::IN_ONLYDIR,
        )
    };
    if watch < 0 {
        return Err(format!(
            "cannot watch the fixed /etc directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

fn run() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("the system observer must run as root".to_string());
    }

    let storage_root = system_storage::verify()?;
    system_store::initialize(&storage_root)?;
    let report = system_reconcile::reconcile(&storage_root)?;
    eprintln!(
        "lifeline-system-observer: verified {} catalog records",
        report.catalog_records
    );

    // Establish the directory watch before the initial baseline check. Any
    // concurrent write is therefore queued and processed after the baseline.
    let mut watcher = watch_etc()?;
    match observe_current(&storage_root) {
        Ok(true) => {
            eprintln!("lifeline-system-observer: initial hostname baseline is ready")
        }
        Ok(false) => {
            eprintln!("lifeline-system-observer: hostname baseline is current")
        }
        Err(error) => eprintln!(
            "lifeline-system-observer: initial observation unavailable: {error}"
        ),
    }

    let mut buffer = vec![0_u8; EVENT_BUFFER_BYTES];
    loop {
        // Inotify is the fast path. Periodic reconciliation is the safety net:
        // it repairs the latest known state if an event was missed or the
        // observer was unable to process it. It cannot reconstruct intermediate
        // versions that were overwritten before reconciliation.
        if !poll_readable(watcher.as_raw_fd(), RECONCILE_INTERVAL_MS)? {
            match observe_current(&storage_root) {
                Ok(true) => eprintln!(
                    "lifeline-system-observer: periodic reconciliation recorded {TARGET}"
                ),
                Ok(false) => {}
                Err(error) => eprintln!(
                    "lifeline-system-observer: periodic reconciliation unavailable: {error}"
                ),
            }
            continue;
        }
        let count = loop {
            match watcher.read(&mut buffer) {
                Ok(count) => break count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(format!("cannot read filesystem events: {error}"));
                }
            }
        };
        if count == 0 {
            continue;
        }
        match relevant_event(&buffer[..count]) {
            Ok(true) => match observe_current(&storage_root) {
                Ok(true) => {}
                Ok(false) => {}
                Err(error) => eprintln!(
                    "lifeline-system-observer: could not record {TARGET} change: {error}"
                ),
            },
            Ok(false) => {}
            Err(error) => return Err(format!("invalid inotify event: {error}")),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lifeline-system-observer: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(mask: u32, name: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i32.to_ne_bytes());
        bytes.extend_from_slice(&mask.to_ne_bytes());
        bytes.extend_from_slice(&0_u32.to_ne_bytes());
        let mut field = name.to_vec();
        if !field.is_empty() {
            field.push(0);
            while field.len() % 4 != 0 {
                field.push(0);
            }
        }
        bytes.extend_from_slice(&(field.len() as u32).to_ne_bytes());
        bytes.extend_from_slice(&field);
        bytes
    }


    #[test]
    fn periodic_poll_wait_distinguishes_timeout_from_readiness() {
        use std::io::Write;

        let mut descriptors = [-1_i32; 2];
        let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(result, 0);

        // SAFETY: pipe2 returned two newly owned descriptors.
        let reader = unsafe { File::from_raw_fd(descriptors[0]) };
        let mut writer = unsafe { File::from_raw_fd(descriptors[1]) };
        assert!(!poll_readable(reader.as_raw_fd(), 0).unwrap());

        writer.write_all(b"x").unwrap();
        assert!(poll_readable(reader.as_raw_fd(), 100).unwrap());
    }

    #[test]
    fn reacts_only_to_completed_changes_for_hostname() {
        assert!(relevant_event(&event(libc::IN_CLOSE_WRITE, NAME)).unwrap());
        assert!(relevant_event(&event(libc::IN_MOVED_TO, NAME)).unwrap());
        assert!(relevant_event(&event(libc::IN_DELETE, NAME)).unwrap());
        assert!(!relevant_event(&event(libc::IN_CLOSE_WRITE, b"hosts")).unwrap());
        assert!(!relevant_event(&event(libc::IN_CREATE, NAME)).unwrap());
    }

    #[test]
    fn rescans_after_inotify_queue_overflow() {
        assert!(relevant_event(&event(libc::IN_Q_OVERFLOW, b"")).unwrap());
    }

    #[test]
    fn rejects_truncated_event_records() {
        assert!(relevant_event(&[0_u8; 8]).is_err());
    }

    #[test]
    fn requests_restart_when_the_directory_watch_is_invalidated() {
        assert!(relevant_event(&event(libc::IN_IGNORED, b"")).is_err());
        assert!(relevant_event(&event(libc::IN_DELETE_SELF, b"")).is_err());
    }
}
