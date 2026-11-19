//! The daemon's memory of each file as it was the last time it looked.
//!
//! When a file changes, the daemon needs the *old* contents, but by then the
//! file already holds the new ones. So for every watched file it keeps a copy
//! of the last version it saw (the "baseline"). On a change the baseline is
//! moved onto the shelf as an undo point and replaced with the new contents.
//!
//! Each baseline is `<home>/baseline/<hash>` plus a `<hash>.path` note that
//! records which file it belongs to.

use std::fs;
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

pub fn exists(path: &Path) -> bool {
    file_for(path).exists()
}

/// Make the file's current contents its new baseline.
pub fn store(path: &Path) -> Result<(), String> {
    fs::create_dir_all(dir()).map_err(|e| format!("cannot create baseline dir: {e}"))?;
    fs::copy(path, file_for(path))
        .map_err(|e| format!("cannot copy {}: {e}", path.display()))?;
    fs::write(sidecar_for(path), path.to_string_lossy().as_bytes())
        .map_err(|e| format!("cannot write baseline note: {e}"))?;
    Ok(())
}

/// Throw the baseline away (the file is gone and has been saved).
pub fn forget(path: &Path) {
    let _ = fs::remove_file(file_for(path));
    let _ = fs::remove_file(sidecar_for(path));
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
