//! Append-only journal of snapshots, checkpoints, and compensating recovery actions.
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub timestamp: u64,
    pub kind: String,
    pub path: String,
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
        let mut p = line.splitn(5, '\t');
        Some(Entry {
            id: p.next()?.parse().ok()?,
            timestamp: p.next()?.parse().ok()?,
            kind: p.next()?.to_string(),
            path: p.next()?.to_string(),
            snapshot: p.next()?.to_string(),
        })
    }
    pub fn display(&self) -> String {
        let when = ago(self.timestamp);
        match self.kind.as_str() {
            "checkpoint" => format!("#{:<4} {:>8}  checkpoint  {}", self.id, when, self.path),
            "restore" => format!(
                "#{:<4} {:>8}  restored    entry #{}",
                self.id, when, self.path
            ),
            "redo" => format!(
                "#{:<4} {:>8}  redone      restore #{}",
                self.id, when, self.path
            ),
            "move" => format!(
                "#{:<4} {:>8}  moved       {} -> {}",
                self.id, when, self.path, self.snapshot
            ),
            _ => format!("#{:<4} {:>8}  {}  {}", self.id, when, self.kind, self.path),
        }
    }
}
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
    let p = journal_path();
    if !p.exists() {
        return Ok(Vec::new());
    }
    let t = fs::read_to_string(&p).map_err(|e| format!("cannot read journal: {e}"))?;
    Ok(t.lines().filter_map(Entry::from_line).collect())
}
pub fn next_id() -> Result<u64, String> {
    Ok(read_all()?.iter().map(|e| e.id).max().unwrap_or(0) + 1)
}
pub fn append(id: u64, kind: &str, path: &str, snapshot: &str) -> Result<Entry, String> {
    if path.contains('\t') || path.contains('\n') {
        return Err("paths and names cannot contain tabs or newlines".into());
    }
    fs::create_dir_all(home()).map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let e = Entry {
        id,
        timestamp: now(),
        kind: kind.into(),
        path: path.into(),
        snapshot: snapshot.into(),
    };
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path())
        .map_err(|e| format!("cannot open journal: {e}"))?;
    writeln!(f, "{}", e.to_line()).map_err(|e| format!("cannot write journal: {e}"))?;
    // Do not report a journal event as committed while it is only in cache.
    f.sync_all()
        .map_err(|e| format!("cannot sync journal: {e}"))?;
    // Persist creation of journal.tsv as well as the appended record.
    let path = journal_path();
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    OpenOptions::new()
        .read(true)
        .open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("cannot sync journal directory: {e}"))?;
    Ok(e)
}
/// Snapshot entries not currently undone (or whose undo has been redone).
pub fn active_snapshots() -> Result<Vec<Entry>, String> {
    let entries = read_all()?;
    let mut latest = std::collections::HashMap::<u64, u64>::new();
    let mut redone = std::collections::HashSet::<u64>::new();
    for e in &entries {
        if e.kind == "restore" {
            if let Ok(id) = e.path.parse() {
                latest.insert(id, e.id);
            }
        } else if e.kind == "redo" {
            if let Ok(id) = e.path.parse() {
                redone.insert(id);
            }
        }
    }
    Ok(entries
        .into_iter()
        .filter(|e| {
            e.kind == "snap"
                && match latest.get(&e.id) {
                    None => true,
                    Some(r) => redone.contains(r),
                }
        })
        .collect())
}
pub fn pending_redo() -> Result<Option<Entry>, String> {
    let entries = read_all()?;
    let redone: std::collections::HashSet<u64> = entries
        .iter()
        .filter(|e| e.kind == "redo")
        .filter_map(|e| e.path.parse().ok())
        .collect();
    Ok(entries
        .into_iter()
        .rev()
        .find(|e| e.kind == "restore" && !redone.contains(&e.id)))
}
pub fn find_checkpoint(name: &str) -> Result<Option<u64>, String> {
    Ok(read_all()?
        .iter()
        .rev()
        .find(|e| e.kind == "checkpoint" && e.path == name)
        .map(|e| e.id))
}

