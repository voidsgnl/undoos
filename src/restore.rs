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

/// Reconcile the baseline association after a move. A crash can happen
/// after rename but before metadata relocation, so either side may own it.
fn reconcile_move_baseline(from: &Path, to: &Path) -> Result<(), String> {
    if baseline::exists(from) {
        if baseline::exists(to) {
            return Err(format!("both paths have recovery baselines ({} and {}); leaving them untouched", from.display(), to.display()));
        }
        baseline::relocate(from, to)?;
    }
    Ok(())
}

/// Resolve interrupted file and move undo/redo transactions before accepting new work.
/// A transaction is completed only when the live file matches the intended snapshot;
/// it is aborted only when the original pre-operation state is still present.
pub fn reconcile_pending() -> Result<(), String> {
    let entries = journal::read_all()?;

    // Move transactions have no content copy to compare: the two possible
    // filesystem states are identified by which side of the rename exists.
    for pending in entries.iter().filter(|e| e.kind == "move_undo_pending" || e.kind == "move_redo_pending") {
        let is_undo = pending.kind == "move_undo_pending";
        let completed_kind = if is_undo { "restore" } else { "redo" };
        let aborted_kind = if is_undo { "move_undo_abort" } else { "move_redo_abort" };
        if entries.iter().any(|e| e.id > pending.id &&
            (e.kind == completed_kind || e.kind == aborted_kind) && e.path == pending.path) {
            continue;
        }
        let reference_id: u64 = pending.path.parse()
            .map_err(|_| format!("invalid move transaction reference: {}", pending.path))?;
        let original = if is_undo {
            entries.iter().find(|e| e.kind == "move" && e.id == reference_id)
        } else {
            let restore = entries.iter().find(|e| e.kind == "restore" && e.id == reference_id)
                .ok_or_else(|| format!("move redo #{} references missing restore history", pending.id))?;
            let move_id: u64 = restore.path.parse()
                .map_err(|_| format!("invalid original move reference: {}", restore.path))?;
            entries.iter().find(|e| e.kind == "move" && e.id == move_id)
        }.ok_or_else(|| format!("move transaction #{} references missing move history", pending.id))?;
        let old = Path::new(&original.path);
        let new = Path::new(&original.snapshot);
        let (source, destination) = if is_undo { (new, old) } else { (old, new) };
        let source_exists = source.exists();
        let destination_exists = destination.exists();
        let id = journal::next_id()?;

        match (source_exists, destination_exists) {
            (true, false) => {
                reconcile_move_baseline(source, destination)?;
                journal::append(id, aborted_kind, &pending.path, "")?;
            }
            (false, true) => {
                reconcile_move_baseline(source, destination)?;
                journal::append(id, completed_kind, &pending.path, "")?;
            }
            (true, true) => return Err(format!(
                "unfinished move transaction #{} is ambiguous: both {} and {} exist; leaving them untouched",
                pending.id, source.display(), destination.display()
            )),
            (false, false) => return Err(format!(
                "unfinished move transaction #{} is ambiguous: neither {} nor {} exists; leaving recovery history untouched",
                pending.id, source.display(), destination.display()
            )),
        }
    }

    for pending in entries.iter().filter(|e| e.kind == "undo_pending" || e.kind == "redo_pending") {
        let completed_kind = if pending.kind == "undo_pending" { "restore" } else { "redo" };
        let aborted_kind = if pending.kind == "undo_pending" { "undo_abort" } else { "redo_abort" };
        let already_resolved = entries.iter().any(|e|
            e.id > pending.id && (e.kind == completed_kind || e.kind == aborted_kind) && e.path == pending.path
        );
        if already_resolved { continue; }

        let reference_id: u64 = pending.path.parse()
            .map_err(|_| format!("invalid recovery transaction reference: {}", pending.path))?;
        let referenced = if pending.kind == "undo_pending" {
            entries.iter().find(|e| e.kind == "snap" && e.id == reference_id)
        } else {
            entries.iter().find(|e| e.kind == "restore" && e.id == reference_id)
        }.ok_or_else(|| format!("recovery transaction #{} references missing history", pending.id))?;
        let destination = if pending.kind == "undo_pending" {
            referenced.path.clone()
        } else {
            entries.iter().find(|e| e.kind == "snap" && e.id.to_string() == referenced.path)
                .ok_or_else(|| format!("original entry #{} is missing", referenced.path))?.path.clone()
        };
        let intended = referenced.snapshot.as_str();
        let live = Path::new(&destination);
        let id = journal::next_id()?;

        if snapshot::matches(intended, live)? {
            if baseline::exists(live) && live.is_file() { baseline::store(live)?; }
            journal::append(id, completed_kind, &pending.path, &pending.snapshot)?;
        } else if snapshot::matches(&pending.snapshot, live)? {
            journal::append(id, aborted_kind, &pending.path, "")?;
        } else {
            return Err(format!(
                "unfinished recovery transaction #{} needs attention: {} matches neither its saved target nor its pre-recovery copy; leaving it untouched",
                pending.id, destination
            ));
        }
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
        let intent_id = journal::next_id()?;
        journal::append(intent_id, "move_undo_pending", &entry.id.to_string(), "")?;
        if let Some(parent) = old.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        if let Err(err) = fs::rename(new, old) {
            journal::append(journal::next_id()?, "move_undo_abort", &entry.id.to_string(), "")?;
            return Err(format!("cannot move {} back to {}: {err}", new.display(), old.display()));
        }
        reconcile_move_baseline(new, old)?;
        journal::append(journal::next_id()?, "restore", &entry.id.to_string(), "")?;
        sync_parent(old)?;
        if new.parent() != old.parent() { sync_parent(new)?; }
        return Ok(());
    }
    if entry.kind != "snap" { return Ok(()); }

    let dest = Path::new(&entry.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let id = journal::next_id()?;
    let backup = if dest.is_file() { snapshot::store(id, dest)? } else { String::new() };
    journal::append(id + 1, "undo_pending", &entry.id.to_string(), &backup)?;
    if let Err(err) = apply_snapshot(&entry.snapshot, &entry.path) {
        // If the original state is still present, record a clean abort. If the
        // atomic rename happened, leave the intent for startup reconciliation.
        if snapshot::matches(&backup, dest).unwrap_or(false) {
            let _ = journal::append(journal::next_id()?, "undo_abort", &entry.id.to_string(), "");
        } else if let Ok(true) = snapshot::matches(&entry.snapshot, dest) {
            if had_baseline && dest.is_file() { let _ = baseline::store(dest); }
            let _ = journal::append(journal::next_id()?, "restore", &entry.id.to_string(), &backup);
        }
        return Err(err);
    }
    if had_baseline {
        baseline::store(dest).map_err(|err| format!("could not update recovery baseline; transaction will be reconciled: {err}"))?;
    }
    journal::append(journal::next_id()?, "restore", &entry.id.to_string(), &backup)?;
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
        journal::append(id, "move_redo_pending", &restore.id.to_string(), "")?;
        if let Some(parent) = new.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        if let Err(err) = fs::rename(old, new) {
            journal::append(journal::next_id()?, "move_redo_abort", &restore.id.to_string(), "")?;
            return Err(format!("cannot move {} to {}: {err}", old.display(), new.display()));
        }
        reconcile_move_baseline(old, new)?;
        journal::append(journal::next_id()?, "redo", &restore.id.to_string(), "")?;
        sync_parent(new)?;
        if old.parent() != new.parent() { sync_parent(old)?; }
        return Ok(Some(restore));
    }

    let dest = Path::new(&original.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let backup = if dest.is_file() { snapshot::store(id, dest)? } else { String::new() };
    journal::append(id + 1, "redo_pending", &restore.id.to_string(), &backup)?;
    if let Err(err) = apply_snapshot(&restore.snapshot, &original.path) {
        if snapshot::matches(&backup, dest).unwrap_or(false) {
            let _ = journal::append(journal::next_id()?, "redo_abort", &restore.id.to_string(), "");
        } else if let Ok(true) = snapshot::matches(&restore.snapshot, dest) {
            if had_baseline && dest.is_file() { let _ = baseline::store(dest); }
            let _ = journal::append(journal::next_id()?, "redo", &restore.id.to_string(), &backup);
        }
        return Err(err);
    }
    if had_baseline {
        baseline::store(dest).map_err(|err| format!("could not update recovery baseline; transaction will be reconciled: {err}"))?;
    }
    journal::append(journal::next_id()?, "redo", &restore.id.to_string(), &backup)?;
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
