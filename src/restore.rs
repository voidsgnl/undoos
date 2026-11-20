//! Recovery operations append compensating events; original history is immutable.
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::journal::Entry;
use crate::snapshot;
use crate::{baseline, journal};

static RESTORE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Test-only synchronization point used by integration tests to send SIGKILL
/// at a real transaction boundary. It is inert in release builds.
fn test_pause(point: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var("LIFELINE_TEST_PAUSE_AT").as_deref() != Ok(point) {
        return;
    }
    if let Ok(marker) = std::env::var("LIFELINE_TEST_PAUSE_MARKER") {
        let _ = fs::write(marker, point);
    }
    if let Ok(release) = std::env::var("LIFELINE_TEST_CONTINUE_MARKER") {
        while !Path::new(&release).exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let id = RESTORE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".lifeline-restore-{}-{id}.tmp", std::process::id()));
    destination.with_file_name(name)
}

/// Refuse to overwrite a watched file if it no longer matches the last
/// state Lifeline observed when it recorded the undo point.
fn ensure_no_conflict(path: &Path) -> Result<bool, String> {
    if !baseline::exists(path) {
        return Ok(false);
    }
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
    let directory = OpenOptions::new()
        .read(true)
        .open(parent)
        .map_err(|e| format!("cannot open directory {} for sync: {e}", parent.display()))?;
    directory
        .sync_all()
        .map_err(|e| format!("cannot sync directory {}: {e}", parent.display()))
}

/// Install a saved version without ever exposing a partially copied destination.
/// The temporary file is created in the same directory so rename stays atomic
/// on the destination filesystem.
fn apply_snapshot(name: &str, destination: &str, expected_current: &str) -> Result<(), String> {
    // The file may have changed after the initial conflict check and backup.
    // Revalidate immediately before the destructive filesystem operation.
    let verify_current = || -> Result<(), String> {
        if snapshot::matches(expected_current, Path::new(destination))? {
            Ok(())
        } else {
            Err(format!(
                "recovery conflict: {} changed while recovery was being prepared; leaving the newer state untouched",
                destination
            ))
        }
    };
    let dest = Path::new(destination);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    if name.is_empty() {
        test_pause("apply_before_commit");
        verify_current()?;
        if dest.exists() {
            fs::remove_file(dest).map_err(|e| format!("cannot remove {}: {e}", dest.display()))?;
            sync_parent(dest)?;
        }
        return Ok(());
    }

    let source = snapshot::path_for(name);
    if !source.is_file() {
        return Err(format!("recovery copy is missing: {}", source.display()));
    }

    let temporary = temporary_sibling(dest);
    let prepared = (|| -> io::Result<()> {
        let mut input = fs::File::open(&source)?;
        let permissions = input.metadata()?.permissions();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        // Match the saved file's mode bits (including executable/read-only).
        fs::set_permissions(&temporary, permissions)?;
        output.sync_all()?;
        Ok(())
    })();

    if let Err(err) = prepared {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "cannot prepare recovery copy for {}: {err}",
            destination
        ));
    }

    // Recheck after staging the full replacement, immediately before rename.
    // This closes the practical race between the earlier backup and file copy.
    test_pause("apply_before_commit");
    if let Err(err) = verify_current() {
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }

    let committed = fs::rename(&temporary, dest).and_then(|()| {
        // Persist the directory entry change as well as the file contents.
        let parent = dest.parent().unwrap_or_else(|| Path::new("."));
        OpenOptions::new().read(true).open(parent)?.sync_all()
    });
    if let Err(err) = committed {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot atomically restore {}: {err}", destination));
    }
    Ok(())
}

/// Reconcile the baseline association after a move. A crash can happen
/// after rename but before metadata relocation, so either side may own it.
fn reconcile_move_baseline(from: &Path, to: &Path) -> Result<(), String> {
    if baseline::exists(from) || baseline::exists(to) {
        baseline::relocate(from, to)?;
    }
    Ok(())
}

/// During crash recovery, path existence alone cannot prove that a rename
/// completed: another file may have appeared at the destination. Compare the
/// live file with the identity retained by the baseline (which may already
/// have been relocated by a partially completed reconciliation).
fn ensure_reconciled_move_identity(baseline_path: &Path, live_path: &Path) -> Result<(), String> {
    let expected = baseline::identity(baseline_path)
        .or_else(|| baseline::identity(live_path))
        .ok_or_else(|| format!(
            "cannot safely reconcile move: saved identity for {} is missing; leaving both paths and history untouched",
            baseline_path.display()
        ))?;
    if baseline::live_identity(live_path) != Some(expected) {
        return Err(format!(
            "recovery conflict: {} is not the original moved file; leaving it untouched",
            live_path.display()
        ));
    }
    Ok(())
}

