//! Reads the daemon's config file: a small, dependency-free slice of TOML.
//!
//! Supported: `[section]` headers, `key = 123`, `key = ["a", "b"]` (a list must
//! fit on one line) and `#` comments. Unknown keys are ignored.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    /// Folders to watch (`[watch] paths`).
    pub paths: Vec<PathBuf>,
    /// File or folder names to skip (`[ignore] patterns`).
    pub ignore: Vec<String>,
    /// How often to look for changes, in milliseconds.
    pub poll_ms: u64,
    /// Wait until a file has been quiet this long before saving it.
    pub settle_ms: u64,
    /// Save at most one version of each file per this many seconds.
    pub min_gap_s: u64,
    /// Skip files bigger than this many megabytes.
    pub max_file_mb: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            paths: Vec::new(),
            ignore: Vec::new(),
            poll_ms: 2000,
            settle_ms: 1000,
            min_gap_s: 10,
            max_file_mb: 50,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Config::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let mut cfg = Config::default();
        let mut section = String::new();
        for (index, raw) in text.lines().enumerate() {
            let n = index + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => return Err(format!("config line {n}: expected key = value")),
            };
            let bad = |e: String| format!("config line {n}: {e}");
            match (section.as_str(), key) {
                ("watch", "paths") => {
                    cfg.paths = parse_array(value)
                        .map_err(bad)?
                        .iter()
                        .map(|p| expand(p))
                        .collect();
                }
                ("watch", "poll_ms") => cfg.poll_ms = parse_num(value).map_err(bad)?,
                ("watch", "settle_ms") => cfg.settle_ms = parse_num(value).map_err(bad)?,
                ("watch", "min_gap_s") => cfg.min_gap_s = parse_num(value).map_err(bad)?,
                ("ignore", "patterns") => cfg.ignore = parse_array(value).map_err(bad)?,
                ("storage", "max_file_mb") => cfg.max_file_mb = parse_num(value).map_err(bad)?,
                _ => {}
            }
        }
        if cfg.paths.is_empty() {
            return Err("config has no folders to watch (set paths under [watch])".to_string());
        }
        cfg.poll_ms = cfg.poll_ms.max(100);
        Ok(cfg)
    }
}

/// Cut off a trailing `# comment`, ignoring any `#` inside quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_num(value: &str) -> Result<u64, String> {
    value.parse().map_err(|_| format!("'{value}' is not a number"))
}

fn parse_array(value: &str) -> Result<Vec<String>, String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| "expected a list like [\"a\", \"b\"] on one line".to_string())?;
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for c in inner.chars() {
        if c == '"' {
            if in_string {
                items.push(std::mem::take(&mut current));
            }
            in_string = !in_string;
        } else if in_string {
            current.push(c);
        }
    }
    if in_string {
        return Err("unterminated string in list".to_string());
    }
    Ok(items)
}

/// Turn a leading `~` into the home folder.
fn expand(p: &str) -> PathBuf {
    if p == "~" || p.starts_with("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(p.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(p)
}

/// Match a file or folder *name* against a simple pattern: `*.tmp`, `backup*`
/// or an exact name like `node_modules`.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix('*') {
        name.ends_with(rest)
    } else if let Some(rest) = pattern.strip_suffix('*') {
        name.starts_with(rest)
    } else {
        pattern == name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_config() {
        let cfg = Config::parse(
            r#"# comment
[watch]
paths = ["/tmp/a", "/tmp/b"]  # two folders
poll_ms = 500

[ignore]
patterns = ["*.tmp"]

[storage]
max_file_mb = 5
"#,
        )
        .unwrap();
        assert_eq!(cfg.paths, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
        assert_eq!(cfg.poll_ms, 500);
        assert_eq!(cfg.ignore, vec!["*.tmp".to_string()]);
        assert_eq!(cfg.max_file_mb, 5);
    }

    #[test]
    fn rejects_a_config_with_no_folders() {
        assert!(Config::parse("[ignore]\npatterns = []\n").is_err());
    }

    #[test]
    fn rejects_a_bad_number() {
        assert!(Config::parse("[watch]\npaths = [\"/tmp\"]\npoll_ms = soon\n").is_err());
    }

    #[test]
    fn matches_simple_patterns() {
        assert!(name_matches("*.tmp", "draft.tmp"));
        assert!(name_matches("node_modules", "node_modules"));
        assert!(name_matches("~*", "~lock"));
        assert!(!name_matches("*.tmp", "draft.txt"));
        assert!(!name_matches("target", "targets"));
    }
}
