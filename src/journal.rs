//! The notebook: an append-only list of everything lifeline has saved.
//!
//! Stored as tab-separated lines in `<home>/journal.tsv`:
//! `id <TAB> unix_time <TAB> kind <TAB> path_or_name <TAB> snapshot`

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub timestamp: u64,
    /// "snap" (a saved file) or "checkpoint" (a named marker).
    pub kind: String,
    /// File path for "snap", marker name for "checkpoint".
    pub path: String,
    /// Snapshot file name on the shelf (empty for checkpoints).
    pub snapshot: String,
}

impl Entry {
    fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.id, self.timestamp, self.kind, self.path, self.snapshot
        )
    }

    fn from_line(line: &str) -> Option<Entry> {
        let mut parts = line.splitn(5, '\t');
        Some(Entry {
            id: parts.next()?.parse().ok()?,
            timestamp: parts.next()?.parse().ok()?,
            kind: parts.next()?.to_string(),
            path: parts.next()?.to_string(),
            snapshot: parts.next()?.to_string(),
        })
    }

    pub fn display(&self) -> String {
        let when = ago(self.timestamp);
        if self.kind == "checkpoint" {
            format!("#{:<4} {:>8}  checkpoint  {}", self.id, when, self.path)
        } else {
            format!("#{:<4} {:>8}  {}  {}", self.id, when, self.kind, self.path)
        }
    }
}

/// Where lifeline keeps its data. Override with `LIFELINE_HOME`.
pub fn home() -> PathBuf {
    if let Ok(p) = env::var("LIFELINE_HOME") {
        return PathBuf::from(p);
    }
    if let Ok(h) = env::var("HOME") {
        return PathBuf::from(h).join(".local/share/lifeline");
    }
    PathBuf::from(".lifeline")
}

fn journal_path() -> PathBuf {
    home().join("journal.tsv")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ago(ts: u64) -> String {
    let d = now().saturating_sub(ts);
    if d < 60 {
        format!("{d}s ago")
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}

pub fn read_all() -> Result<Vec<Entry>, String> {
    let path = journal_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read journal: {e}"))?;
    Ok(text.lines().filter_map(Entry::from_line).collect())
}

pub fn write_all(entries: &[Entry]) -> Result<(), String> {
    fs::create_dir_all(home()).map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let mut out = String::new();
    for entry in entries {
        out.push_str(&entry.to_line());
        out.push('\n');
    }
    fs::write(journal_path(), out).map_err(|e| format!("cannot write journal: {e}"))
}

pub fn next_id() -> Result<u64, String> {
    Ok(read_all()?.iter().map(|e| e.id).max().unwrap_or(0) + 1)
}

pub fn append(id: u64, kind: &str, path: &str, snapshot: &str) -> Result<Entry, String> {
    if path.contains('\t') || path.contains('\n') {
        return Err("paths and names cannot contain tabs or newlines".to_string());
    }
    fs::create_dir_all(home()).map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let entry = Entry {
        id,
        timestamp: now(),
        kind: kind.to_string(),
        path: path.to_string(),
        snapshot: snapshot.to_string(),
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path())
        .map_err(|e| format!("cannot open journal: {e}"))?;
    writeln!(file, "{}", entry.to_line()).map_err(|e| format!("cannot write journal: {e}"))?;
    Ok(entry)
}

/// Remove one entry from the journal.
pub fn remove(id: u64) -> Result<(), String> {
    let kept: Vec<Entry> = read_all()?.into_iter().filter(|e| e.id != id).collect();
    write_all(&kept)
}

/// Drop every entry newer than `id` and return what was dropped.
pub fn truncate_after(id: u64) -> Result<Vec<Entry>, String> {
    let (keep, removed): (Vec<Entry>, Vec<Entry>) =
        read_all()?.into_iter().partition(|e| e.id <= id);
    write_all(&keep)?;
    Ok(removed)
}

/// The id of the most recent checkpoint with this name.
pub fn find_checkpoint(name: &str) -> Result<Option<u64>, String> {
    Ok(read_all()?
        .iter()
        .rev()
        .find(|e| e.kind == "checkpoint" && e.path == name)
        .map(|e| e.id))
}
