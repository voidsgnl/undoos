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
        fs::copy(src, &temporary)?;
        let file = OpenOptions::new().read(true).open(&temporary)?;
        file.sync_all()?;
        fs::rename(&temporary, &destination)
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot snapshot {}: {err}", src.display()));
    }
    Ok(name)
}

pub fn remove(name: &str) {
    let _ = fs::remove_file(path_for(name));
}
