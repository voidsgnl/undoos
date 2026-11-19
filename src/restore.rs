//! Recovery operations append compensating events; original history is immutable.
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{baseline, journal};
use crate::journal::Entry;
use crate::snapshot;

static RESTORE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_sibling(destination: &Path) -> PathBuf {
    let id = RESTORE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".lifeline-restore-{}-{id}.tmp", std::process::id()));
    destination.with_file_name(name)
}

/// Refuse to overwrite a watched file if it no longer matches the last
/// state Lifeline observed when it recorded the undo point.
fn ensure_no_conflict(path: &Path) -> Result<bool, String> {
    if !baseline::exists(path) { return Ok(false); }
    if !path.is_file() {
        return Err(format!("recovery conflict: {} is no longer present; leaving the current filesystem state untouched", path.display()));
    }
    if !baseline::same_as(path)? {
        return Err(format!("recovery conflict: {} changed after Lifeline recorded this event; leaving your newer version untouched", path.display()));
    }
    Ok(true)
}

/// If an identity is available, ensure this is still the file Lifeline recorded.
fn ensure_move_identity(path: &Path) -> Result<(), String> {
    if let Some(expected) = baseline::identity(path) {
        if baseline::live_identity(path) != Some(expected) {
            return Err(format!("recovery conflict: {} is no longer the file Lifeline recorded; refusing to move it", path.display()));
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let directory = OpenOptions::new().read(true).open(parent)
        .map_err(|e| format!("cannot open directory {} for sync: {e}", parent.display()))?;
    directory.sync_all()
        .map_err(|e| format!("cannot sync directory {}: {e}", parent.display()))
}

/// Install a saved version without ever exposing a partially copied destination.
/// The temporary file is created in the same directory so rename stays atomic
/// on the destination filesystem.
fn apply_snapshot(name: &str, destination: &str) -> Result<(), String> {
    let dest = Path::new(destination);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    if name.is_empty() {
        if dest.exists() {
            fs::remove_file(dest)
                .map_err(|e| format!("cannot remove {}: {e}", dest.display()))?;
            sync_parent(dest)?;
        }
        return Ok(());
    }

    let source = snapshot::path_for(name);
    if !source.is_file() {
        return Err(format!("recovery copy is missing: {}", source.display()));
    }

    let temporary = temporary_sibling(dest);
    let result = (|| -> io::Result<()> {
        let mut input = fs::File::open(&source)?;
        let permissions = input.metadata()?.permissions();
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        // Match the saved file's mode bits (including executable/read-only).
        fs::set_permissions(&temporary, permissions)?;
        output.sync_all()?;
        fs::rename(&temporary, dest)?;
        // Persist the directory entry change as well as the file contents.
        let parent = dest.parent().unwrap_or_else(|| Path::new("."));
        OpenOptions::new().read(true).open(parent)?.sync_all()?;
        Ok(())
    })();

    if let Err(err) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot atomically restore {}: {err}", destination));
    }
    Ok(())
}

/// Restore a saved file and append a record of the state replaced.
pub fn undo_entry(entry: &Entry) -> Result<(), String> {
    if entry.kind == "move" {
        let old = Path::new(&entry.path);
        let new = Path::new(&entry.snapshot);
        if old.exists() { return Err(format!("cannot undo move: {} already exists", old.display())); }
        if !new.exists() { return Err(format!("cannot undo move: {} is missing", new.display())); }
        ensure_move_identity(new)?;
        let id = journal::next_id()?;
        if let Some(parent) = old.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        fs::rename(new, old).map_err(|e| format!("cannot move {} back to {}: {e}", new.display(), old.display()))?;
        if let Err(err) = baseline::relocate(new, old) {
            let _ = fs::rename(old, new);
            return Err(err);
        }
        if let Err(err) = journal::append(id, "restore", &entry.id.to_string(), "") {
            let _ = baseline::relocate(old, new);
            let _ = fs::rename(old, new);
            return Err(err);
        }
        sync_parent(old)?;
        if new.parent() != old.parent() { sync_parent(new)?; }
        return Ok(());
    }
    if entry.kind != "snap" { return Ok(()); }

    let dest = Path::new(&entry.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let id = journal::next_id()?;
    let backup = if dest.is_file() { snapshot::store(id, dest)? } else { String::new() };
    if let Err(err) = apply_snapshot(&entry.snapshot, &entry.path) {
        // apply_snapshot can fail after rename if the directory sync fails.
        // Restore the pre-operation contents before reporting failure.
        let _ = apply_snapshot(&backup, &entry.path);
        if !backup.is_empty() { snapshot::remove(&backup); }
        return Err(err);
    }
    if had_baseline {
        if let Err(err) = baseline::store(dest) {
            let _ = apply_snapshot(&backup, &entry.path);
            if dest.is_file() { let _ = baseline::store(dest); }
            return Err(format!("could not update recovery baseline: {err}"));
        }
    }
    if let Err(err) = journal::append(id, "restore", &entry.id.to_string(), &backup) {
        let _ = apply_snapshot(&backup, &entry.path);
        if had_baseline && dest.is_file() { let _ = baseline::store(dest); }
        if !backup.is_empty() { snapshot::remove(&backup); }
        return Err(err);
    }
    Ok(())
}

pub fn undo_last() -> Result<Option<Entry>, String> {
    let entry = match journal::active_snapshots()?.into_iter()
        .chain(journal::active_moves()?.into_iter()).max_by_key(|e| e.id) {
        Some(entry) => entry,
        None => return Ok(None),
    };
    undo_entry(&entry)?;
    Ok(Some(entry))
}

/// Reapply the state which existed immediately before the latest undo.
pub fn redo_last() -> Result<Option<Entry>, String> {
    let restore = match journal::pending_redo()? { Some(entry) => entry, None => return Ok(None) };
    let original = journal::read_all()?.into_iter()
        .find(|e| (e.kind == "snap" || e.kind == "move") && e.id.to_string() == restore.path)
        .ok_or_else(|| format!("original entry #{} is missing", restore.path))?;
    let id = journal::next_id()?;

    if original.kind == "move" {
        let old = Path::new(&original.path);
        let new = Path::new(&original.snapshot);
        if new.exists() { return Err(format!("cannot redo move: {} already exists", new.display())); }
        if !old.exists() { return Err(format!("cannot redo move: {} is missing", old.display())); }
        ensure_move_identity(old)?;
        if let Some(parent) = new.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        fs::rename(old, new).map_err(|e| format!("cannot move {} to {}: {e}", old.display(), new.display()))?;
        if let Err(err) = baseline::relocate(old, new) {
            let _ = fs::rename(new, old);
            return Err(err);
        }
        if let Err(err) = journal::append(id, "redo", &restore.id.to_string(), "") {
            let _ = baseline::relocate(new, old);
            let _ = fs::rename(new, old);
            return Err(err);
        }
        sync_parent(new)?;
        if old.parent() != new.parent() { sync_parent(old)?; }
        return Ok(Some(restore));
    }

    let dest = Path::new(&original.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let backup = if dest.is_file() { snapshot::store(id, dest)? } else { String::new() };
    if let Err(err) = apply_snapshot(&restore.snapshot, &original.path) {
        let _ = apply_snapshot(&backup, &original.path);
        if !backup.is_empty() { snapshot::remove(&backup); }
        return Err(err);
    }
    if had_baseline {
        if let Err(err) = baseline::store(dest) {
            let _ = apply_snapshot(&backup, &original.path);
            if dest.is_file() { let _ = baseline::store(dest); }
            return Err(format!("could not update recovery baseline: {err}"));
        }
    }
    if let Err(err) = journal::append(id, "redo", &restore.id.to_string(), &backup) {
        let _ = apply_snapshot(&backup, &original.path);
        if had_baseline && dest.is_file() { let _ = baseline::store(dest); }
        if !backup.is_empty() { snapshot::remove(&backup); }
        return Err(err);
    }
    Ok(Some(restore))
}

/// Rewind active saved changes newer than id, newest first, without truncating history.
pub fn rewind_to(id: u64) -> Result<usize, String> {
    let mut newer: Vec<Entry> = journal::active_snapshots()?.into_iter()
        .chain(journal::active_moves()?.into_iter()).filter(|e| e.id > id).collect();
    newer.sort_by_key(|e| std::cmp::Reverse(e.id));
    let mut done = 0;
    for entry in newer { undo_entry(&entry)?; done += 1; }
    Ok(done)
}