/// Directory presence events which have not yet been undone (or whose undo was redone).
pub fn active_directories() -> Result<Vec<Entry>, String> {
    let entries = read_all()?;
    let mut latest = std::collections::HashMap::<u64, u64>::new();
    let mut redone = std::collections::HashSet::<u64>::new();
    let mut redo_identities = std::collections::HashMap::<u64, String>::new();
    for e in &entries {
        if e.kind == "dir_restore" {
            if let Ok(id) = e.path.parse() {
                latest.insert(id, e.id);
            }
        } else if e.kind == "dir_redo" {
            if let Ok(restore_id) = e.path.parse() {
                redone.insert(restore_id);
                if let Some(restore) = entries
                    .iter()
                    .find(|candidate| candidate.kind == "dir_restore" && candidate.id == restore_id)
                {
                    if let Ok(original_id) = restore.path.parse() {
                        redo_identities.insert(original_id, e.snapshot.clone());
                    }
                }
            }
        }
    }
    Ok(entries
        .into_iter()
        .filter(|e| {
            (e.kind == "dir_deleted" || (e.kind == "dir_track" && !e.snapshot.is_empty()))
                && match latest.get(&e.id) {
                    None => true,
                    Some(r) => redone.contains(r),
                }
        })
        .map(|mut entry| {
            if let Some(identity) = redo_identities.get(&entry.id) {
                if !identity.is_empty() {
                    entry.snapshot = identity.clone();
                }
            }
            entry
        })
        .collect())
}

/// Most recent directory undo which has not yet been redone.
pub fn pending_directory_redo() -> Result<Option<Entry>, String> {
    let entries = read_all()?;
    let redone: std::collections::HashSet<u64> = entries
        .iter()
        .filter(|e| e.kind == "dir_redo")
        .filter_map(|e| e.path.parse().ok())
        .collect();
    Ok(entries
        .into_iter()
        .rev()
        .find(|e| e.kind == "dir_restore" && !redone.contains(&e.id)))
}

/// Directory permission changes which are currently active.
pub fn active_directory_modes() -> Result<Vec<Entry>, String> {
    let entries = read_all()?;
    let mut latest = std::collections::HashMap::<u64, u64>::new();
    let mut redone = std::collections::HashSet::<u64>::new();
    for entry in &entries {
        if entry.kind == "dir_mode_restore" {
            if let Ok(original_id) = entry.path.parse() {
                latest.insert(original_id, entry.id);
            }
        } else if entry.kind == "dir_mode_redo" {
            if let Ok(restore_id) = entry.path.parse() {
                redone.insert(restore_id);
            }
        }
    }
    Ok(entries
        .into_iter()
        .filter(|entry| {
            entry.kind == "dir_mode"
                && match latest.get(&entry.id) {
                    None => true,
                    Some(restore_id) => redone.contains(restore_id),
                }
        })
        .collect())
}

/// Most recent directory-mode undo which has not yet been redone.
pub fn pending_directory_mode_redo() -> Result<Option<Entry>, String> {
    let entries = read_all()?;
    let redone: std::collections::HashSet<u64> = entries
        .iter()
        .filter(|entry| entry.kind == "dir_mode_redo")
        .filter_map(|entry| entry.path.parse().ok())
        .collect();
    Ok(entries
        .into_iter()
        .rev()
        .find(|entry| entry.kind == "dir_mode_restore" && !redone.contains(&entry.id)))
}

/// Move entries which have not yet been undone (or whose undo was redone).
pub fn active_moves() -> Result<Vec<Entry>, String> {
    let entries = read_all()?;
    let mut latest = std::collections::HashMap::<u64, u64>::new();
    let mut redone = std::collections::HashSet::<u64>::new();
    for e in &entries {
        if e.kind == "restore" {
            if let Ok(id) = e.path.parse() {
                latest.insert(id, e.id);
            }
        } else if e.kind == "redo" {
            if let Ok(id) = e.path.parse() {
                redone.insert(id);
            }
        }
    }
    Ok(entries
        .into_iter()
        .filter(|e| {
            e.kind == "move"
                && match latest.get(&e.id) {
                    None => true,
                    Some(r) => redone.contains(r),
                }
        })
        .collect())
}
