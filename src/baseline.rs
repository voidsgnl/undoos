//! The daemon's memory of each file as it was the last time it looked.
//!
//! When a file changes, the daemon needs the *old* contents, but by then the
//! file already holds the new ones. So for every watched file it keeps a copy
//! of the last version it saw (the "baseline"). On a change the baseline is
//! moved onto the shelf as an undo point and replaced with the new contents.
//!
//! Each baseline is `<home>/baseline/<hash>` plus a `<hash>.path` note that
//! records which file it belongs to and a `<hash>.id` note with Linux file identity.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::{Path, PathBuf};

use crate::journal;

fn dir() -> PathBuf {
    journal::home().join("baseline")
}

/// FNV-1a, a tiny hash whose output never changes between Rust versions.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn stem(path: &Path) -> String {
    format!("{:016x}", fnv1a(&path.to_string_lossy()))
}

/// Where the baseline copy of `path` lives.
pub fn file_for(path: &Path) -> PathBuf {
    dir().join(stem(path))
}

fn sidecar_for(path: &Path) -> PathBuf {
    dir().join(format!("{}.path", stem(path)))
}

fn identity_for(path: &Path) -> PathBuf {
    dir().join(format!("{}.id", stem(path)))
}

/// Live Linux identity: device, inode, and creation time (statx btime).
/// Creation time does not change on rename, unlike ctime.
#[cfg(target_os = "linux")]
pub fn live_identity(path: &Path) -> Option<(u64, u64, i64, u32)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).ok()?;
    let name = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::statx(libc::AT_FDCWD, name.as_ptr(), libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BTIME, &mut stat)
    };
    if result != 0 || stat.stx_mask & libc::STATX_BTIME == 0 { return None; }
    Some((meta.dev(), meta.ino(), stat.stx_btime.tv_sec, stat.stx_btime.tv_nsec))
}

#[cfg(not(target_os = "linux"))]
pub fn live_identity(_path: &Path) -> Option<(u64, u64, i64, u32)> { None }

