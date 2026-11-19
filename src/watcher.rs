//! Watches folders and quietly saves the old version of anything that changes
//! or disappears.
//!
//! Every scan walks the watched folders and compares each file with its
//! baseline (see `baseline.rs`):
//!
//! - never seen before: remember it ("tracking"), save nothing;
//! - contents changed: put the old version on the shelf ("saved");
//! - file vanished: put its last known version on the shelf ("deleted").

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{name_matches, Config};
use crate::{baseline, journal, snapshot};

/// One thing the watcher did, for the daemon to print.
pub struct Event {
    pub kind: &'static str,
    pub text: String,
}

impl Event {
    fn new(kind: &'static str, path: &Path) -> Event {
        Event {
            kind,
            text: path.display().to_string(),
        }
    }

    fn error(path: &Path, message: &str) -> Event {
        Event {
            kind: "error",
            text: format!("{}: {message}", path.display()),
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:<9} {}", self.kind, self.text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    size: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn file_stamp(meta: &fs::Metadata) -> FileStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileStamp {
            modified: meta.modified().unwrap_or(UNIX_EPOCH),
            size: meta.len(),
            device: meta.dev(),
            inode: meta.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        FileStamp { modified: meta.modified().unwrap_or(UNIX_EPOCH), size: meta.len() }
    }
}

pub struct Watcher {
    roots: Vec<PathBuf>,
    home: Option<PathBuf>,
    ignore: Vec<String>,
    max_bytes: u64,
    settle: Duration,
    /// File identity and metadata from the last look, so replacements are noticed too.
    cache: HashMap<PathBuf, FileStamp>,
}