fn directory_identity(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect directory {}: {e}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!("recovery conflict: {} is not a directory", path.display()));
    }
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

fn rename_directory_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn staged_directory_path(destination: &Path, id: u64) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".lifeline-dir-{id}.tmp"));
    destination.with_file_name(name)
}

fn directory_is_empty(path: &Path) -> Result<bool, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| format!("cannot inspect directory {}: {e}", path.display()))?;
    match entries.next() {
        None => Ok(true),
        Some(Ok(_)) => Ok(false),
        Some(Err(error)) => Err(format!(
            "cannot inspect directory contents in {}: {error}",
            path.display()
        )),
    }
}

fn create_directory_transaction(
    original: &Entry,
    pending_kind: &str,
    completed_kind: &str,
    reference_id: u64,
) -> Result<(), String> {
    let destination = Path::new(&original.path);
    if fs::symlink_metadata(destination).is_ok() {
        return Err(format!(
            "recovery conflict: {} already exists; leaving it untouched",
            destination.display()
        ));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(format!(
            "cannot restore directory {} because its parent {} is missing",
            destination.display(),
            parent.display()
        ));
    }
    let id = journal::next_id()?;
    let staged = staged_directory_path(destination, id);
    if fs::symlink_metadata(&staged).is_ok() {
        return Err(format!("temporary recovery path already exists: {}", staged.display()));
    }
    fs::create_dir(&staged)
        .map_err(|e| format!("cannot stage directory {}: {e}", staged.display()))?;
    let identity = match directory_identity(&staged) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = fs::remove_dir(&staged);
            return Err(error);
        }
    };
    if let Err(error) = sync_parent(&staged) {
        let _ = fs::remove_dir(&staged);
        return Err(error);
    }
    if let Err(error) = journal::append(id, pending_kind, &reference_id.to_string(), &identity) {
        let _ = fs::remove_dir(&staged);
        return Err(error);
    }
    test_pause(if pending_kind == "dir_undo_pending" {
        "dir_undo_after_intent"
    } else {
        "dir_redo_after_intent"
    });
    if !directory_is_empty(&staged)? {
        return Err(format!(
            "recovery conflict: staged directory {} gained contents; leaving it untouched",
            staged.display()
        ));
    }
    if let Err(error) = rename_directory_noreplace(&staged, destination) {
        journal::append(
            journal::next_id()?,
            if pending_kind == "dir_undo_pending" { "dir_undo_abort" } else { "dir_redo_abort" },
            &reference_id.to_string(),
            "",
        )?;
        let _ = fs::remove_dir(&staged);
        return Err(format!(
            "cannot restore directory {}: {error}",
            destination.display()
        ));
    }
    sync_parent(destination)?;
    test_pause(if pending_kind == "dir_undo_pending" {
        "dir_undo_after_rename"
    } else {
        "dir_redo_after_rename"
    });
    journal::append(
        journal::next_id()?,
        completed_kind,
        &reference_id.to_string(),
        &identity,
    )?;
    Ok(())
}

fn remove_directory_transaction(
    original: &Entry,
    pending_kind: &str,
    completed_kind: &str,
    reference_id: u64,
    expected_identity: Option<&str>,
) -> Result<(), String> {
    let destination = Path::new(&original.path);
    let identity = directory_identity(destination)?;
    if expected_identity.is_some_and(|expected| !expected.is_empty() && expected != identity.as_str()) {
        return Err(format!(
            "recovery conflict: {} is not the directory Lifeline recorded; leaving it untouched",
            destination.display()
        ));
    }
    if !directory_is_empty(destination)? {
        return Err(format!(
            "recovery conflict: {} is not empty; refusing to remove it or any of its contents",
            destination.display()
        ));
    }
    let id = journal::next_id()?;
    journal::append(id, pending_kind, &reference_id.to_string(), &identity)?;
    test_pause(if pending_kind == "dir_undo_pending" {
        "dir_undo_after_intent"
    } else {
        "dir_redo_after_intent"
    });
    if let Err(error) = fs::remove_dir(destination) {
        journal::append(
            journal::next_id()?,
            if pending_kind == "dir_undo_pending" { "dir_undo_abort" } else { "dir_redo_abort" },
            &reference_id.to_string(),
            "",
        )?;
        return Err(format!(
            "cannot remove directory {} safely: {error}",
            destination.display()
        ));
    }
    sync_parent(destination)?;
    test_pause(if pending_kind == "dir_undo_pending" {
        "dir_undo_after_remove"
    } else {
        "dir_redo_after_remove"
    });
    journal::append(journal::next_id()?, completed_kind, &reference_id.to_string(), "")?;
    Ok(())
}

