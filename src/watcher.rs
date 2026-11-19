#![allow(dead_code)]
//! Watches for changes. Not wired up yet: the daemon will call this.
//!
//! This first version is a simple poller that remembers modification times
//! and reports which files changed between scans.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct Watcher {
    known: HashMap<PathBuf, SystemTime>,
}

impl Watcher {
    pub fn new() -> Self {
        Watcher {
            known: HashMap::new(),
        }
    }

    /// Scan `dir` recursively and return files modified since the last scan.
    /// The first scan only records what is there and reports nothing.
    pub fn scan(&mut self, dir: &Path) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        self.walk(dir, &mut changed);
        changed
    }

    fn walk(&mut self, dir: &Path, changed: &mut Vec<PathBuf>) {
        let read = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                self.walk(&path, changed);
            } else if let Ok(modified) = meta.modified() {
                if let Some(prev) = self.known.insert(path.clone(), modified) {
                    if prev != modified {
                        changed.push(path);
                    }
                }
            }
        }
    }
}
