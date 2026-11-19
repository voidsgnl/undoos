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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

pub struct Watcher {
    roots: Vec<PathBuf>,
    home: Option<PathBuf>,
    ignore: Vec<String>,
    max_bytes: u64,
    settle: Duration,
    gap: Duration,
    /// (modified time, size) from the last look, so unchanged files are skipped quickly.
    cache: HashMap<PathBuf, (SystemTime, u64)>,
    last_saved: HashMap<PathBuf, Instant>,
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
            gap: Duration::from_secs(cfg.min_gap_s),
            cache: HashMap::new(),
            last_saved: HashMap::new(),
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Look at everything once and return what happened.
    pub fn scan(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        // If any subtree could not be inspected, absence inside it is unknown,
        // not proof of deletion. This prevents permission errors from creating
        // false recovery events for files that are still present.
        let mut incomplete = HashSet::new();
        let roots = self.roots.clone();
        for root in &roots {
            self.walk(root, &mut seen, &mut incomplete, &mut events);
        }
        self.find_deleted(&seen, &incomplete, &mut events);
        events
    }

    fn walk(
        &mut self,
        dir: &Path,
        seen: &mut HashSet<PathBuf>,
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
                self.walk(&path, seen, incomplete, events);
            } else if meta.is_file() && meta.len() <= self.max_bytes {
                seen.insert(path.clone());
                if let Err(err) = self.check_file(&path, &meta, events) {
                    events.push(Event::error(&path, &err));
                }
            }
        }
    }

    fn check_file(
        &mut self,
        path: &Path,
        meta: &fs::Metadata,
        events: &mut Vec<Event>,
    ) -> Result<(), String> {
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let stamp = (modified, meta.len());
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

        // Changed too soon after the last saved version? Fold it into the next one.
        if let Some(when) = self.last_saved.get(path) {
            if when.elapsed() < self.gap {
                return Ok(());
            }
        }

        save_old(path)?;
        baseline::store(path)?;
        self.last_saved.insert(path.to_path_buf(), Instant::now());
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
                    self.last_saved.remove(&path);
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