fn undo_directory_entry(entry: &Entry) -> Result<(), String> {
    match entry.kind.as_str() {
        "dir_track" => remove_directory_transaction(
            entry,
            "dir_undo_pending",
            "dir_restore",
            entry.id,
            Some(entry.snapshot.as_str()),
        ),
        "dir_deleted" => create_directory_transaction(
            entry,
            "dir_undo_pending",
            "dir_restore",
            entry.id,
        ),
        _ => Ok(()),
    }
}

fn redo_directory_entry(restore: &Entry) -> Result<(), String> {
    let original_id: u64 = restore.path.parse().map_err(|_| {
        format!("invalid directory recovery reference: {}", restore.path)
    })?;
    let original = journal::read_all()?
        .into_iter()
        .find(|entry| {
            (entry.kind == "dir_track" || entry.kind == "dir_deleted")
                && entry.id == original_id
        })
        .ok_or_else(|| format!("original directory event #{original_id} is missing"))?;
    match original.kind.as_str() {
        "dir_track" => create_directory_transaction(
            &original,
            "dir_redo_pending",
            "dir_redo",
            restore.id,
            Some(restore.snapshot.as_str()),
        ),
        "dir_deleted" => remove_directory_transaction(
            &original,
            "dir_redo_pending",
            "dir_redo",
            restore.id,
            Some(&restore.snapshot),
        ),
        _ => Ok(()),
    }
}

