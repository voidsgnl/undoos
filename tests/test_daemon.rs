use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn lifeline(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .args(args)
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .output()
        .expect("failed to run lifeline")
}

/// Run the daemon for a single scan, so tests don't need a background process.
fn daemon_once(home: &Path, config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap(), "--once"])
        .env("LIFELINE_HOME", home)
        .output()
        .expect("failed to run lifeline-daemon")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("undoos-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Returns (root, lifeline home, watched folder, config file).
fn setup(name: &str, ignore: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let home = root.join("home");
    let watched = root.join("watched");
    fs::create_dir_all(&watched).unwrap();
    let config = root.join("config.toml");
    fs::write(
        &config,
        format!(
            "[watch]\npaths = [\"{}\"]\nsettle_ms = 0\nmin_gap_s = 0\n\n[ignore]\npatterns = [{}]\n",
            watched.display(),
            ignore
        ),
    )
    .unwrap();
    (root, home, watched, config)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn daemon_saves_the_old_version_when_a_file_changes() {
    let (root, home, watched, config) = setup("daemon-change", "");
    let file = watched.join("notes.txt");

    fs::write(&file, "v1").unwrap();
    let out = daemon_once(&home, &config);
    assert!(out.status.success(), "daemon failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("tracking"));

    fs::write(&file, "v2").unwrap();
    let out = daemon_once(&home, &config);
    assert!(stdout(&out).contains("saved"), "expected a save, got: {}", stdout(&out));

    let out = lifeline(&home, &["undo"]);
    assert!(out.status.success(), "undo failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_saves_a_file_that_gets_deleted() {
    let (root, home, watched, config) = setup("daemon-delete", "");
    let file = watched.join("precious.txt");

    fs::write(&file, "keep me").unwrap();
    daemon_once(&home, &config);

    fs::remove_file(&file).unwrap();
    let out = daemon_once(&home, &config);
    assert!(stdout(&out).contains("deleted"), "expected a delete, got: {}", stdout(&out));

    let out = lifeline(&home, &["undo"]);
    assert!(out.status.success(), "undo failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep me");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_skips_ignored_files() {
    let (root, home, watched, config) = setup("daemon-ignore", "\"*.tmp\"");
    fs::write(watched.join("scratch.tmp"), "junk").unwrap();
    fs::write(watched.join("real.txt"), "data").unwrap();

    let out = daemon_once(&home, &config);
    let text = stdout(&out);
    assert!(text.contains("real.txt"));
    assert!(!text.contains("scratch.tmp"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_refuses_to_start_without_a_config() {
    let root = scratch("daemon-noconfig");
    let out = Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", root.join("missing.toml").to_str().unwrap(), "--once"])
        .env("LIFELINE_HOME", root.join("home"))
        .output()
        .expect("failed to run lifeline-daemon");
    assert!(!out.status.success());
    let _ = fs::remove_dir_all(&root);
}
