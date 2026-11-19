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

#[test]
fn daemon_recovers_all_files_from_a_deleted_directory() {
    let (root, home, watched, config) = setup("daemon-delete-tree", "");
    let folder = watched.join("school");
    fs::create_dir_all(folder.join("semester")).unwrap();
    let first = folder.join("assignment.txt");
    let second = folder.join("semester").join("notes.txt");
    fs::write(&first, "assignment contents").unwrap();
    fs::write(&second, "semester notes").unwrap();

    let initial = daemon_once(&home, &config);
    assert!(initial.status.success());
    assert!(stdout(&initial).contains("tracking"));

    fs::remove_dir_all(&folder).unwrap();
    let deleted = daemon_once(&home, &config);
    assert!(deleted.status.success());
    let output = stdout(&deleted);
    assert!(output.contains("deleted"), "expected deletion events: {output}");

    // Each file has its own saved state. Undo repeatedly to restore the tree.
    for _ in 0..2 {
        let out = lifeline(&home, &["undo"]);
        assert!(out.status.success(), "undo failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(fs::read_to_string(&first).unwrap(), "assignment contents");
    assert_eq!(fs::read_to_string(&second).unwrap(), "semester notes");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_does_not_report_a_missing_subtree_as_deleted_when_it_cannot_be_read() {
    let (root, home, watched, config) = setup("daemon-unreadable", "");
    let folder = watched.join("private");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join("keep.txt");
    fs::write(&file, "still here").unwrap();

    let initial = daemon_once(&home, &config);
    assert!(initial.status.success());

    // Run the daemon as an unprivileged user where possible. If the test
    // process is root, permission bits alone do not reliably block read_dir,
    // so this case is intentionally skipped rather than producing a false test.
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&folder).unwrap().permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&folder, permissions).unwrap();

    if fs::read_dir(&folder).is_err() {
        let scan = daemon_once(&home, &config);
        assert!(!stdout(&scan).contains("deleted"));
    }

    let mut permissions = fs::metadata(&folder).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&folder, permissions).unwrap();
    assert!(file.exists());
    let _ = fs::remove_dir_all(&root);
}


#[test]
fn daemon_reacts_to_filesystem_events_without_waiting_for_poll_interval() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let (root, home, watched, config) = setup("daemon-events", "");
    // A ten-second reconciliation interval makes this test prove that the
    // native event path, rather than polling, notices the edit.
    let config_text = fs::read_to_string(&config).unwrap().replace(
        "settle_ms = 0",
        "poll_ms = 10000\nsettle_ms = 0",
    );
    fs::write(&config, config_text).unwrap();

    let file = watched.join("event.txt");
    fs::write(&file, "before").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap()])
        .env("LIFELINE_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start event-driven daemon");

    // Give the daemon time to register its watch and take the initial scan.
    thread::sleep(Duration::from_millis(500));
    fs::write(&file, "after").unwrap();

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut restored = false;
    while Instant::now() < deadline {
        let out = lifeline(&home, &["undo"]);
        if out.status.success() {
            restored = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = child.kill();
    let _ = child.wait();

    assert!(restored, "event-driven watcher did not save the old version before the 10-second fallback scan");
    assert_eq!(fs::read_to_string(&file).unwrap(), "before");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_preserves_rapid_successive_versions_even_with_legacy_gap_setting() {
    let (root, home, watched, config) = setup("daemon-rapid-versions", "");
    let config_text = fs::read_to_string(&config).unwrap().replace("min_gap_s = 0", "min_gap_s = 10");
    fs::write(&config, config_text).unwrap();

    let file = watched.join("rapid.txt");
    fs::write(&file, "version one").unwrap();
    assert!(daemon_once(&home, &config).status.success());

    fs::write(&file, "version two").unwrap();
    let second = daemon_once(&home, &config);
    assert!(stdout(&second).contains("saved"), "second version was not recorded: {}", stdout(&second));

    fs::write(&file, "version three").unwrap();
    let third = daemon_once(&home, &config);
    assert!(stdout(&third).contains("saved"), "third version was not recorded: {}", stdout(&third));

    for expected in ["version two", "version one"] {
        let undone = lifeline(&home, &["undo"]);
        assert!(undone.status.success(), "undo failed: {}", String::from_utf8_lossy(&undone.stderr));
        assert_eq!(fs::read_to_string(&file).unwrap(), expected);
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_recovers_previous_contents_after_atomic_file_replacement() {
    let (root, home, watched, config) = setup("daemon-atomic-replace", "");
    let file = watched.join("document.txt");
    let temporary = watched.join("document.txt.editor-temp");

    fs::write(&file, "original contents").unwrap();
    assert!(daemon_once(&home, &config).status.success());

    fs::write(&temporary, "replacement contents").unwrap();
    fs::rename(&temporary, &file).unwrap();

    let scan = daemon_once(&home, &config);
    assert!(stdout(&scan).contains("saved"), "atomic replacement was not recorded: {}", stdout(&scan));

    let undone = lifeline(&home, &["undo"]);
    assert!(undone.status.success(), "undo failed: {}", String::from_utf8_lossy(&undone.stderr));
    assert_eq!(fs::read_to_string(&file).unwrap(), "original contents");
    let _ = fs::remove_dir_all(&root);
}