/// Resolve interrupted file, move, and directory transactions before accepting new work.
/// A transaction is completed only when the live file matches the intended snapshot;
/// it is aborted only when the original pre-operation state is still present.
pub fn reconcile_pending() -> Result<(), String> {
    let entries = journal::read_all()?;

    // Directory operations use staged sibling directories for creation. The
    // staged directory's device/inode identity is journaled before its atomic
    // rename, so recovery never adopts an unrelated directory at the target.
    for pending in entries.iter().filter(|entry| {
        entry.kind == "dir_undo_pending" || entry.kind == "dir_redo_pending"
    }) {
        let is_undo = pending.kind == "dir_undo_pending";
        let completed_kind = if is_undo { "dir_restore" } else { "dir_redo" };
        let aborted_kind = if is_undo { "dir_undo_abort" } else { "dir_redo_abort" };
        if entries.iter().any(|entry| {
            entry.id > pending.id
                && (entry.kind == completed_kind || entry.kind == aborted_kind)
                && entry.path == pending.path
        }) {
            continue;
        }

        let reference_id: u64 = pending.path.parse().map_err(|_| {
            format!("invalid directory transaction reference: {}", pending.path)
        })?;
        let original = (if is_undo {
            entries.iter().find(|entry| {
                (entry.kind == "dir_track" || entry.kind == "dir_deleted")
                    && entry.id == reference_id
            })
        } else {
            let restore = entries.iter().find(|entry| {
                entry.kind == "dir_restore" && entry.id == reference_id
            }).ok_or_else(|| format!(
                "directory redo #{} references missing restore history",
                pending.id
            ))?;
            let original_id: u64 = restore.path.parse().map_err(|_| {
                format!("invalid original directory reference: {}", restore.path)
            })?;
            entries.iter().find(|entry| {
                (entry.kind == "dir_track" || entry.kind == "dir_deleted")
                    && entry.id == original_id
            })
        })
        .ok_or_else(|| {
            format!(
                "directory transaction #{} references missing directory history",
                pending.id
            )
        })?;

        let creates_directory = if is_undo {
            original.kind == "dir_deleted"
        } else {
            original.kind == "dir_track"
        };
        let destination = Path::new(&original.path);
        let staged = staged_directory_path(destination, pending.id);
        let live_identity = match fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                use std::os::unix::fs::MetadataExt;
                Some(format!("{}:{}", metadata.dev(), metadata.ino()))
            }
            Ok(_) => return Err(format!(
                "unfinished directory transaction #{} conflicts with non-directory path {}; leaving it untouched",
                pending.id, destination.display()
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!(
                "cannot inspect directory {} during recovery: {error}",
                destination.display()
            )),
        };
        let staged_identity = match fs::symlink_metadata(&staged) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                use std::os::unix::fs::MetadataExt;
                Some(format!("{}:{}", metadata.dev(), metadata.ino()))
            }
            Ok(_) => return Err(format!(
                "temporary recovery path {} is not a directory",
                staged.display()
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!(
                "cannot inspect temporary directory {}: {error}",
                staged.display()
            )),
        };

        if creates_directory {
            if live_identity.as_deref() == Some(pending.snapshot.as_str()) {
                journal::append(
                    journal::next_id()?,
                    completed_kind,
                    &pending.path,
                    &pending.snapshot,
                )?;
            } else if live_identity.is_some() {
                return Err(format!(
                    "unfinished directory transaction #{} found a different directory at {}; leaving it untouched",
                    pending.id, destination.display()
                ));
            } else if staged_identity.as_deref() == Some(pending.snapshot.as_str()) {
                if !directory_is_empty(&staged)? {
                    return Err(format!(
                        "unfinished directory transaction #{} found contents in staging path {}; leaving them untouched",
                        pending.id, staged.display()
                    ));
                }
                rename_directory_noreplace(&staged, destination).map_err(|error| {
                    format!(
                        "cannot finish restoring directory {}: {error}",
                        destination.display()
                    )
                })?;
                sync_parent(destination)?;
                journal::append(
                    journal::next_id()?,
                    completed_kind,
                    &pending.path,
                    &pending.snapshot,
                )?;
            } else if staged_identity.is_none() {
                journal::append(journal::next_id()?, aborted_kind, &pending.path, "")?;
            } else {
                return Err(format!(
                    "unfinished directory transaction #{} has an unexpected staging directory {}; leaving it untouched",
                    pending.id, staged.display()
                ));
            }
        } else if live_identity.is_none() {
            journal::append(journal::next_id()?, completed_kind, &pending.path, "")?;
        } else if live_identity.as_deref() == Some(pending.snapshot.as_str()) {
            if directory_is_empty(destination)? {
                journal::append(journal::next_id()?, aborted_kind, &pending.path, "")?;
            } else {
                return Err(format!(
                    "unfinished directory transaction #{} found new contents in {}; leaving them untouched",
                    pending.id, destination.display()
                ));
            }
        } else {
            return Err(format!(
                "unfinished directory transaction #{} found a different directory at {}; leaving it untouched",
                pending.id, destination.display()
            ));
        }
    }

    // Move transactions have no content copy to compare: the two possible
    // filesystem states are identified by which side of the rename exists.
    for pending in entries
        .iter()
        .filter(|e| e.kind == "move_undo_pending" || e.kind == "move_redo_pending")
    {
        let is_undo = pending.kind == "move_undo_pending";
        let completed_kind = if is_undo { "restore" } else { "redo" };
        let aborted_kind = if is_undo {
            "move_undo_abort"
        } else {
            "move_redo_abort"
        };
        if entries.iter().any(|e| {
            e.id > pending.id
                && (e.kind == completed_kind || e.kind == aborted_kind)
                && e.path == pending.path
        }) {
            continue;
        }
        let reference_id: u64 = pending
            .path
            .parse()
            .map_err(|_| format!("invalid move transaction reference: {}", pending.path))?;
        let original = if is_undo {
            entries
                .iter()
                .find(|e| e.kind == "move" && e.id == reference_id)
        } else {
            let restore = entries
                .iter()
                .find(|e| e.kind == "restore" && e.id == reference_id)
                .ok_or_else(|| {
                    format!(
                        "move redo #{} references missing restore history",
                        pending.id
                    )
                })?;
            let move_id: u64 = restore
                .path
                .parse()
                .map_err(|_| format!("invalid original move reference: {}", restore.path))?;
            entries.iter().find(|e| e.kind == "move" && e.id == move_id)
        }
        .ok_or_else(|| {
            format!(
                "move transaction #{} references missing move history",
                pending.id
            )
        })?;
        let old = Path::new(&original.path);
        let new = Path::new(&original.snapshot);
        let (source, destination) = if is_undo { (new, old) } else { (old, new) };
        let source_exists = source.exists();
        let destination_exists = destination.exists();
        let id = journal::next_id()?;

        match (source_exists, destination_exists) {
            (true, false) => {
                // Confirm the source is still the file recorded before
                // declaring that the rename never happened.
                ensure_reconciled_move_identity(source, source)?;
                journal::append(id, aborted_kind, &pending.path, "")?;
            }
            (false, true) => {
                // Confirm the destination is the original file before
                // accepting the rename as completed.
                ensure_reconciled_move_identity(source, destination)?;
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

    for pending in entries
        .iter()
        .filter(|e| e.kind == "undo_pending" || e.kind == "redo_pending")
    {
        let completed_kind = if pending.kind == "undo_pending" {
            "restore"
        } else {
            "redo"
        };
        let aborted_kind = if pending.kind == "undo_pending" {
            "undo_abort"
        } else {
            "redo_abort"
        };
        let already_resolved = entries.iter().any(|e| {
            e.id > pending.id
                && (e.kind == completed_kind || e.kind == aborted_kind)
                && e.path == pending.path
        });
        if already_resolved {
            continue;
        }

        let reference_id: u64 = pending
            .path
            .parse()
            .map_err(|_| format!("invalid recovery transaction reference: {}", pending.path))?;
        let referenced = if pending.kind == "undo_pending" {
            entries
                .iter()
                .find(|e| e.kind == "snap" && e.id == reference_id)
        } else {
            entries
                .iter()
                .find(|e| e.kind == "restore" && e.id == reference_id)
        }
        .ok_or_else(|| {
            format!(
                "recovery transaction #{} references missing history",
                pending.id
            )
        })?;
        let destination = if pending.kind == "undo_pending" {
            referenced.path.clone()
        } else {
            entries
                .iter()
                .find(|e| e.kind == "snap" && e.id.to_string() == referenced.path)
                .ok_or_else(|| format!("original entry #{} is missing", referenced.path))?
                .path
                .clone()
        };
        let intended = referenced.snapshot.as_str();
        let live = Path::new(&destination);
        let id = journal::next_id()?;

        if snapshot::matches(intended, live)? {
            if baseline::exists(live) && live.is_file() {
                baseline::store(live)?;
            }
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
    if entry.kind == "dir_track" || entry.kind == "dir_deleted" {
        return undo_directory_entry(entry);
    }
    if entry.kind == "move" {
        let old = Path::new(&entry.path);
        let new = Path::new(&entry.snapshot);
        if old.exists() {
            return Err(format!(
                "cannot undo move: {} already exists",
                old.display()
            ));
        }
        if !new.exists() {
            return Err(format!("cannot undo move: {} is missing", new.display()));
        }
        ensure_move_identity(new)?;
        let intent_id = journal::next_id()?;
        journal::append(intent_id, "move_undo_pending", &entry.id.to_string(), "")?;
        test_pause("move_undo_after_intent");
        if let Some(parent) = old.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        if let Err(err) = fs::rename(new, old) {
            journal::append(
                journal::next_id()?,
                "move_undo_abort",
                &entry.id.to_string(),
                "",
            )?;
            return Err(format!(
                "cannot move {} back to {}: {err}",
                new.display(),
                old.display()
            ));
        }
        sync_parent(old)?;
        if new.parent() != old.parent() {
            sync_parent(new)?;
        }
        test_pause("move_undo_after_rename");
        reconcile_move_baseline(new, old)?;
        journal::append(journal::next_id()?, "restore", &entry.id.to_string(), "")?;
        return Ok(());
    }
    if entry.kind != "snap" {
        return Ok(());
    }

    let dest = Path::new(&entry.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let id = journal::next_id()?;
    let backup = if dest.is_file() {
        snapshot::store(id, dest)?
    } else {
        String::new()
    };
    journal::append(id + 1, "undo_pending", &entry.id.to_string(), &backup)?;
    test_pause("undo_after_intent");
    if let Err(err) = apply_snapshot(&entry.snapshot, &entry.path, &backup) {
        // If the original state is still present, record a clean abort. If the
        // atomic rename happened, leave the intent for startup reconciliation.
        if snapshot::matches(&backup, dest).unwrap_or(false) {
            let _ = journal::append(journal::next_id()?, "undo_abort", &entry.id.to_string(), "");
        } else if let Ok(true) = snapshot::matches(&entry.snapshot, dest) {
            if had_baseline && dest.is_file() {
                let _ = baseline::store(dest);
            }
            let _ = journal::append(
                journal::next_id()?,
                "restore",
                &entry.id.to_string(),
                &backup,
            );
        }
        return Err(err);
    }
    test_pause("undo_after_apply");
    if had_baseline {
        baseline::store(dest).map_err(|err| {
            format!("could not update recovery baseline; transaction will be reconciled: {err}")
        })?;
    }
    journal::append(
        journal::next_id()?,
        "restore",
        &entry.id.to_string(),
        &backup,
    )?;
    Ok(())
}

pub fn undo_last() -> Result<Option<Entry>, String> {
    let entry = match journal::active_snapshots()?
        .into_iter()
        .chain(journal::active_moves()?.into_iter())
        .chain(journal::active_directories()?.into_iter())
        .max_by_key(|e| e.id)
    {
        Some(entry) => entry,
        None => return Ok(None),
    };
    undo_entry(&entry)?;
    Ok(Some(entry))
}

/// Reapply the state which existed immediately before the latest undo.
pub fn redo_last() -> Result<Option<Entry>, String> {
    let file_restore = journal::pending_redo()?;
    let directory_restore = journal::pending_directory_redo()?;
    let restore = match (file_restore, directory_restore) {
        (Some(file), Some(directory)) => {
            if file.id > directory.id { file } else { directory }
        }
        (Some(file), None) => file,
        (None, Some(directory)) => directory,
        (None, None) => return Ok(None),
    };
    if restore.kind == "dir_restore" {
        redo_directory_entry(&restore)?;
        return Ok(Some(restore));
    }
    let original = journal::read_all()?
        .into_iter()
        .find(|e| (e.kind == "snap" || e.kind == "move") && e.id.to_string() == restore.path)
        .ok_or_else(|| format!("original entry #{} is missing", restore.path))?;
    let id = journal::next_id()?;

    if original.kind == "move" {
        let old = Path::new(&original.path);
        let new = Path::new(&original.snapshot);
        if new.exists() {
            return Err(format!(
                "cannot redo move: {} already exists",
                new.display()
            ));
        }
        if !old.exists() {
            return Err(format!("cannot redo move: {} is missing", old.display()));
        }
        ensure_move_identity(old)?;
        journal::append(id, "move_redo_pending", &restore.id.to_string(), "")?;
        test_pause("move_redo_after_intent");
        if let Some(parent) = new.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        if let Err(err) = fs::rename(old, new) {
            journal::append(
                journal::next_id()?,
                "move_redo_abort",
                &restore.id.to_string(),
                "",
            )?;
            return Err(format!(
                "cannot move {} to {}: {err}",
                old.display(),
                new.display()
            ));
        }
        sync_parent(new)?;
        if old.parent() != new.parent() {
            sync_parent(old)?;
        }
        test_pause("move_redo_after_rename");
        reconcile_move_baseline(old, new)?;
        journal::append(journal::next_id()?, "redo", &restore.id.to_string(), "")?;
        return Ok(Some(restore));
    }

    let dest = Path::new(&original.path);
    let had_baseline = ensure_no_conflict(dest)?;
    let backup = if dest.is_file() {
        snapshot::store(id, dest)?
    } else {
        String::new()
    };
    journal::append(id + 1, "redo_pending", &restore.id.to_string(), &backup)?;
    test_pause("redo_after_intent");
    if let Err(err) = apply_snapshot(&restore.snapshot, &original.path, &backup) {
        if snapshot::matches(&backup, dest).unwrap_or(false) {
            let _ = journal::append(
                journal::next_id()?,
                "redo_abort",
                &restore.id.to_string(),
                "",
            );
        } else if let Ok(true) = snapshot::matches(&restore.snapshot, dest) {
            if had_baseline && dest.is_file() {
                let _ = baseline::store(dest);
            }
            let _ = journal::append(
                journal::next_id()?,
                "redo",
                &restore.id.to_string(),
                &backup,
            );
        }
        return Err(err);
    }
    test_pause("redo_after_apply");
    if had_baseline {
        baseline::store(dest).map_err(|err| {
            format!("could not update recovery baseline; transaction will be reconciled: {err}")
        })?;
    }
    journal::append(
        journal::next_id()?,
        "redo",
        &restore.id.to_string(),
        &backup,
    )?;
    Ok(Some(restore))
}

/// Rewind active saved changes newer than id, newest first, without truncating history.
pub fn rewind_to(id: u64) -> Result<usize, String> {
    let mut newer: Vec<Entry> = journal::active_snapshots()?
        .into_iter()
        .chain(journal::active_moves()?.into_iter())
        .chain(journal::active_directories()?.into_iter())
        .filter(|e| e.id > id)
        .collect();
    newer.sort_by_key(|e| std::cmp::Reverse(e.id));
    let mut done = 0;
    for entry in newer {
        undo_entry(&entry)?;
        done += 1;
    }
    Ok(done)
}