impl Watcher {
    pub fn new(cfg: &Config) -> Watcher {
        // Lifeline's own data folder must never be watched, or saving a
        // version would count as a change and loop forever.
        let _ = fs::create_dir_all(journal::home());
        let home = fs::canonicalize(journal::home()).ok();

        let mut roots = Vec::new();
        for p in &cfg.paths {
            match fs::canonicalize(p) {
                Ok(real) => roots.push(real),
                Err(e) => eprintln!("lifeline-daemon: skipping {}: {e}", p.display()),
            }
        }

        Watcher {
            roots,
            home,
            ignore: cfg.ignore.clone(),
            max_bytes: cfg.max_file_mb.saturating_mul(1024 * 1024),
            settle: Duration::from_millis(cfg.settle_ms),
            cache: HashMap::new(),
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Look at everything once and return what happened.
    pub fn scan(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        let mut incomplete = HashSet::new();
        let roots = self.roots.clone();
        for root in &roots {
            self.walk(root, &mut seen, &mut found, &mut incomplete, &mut events);
        }
        self.detect_moves(&seen, &found, &mut events);
        for path in found {
            match fs::symlink_metadata(&path) {
                Ok(meta) => if let Err(err) = self.check_file(&path, &meta, &mut events) {
                    events.push(Event::error(&path, &err));
                },
                Err(err) => events.push(Event::error(&path, &format!("cannot inspect entry: {err}"))),
            }
        }
        self.find_deleted(&seen, &incomplete, &mut events);
        events
    }

    fn walk(
        &mut self,
        dir: &Path,
        seen: &mut HashSet<PathBuf>,
        found: &mut Vec<PathBuf>,
        incomplete: &mut HashSet<PathBuf>,
        events: &mut Vec<Event>,
    ) {
        let read = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(err) => {
                incomplete.insert(dir.to_path_buf());
                events.push(Event::error(dir, &format!("cannot inspect directory: {err}")));
                return;
            }
        };
        for item in read {
            let entry = match item {
                Ok(entry) => entry,
                Err(err) => {
                    incomplete.insert(dir.to_path_buf());
                    events.push(Event::error(dir, &format!("cannot read directory entry: {err}")));
                    continue;
                }
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.ignore.iter().any(|p| name_matches(p, &name)) {
                continue;
            }
            if let Some(home) = &self.home {
                if path.starts_with(home) {
                    continue;
                }
            }
            // symlink_metadata: never follow links out of the watched folders.
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(err) => {
                    incomplete.insert(dir.to_path_buf());
                    events.push(Event::error(&path, &format!("cannot inspect entry: {err}")));
                    continue;
                }
            };
            if meta.is_dir() {
                self.walk(&path, seen, found, incomplete, events);
            } else if meta.is_file() && meta.len() <= self.max_bytes {
                seen.insert(path.clone());
                found.push(path);
            }
        }
    }

    /// Recognize a move only when the kernel reports identical device/inode.
    fn detect_moves(&mut self, seen: &HashSet<PathBuf>, found: &[PathBuf], events: &mut Vec<Event>) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mut destinations = HashMap::<(u64, u64), PathBuf>::new();
            for path in found {
                if baseline::exists(path) { continue; }
                if let Ok(meta) = fs::symlink_metadata(path) {
                    destinations.insert((meta.dev(), meta.ino()), path.clone());
                }
            }
            let old_paths: Vec<PathBuf> = self.cache.keys()
                .filter(|path| !seen.contains(*path) && baseline::exists(path))
                .cloned().collect();
            for old in old_paths {
                let stamp = match self.cache.get(&old) { Some(s) => *s, None => continue };
                let new = match destinations.remove(&(stamp.device, stamp.inode)) { Some(p) => p, None => continue };
                let (old_text, new_text) = match (old.to_str(), new.to_str()) { (Some(a), Some(b)) => (a,b), _ => continue };
                if let Err(err) = baseline::relocate(&old, &new) {
                    events.push(Event::error(&new, &err)); continue;
                }
                let id = match journal::next_id() {
                    Ok(id) => id,
                    Err(err) => { events.push(Event::error(&new, &err)); continue; }
                };
                if let Err(err) = journal::append(id, "move", old_text, new_text) {
                    events.push(Event::error(&new, &err)); continue;
                }
                self.cache.remove(&old);
                if let Ok(meta) = fs::symlink_metadata(&new) { self.cache.insert(new.clone(), file_stamp(&meta)); }
                events.push(Event { kind: "moved", text: format!("{} -> {}", old.display(), new.display()) });
            }
        }
        #[cfg(not(unix))]
        { let _ = (seen, found, events); }
    }

    fn check_file(
        &mut self,
        path: &Path,
        meta: &fs::Metadata,
        events: &mut Vec<Event>,
    ) -> Result<(), String> {
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let stamp = file_stamp(meta);
        if self.cache.get(path) == Some(&stamp) {
            return Ok(());
        }

        // Still being written? Look again next scan.
        if let Ok(age) = SystemTime::now().duration_since(modified) {
            if age < self.settle {
                return Ok(());
            }
        }

        if !baseline::exists(path) {
            baseline::store(path)?;
            self.cache.insert(path.to_path_buf(), stamp);
            events.push(Event::new("tracking", path));
            return Ok(());
        }

        if baseline::same_as(path)? {
            // Touched but not changed.
            self.cache.insert(path.to_path_buf(), stamp);
            return Ok(());
        }

        save_old(path)?;
        baseline::store(path)?;
        self.cache.insert(path.to_path_buf(), stamp);
        events.push(Event::new("saved", path));
        Ok(())
    }

    fn find_deleted(
        &mut self,
        seen: &HashSet<PathBuf>,
        incomplete: &HashSet<PathBuf>,
        events: &mut Vec<Event>,
    ) {
        let known = match baseline::list_all() {
            Ok(k) => k,
            Err(e) => {
                events.push(Event {
                    kind: "error",
                    text: e,
                });
                return;
            }
        };
        for text in known {
            let path = PathBuf::from(&text);
            if seen.contains(&path)
                || path.exists()
                || incomplete.iter().any(|dir| path.starts_with(dir))
            {
                continue;
            }
            // Only trust "deleted" if the watched folder itself is still there.
            // An unplugged drive must not look like everything was deleted.
            if !self.roots.iter().any(|r| r.exists() && path.starts_with(r)) {
                continue;
            }
            match save_old(&path) {
                Ok(()) => {
                    baseline::forget(&path);
                    self.cache.remove(&path);
                    events.push(Event::new("deleted", &path));
                }
                Err(err) => events.push(Event::error(&path, &err)),
            }
        }
    }
}

/// Put the baseline copy of `path` on the shelf as a new undo point.
fn save_old(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "path is not valid UTF-8".to_string())?;
    let id = journal::next_id()?;
    let name = snapshot::store(id, &baseline::file_for(path))?;
    if let Err(err) = journal::append(id, "snap", text, &name) {
        snapshot::remove(&name);
        return Err(err);
    }
    Ok(())
}
