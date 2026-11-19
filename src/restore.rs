//! The undo logic: puts saved copies back where they came from.

use std::fs;
use std::path::Path;

use crate::journal::{self, Entry};
use crate::snapshot;

/// Put one saved file back. Checkpoints have nothing to restore.
pub fn undo_entry(entry: &Entry) -> Result<(), String> {
    if entry.kind != "snap" {
        return Ok(());
    }
    let src = snapshot::path_for(&entry.snapshot);
    let dest = Path::new(&entry.path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    fs::copy(&src, dest).map_err(|e| format!("cannot restore {}: {e}", entry.path))?;
    Ok(())
}

/// Undo the most recent saved file and forget that entry.
pub fn undo_last() -> Result<Option<Entry>, String> {
    let entries = journal::read_all()?;
    let entry = match entries.iter().rev().find(|e| e.kind == "snap") {
        Some(e) => e.clone(),
        None => return Ok(None),
    };
    undo_entry(&entry)?;
    journal::remove(entry.id)?;
    snapshot::remove(&entry.snapshot);
    Ok(Some(entry))
}

/// Undo everything newer than `id`, newest first, so the oldest copy of each
/// file is the one that ends up on disk.
pub fn rewind_to(id: u64) -> Result<usize, String> {
    let newer: Vec<Entry> = journal::read_all()?
        .into_iter()
        .filter(|e| e.id > id)
        .collect();
    for entry in newer.iter().rev() {
        undo_entry(entry)?;
    }
    let removed = journal::truncate_after(id)?;
    for entry in &removed {
        if !entry.snapshot.is_empty() {
            snapshot::remove(&entry.snapshot);
        }
    }
    Ok(removed.len())
}
