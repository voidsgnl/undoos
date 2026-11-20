//! The shelf: saved copies of files, one per journal entry.
//!
//! Today a snapshot is a plain copy. Deduplication and deltas are on the roadmap.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::journal;

fn dir() -> PathBuf {
    journal::home().join("snapshots")
}

pub fn path_for(name: &str) -> PathBuf {
    dir().join(name)
}

/// Copy `src` onto the shelf under the name of journal entry `id`.
pub fn store(id: u64, src: &Path) -> Result<String, String> {
    fs::create_dir_all(dir()).map_err(|e| format!("cannot create snapshot dir: {e}"))?;
    let name = id.to_string();
    let destination = path_for(&name);
    let temporary = destination.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    let result = (|| -> io::Result<()> {
        let mut input = fs::File::open(src)?;
        let before = input.metadata()?;
        let permissions = before.permissions();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        let after = fs::metadata(src)?;
        let opened_after = input.metadata()?;
        if !same_source_version(&before, &after) || !same_source_version(&before, &opened_after) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "source changed while being snapshotted",
            ));
        }
        fs::set_permissions(&temporary, permissions)?;
        output.sync_all()?;
        fs::rename(&temporary, &destination)?;
        // Persist the snapshot's directory entry, not just its contents.
        OpenOptions::new().read(true).open(dir())?.sync_all()?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot snapshot {}: {err}", src.display()));
    }
    Ok(name)
}

fn same_source_version(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    if a.len() != b.len() || a.modified().ok() != b.modified().ok() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Whether a live path exactly matches a saved snapshot (empty means absent).
pub fn matches(name: &str, live: &Path) -> Result<bool, String> {
    if name.is_empty() {
        return Ok(!live.exists());
    }
    let saved = path_for(name);
    if !saved.is_file() || !live.is_file() {
        return Ok(false);
    }
    let saved_meta = fs::metadata(&saved).map_err(|e| format!("cannot inspect snapshot: {e}"))?;
    let live_meta =
        fs::metadata(live).map_err(|e| format!("cannot inspect {}: {e}", live.display()))?;
    if saved_meta.len() != live_meta.len() {
        return Ok(false);
    }
    let a = fs::read(saved).map_err(|e| format!("cannot read snapshot: {e}"))?;
    let b = fs::read(live).map_err(|e| format!("cannot read {}: {e}", live.display()))?;
    Ok(a == b)
}

pub fn remove(name: &str) {
    let _ = fs::remove_file(path_for(name));
}