/// Read the identity recorded when this baseline was last refreshed.
pub fn identity(path: &Path) -> Option<(u64, u64, i64, u32)> {
    let text = fs::read_to_string(identity_for(path)).ok()?;
    let mut parts = text.trim().split(':');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

/// Add identity metadata to a baseline created by an older Lifeline version.
pub fn record_identity(path: &Path, _meta: &fs::Metadata) -> Result<(), String> {
    if let Some((device, inode, birth_sec, birth_nsec)) = live_identity(path) {
        let value = format!("{device}:{inode}:{birth_sec}:{birth_nsec}");
        atomic_write(&identity_for(path), value.as_bytes())
            .map_err(|e| format!("cannot save identity for {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Whether a saved baseline exists for this path.
pub fn exists(path: &Path) -> bool {
    file_for(path).exists()
}

/// Make the file's current contents its new baseline. The copy is staged
/// beside the destination and renamed into place only after a stable read.
pub fn store(path: &Path) -> Result<(), String> {
    fs::create_dir_all(dir()).map_err(|e| format!("cannot create baseline dir: {e}"))?;
    let destination = file_for(path);
    copy_stable(path, &destination)?;
    let note = sidecar_for(path);
    atomic_write(&note, path.to_string_lossy().as_bytes())
        .map_err(|e| format!("cannot write baseline note: {e}"))?;
    #[cfg(target_os = "linux")]
    {
        if let Some((device, inode, birth_sec, birth_nsec)) = live_identity(path) {
            let value = format!("{device}:{inode}:{birth_sec}:{birth_nsec}");
            atomic_write(&identity_for(path), value.as_bytes())
                .map_err(|e| format!("cannot write baseline identity: {e}"))?;
        } else {
            // Filesystems without statx birth time cannot provide safe persistent
            // move matching. Keep normal file recovery working without guessing.
            let _ = fs::remove_file(identity_for(path));
        }
    }
    Ok(())
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temp_path(destination: &Path) -> PathBuf {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}-{id}", std::process::id()));
    destination.with_file_name(name)
}

/// Copy a changing source without accepting a torn or mixed-version read.
fn copy_stable(source: &Path, destination: &Path) -> Result<(), String> {
    for attempt in 0..3 {
        let mut input = File::open(source)
            .map_err(|e| format!("cannot open {}: {e}", source.display()))?;
        let before = input.metadata()
            .map_err(|e| format!("cannot inspect {}: {e}", source.display()))?;
        let temporary = temp_path(destination);
        let result = (|| -> io::Result<()> {
            let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            let after = fs::metadata(source)?;
            let opened_after = input.metadata()?;
            if !same_version(&before, &after) || !same_version(&before, &opened_after) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "source changed while being copied"));
            }
            fs::rename(&temporary, destination)?;
            Ok(())
        })();
        if result.is_ok() { return Ok(()); }
        let _ = fs::remove_file(&temporary);
        let err = result.unwrap_err();
        if err.kind() != io::ErrorKind::Interrupted || attempt == 2 {
            return Err(format!("cannot create stable baseline for {}: {err}", source.display()));
        }
    }
    unreachable!()
}

fn same_version(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    if a.len() != b.len() || a.modified().ok() != b.modified().ok() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    { true }
}

fn atomic_write(destination: &Path, contents: &[u8]) -> io::Result<()> {
    let temporary = temp_path(destination);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        use std::io::Write;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, destination)
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}

/// Throw the baseline away (the file is gone and has been saved).
pub fn forget(path: &Path) {
    let _ = fs::remove_file(file_for(path));
    let _ = fs::remove_file(sidecar_for(path));
    let _ = fs::remove_file(identity_for(path));
}

/// True if the file on disk is byte-for-byte the same as its baseline.
pub fn same_as(path: &Path) -> Result<bool, String> {
    let saved = file_for(path);
    let now = fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let then = fs::metadata(&saved).map_err(|e| format!("cannot read baseline: {e}"))?;
    if now.len() != then.len() {
        return Ok(false);
    }
    let a = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let b = fs::read(&saved).map_err(|e| format!("cannot read baseline: {e}"))?;
    Ok(a == b)
}

/// Every file path that currently has a baseline.
pub fn list_all() -> Result<Vec<String>, String> {
    let folder = dir();
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let read = fs::read_dir(&folder).map_err(|e| format!("cannot read baseline dir: {e}"))?;
    for entry in read.flatten() {
        let note = entry.path();
        if note.extension().and_then(|e| e.to_str()) == Some("path") {
            if let Ok(text) = fs::read_to_string(&note) {
                paths.push(text);
            }
        }
    }
    Ok(paths)
}


/// Move a baseline's storage key when the underlying file is renamed.
pub fn relocate(old: &Path, new: &Path) -> Result<(), String> {
    if exists(new) { return Err(format!("a baseline already exists for {}", new.display())); }
    let old_file = file_for(old);
    let new_file = file_for(new);
    let old_note = sidecar_for(old);
    let new_note = sidecar_for(new);
    let old_identity = identity_for(old);
    let new_identity = identity_for(new);
    let saved_identity = fs::read(&old_identity).ok();
    fs::rename(&old_file, &new_file)
        .map_err(|e| format!("cannot relocate baseline for {}: {e}", old.display()))?;
    if let Err(e) = atomic_write(&new_note, new.to_string_lossy().as_bytes()) {
        let _ = fs::rename(&new_file, &old_file);
        return Err(format!("cannot update baseline path note: {e}"));
    }
    if let Some(identity) = saved_identity {
        if let Err(e) = atomic_write(&new_identity, &identity) {
            let _ = fs::rename(&new_file, &old_file);
            let _ = fs::remove_file(&new_note);
            return Err(format!("cannot relocate baseline identity: {e}"));
        }
    }
    let _ = fs::remove_file(old_note);
    let _ = fs::remove_file(old_identity);
    Ok(())
}
