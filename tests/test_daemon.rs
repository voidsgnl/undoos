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
fn undo_refuses_to_overwrite_edits_made_after_the_watcher_saved_a_version() {
    let (root, home, watched, config) = setup("daemon-recovery-conflict", "");
    let file = watched.join("notes.txt");

    fs::write(&file, "version one").unwrap();
    assert!(daemon_once(&home, &config).status.success());
    fs::write(&file, "version two").unwrap();
    let saved = daemon_once(&home, &config);
    assert!(stdout(&saved).contains("saved"));

    // This edit happens after Lifeline's last recorded state. Undo must not
    // silently replace it with the older snapshot.
    fs::write(&file, "newer user edit").unwrap();
    let undo = lifeline(&home, &["undo"]);
    assert!(!undo.status.success(), "undo should report a recovery conflict");
    let message = String::from_utf8_lossy(&undo.stderr);
    assert!(message.contains("recovery conflict"), "unexpected error: {message}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "newer user edit");

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
        if out.status.success() && stdout(&out).contains("Restored") {
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

#[test]
fn daemon_recognizes_file_rename_and_supports_undo_redo() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};
    let (root, home, watched, config) = setup("daemon-file-rename", "");
    let old = watched.join("before.txt");
    let new = watched.join("after.txt");
    fs::write(&old, "same inode, same contents").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap()]).env("LIFELINE_HOME", &home)
        .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    thread::sleep(Duration::from_millis(500));
    fs::rename(&old, &new).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut undone = false;
    while Instant::now() < deadline {
        let out = lifeline(&home, &["undo"]);
        if out.status.success() && stdout(&out).contains("Moved") { undone = true; break; }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill(); let _ = child.wait();
    assert!(undone, "rename was not recorded as one reversible move");
    assert_eq!(fs::read_to_string(&old).unwrap(), "same inode, same contents");
    assert!(!new.exists());
    let redone = lifeline(&home, &["redo"]);
    assert!(redone.status.success(), "redo failed: {}", String::from_utf8_lossy(&redone.stderr));
    assert!(!old.exists());
    assert_eq!(fs::read_to_string(&new).unwrap(), "same inode, same contents");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_recognizes_directory_move_as_individual_reversible_file_moves() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};
    let (root, home, watched, config) = setup("daemon-directory-move", "");
    let old_dir = watched.join("old-folder");
    let new_dir = watched.join("new-folder");
    fs::create_dir_all(old_dir.join("nested")).unwrap();
    let first = old_dir.join("one.txt");
    let second = old_dir.join("nested").join("two.txt");
    fs::write(&first, "one").unwrap();
    fs::write(&second, "two").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap()]).env("LIFELINE_HOME", &home)
        .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    thread::sleep(Duration::from_millis(500));
    fs::rename(&old_dir, &new_dir).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut moves = 0;
    while Instant::now() < deadline && moves < 2 {
        let out = lifeline(&home, &["log"]);
        moves = stdout(&out).lines().filter(|line| line.contains("moved") && line.contains("->")).count();
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill(); let _ = child.wait();
    assert_eq!(moves, 2, "expected two file-level move records");
    for _ in 0..2 {
        let out = lifeline(&home, &["undo"]);
        assert!(out.status.success(), "undo failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(fs::read_to_string(&second).unwrap(), "two");
    let _ = fs::remove_dir_all(&root);
}


#[test]
fn daemon_recognizes_rename_after_restart_and_preserves_identity_through_undo_redo() {
    let (root, home, watched, config) = setup("daemon-rename-restart", "");
    let old = watched.join("before-restart.txt");
    let new = watched.join("after-restart.txt");
    fs::write(&old, "identity survives daemon restart").unwrap();

    // First process establishes the baseline and persists device/inode.
    let initial = daemon_once(&home, &config);
    assert!(initial.status.success(), "initial scan failed");
    assert!(stdout(&initial).contains("tracking"));

    // The daemon is stopped here. The rename happens between two processes.
    fs::rename(&old, &new).unwrap();
    let restarted = daemon_once(&home, &config);
    assert!(restarted.status.success(), "restart scan failed: {}", String::from_utf8_lossy(&restarted.stderr));
    assert!(stdout(&restarted).contains("moved"), "restart did not recognize the move: {}", stdout(&restarted));

    let undone = lifeline(&home, &["undo"]);
    assert!(undone.status.success(), "undo failed: {}", String::from_utf8_lossy(&undone.stderr));
    assert_eq!(fs::read_to_string(&old).unwrap(), "identity survives daemon restart");
    assert!(!new.exists());

    // A later daemon scan must still know the baseline belongs to the old path.
    let after_undo = daemon_once(&home, &config);
    assert!(after_undo.status.success());
    assert!(!stdout(&after_undo).contains("deleted"), "baseline path became stale after undo: {}", stdout(&after_undo));

    let redone = lifeline(&home, &["redo"]);
    assert!(redone.status.success(), "redo failed: {}", String::from_utf8_lossy(&redone.stderr));
    assert!(!old.exists());
    assert_eq!(fs::read_to_string(&new).unwrap(), "identity survives daemon restart");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_does_not_guess_a_move_from_matching_contents() {
    let (root, home, watched, config) = setup("daemon-no-content-guess", "");
    let old = watched.join("old.txt");
    let new = watched.join("new.txt");
    fs::write(&old, "same bytes do not mean same file").unwrap();
    assert!(daemon_once(&home, &config).status.success());

    // Create the replacement while the original still exists so it cannot
    // inherit a just-freed inode number.
    fs::write(&new, "same bytes do not mean same file").unwrap();
    fs::remove_file(&old).unwrap();
    let scan = daemon_once(&home, &config);
    assert!(scan.status.success());
    let output = stdout(&scan);
    assert!(!output.contains("moved"), "different inode was incorrectly classified as a move: {output}");
    assert!(output.contains("deleted"), "old file deletion should be recorded: {output}");
    assert!(output.contains("tracking"), "new file should be tracked independently: {output}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn interrupted_move_undo_after_rename_is_completed_on_next_command() {
    let (root, home, watched, config) = setup("move-undo-reconcile", "");
    let old = watched.join("old.txt");
    let new = watched.join("new.txt");
    fs::write(&old, "move transaction").unwrap();
    assert!(daemon_once(&home, &config).status.success());
    fs::rename(&old, &new).unwrap();
    assert!(daemon_once(&home, &config).status.success());

    let journal = home.join("journal.tsv");
    let text = fs::read_to_string(&journal).unwrap();
    let move_id: u64 = text.lines().find_map(|line| {
        let fields: Vec<_> = line.split('\t').collect();
        (fields.get(2) == Some(&"move")).then(|| fields[0].parse().ok()).flatten()
    }).unwrap();
    let next_id = text.lines().filter_map(|line| line.split('\t').next()?.parse::<u64>().ok()).max().unwrap() + 1;
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    writeln!(file, "{next_id}\t1\tmove_undo_pending\t{move_id}\t").unwrap();
    file.sync_all().unwrap();

    // Simulate a crash after the filesystem rename, before baseline relocation
    // and before the completion event.
    fs::rename(&new, &old).unwrap();
    let out = lifeline(&home, &["log"]);
    assert!(out.status.success(), "reconciliation failed: {}", String::from_utf8_lossy(&out.stderr));
    let log = stdout(&out);
    assert!(log.contains("restored"), "{log}");
    assert!(old.exists() && !new.exists());
    let baseline_paths = fs::read_dir(home.join("baseline")).unwrap()
        .filter_map(Result::ok).filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("path"))
        .filter_map(|e| fs::read_to_string(e.path()).ok()).collect::<Vec<_>>();
    assert!(baseline_paths.iter().any(|p| p == old.to_str().unwrap()));
    assert!(!baseline_paths.iter().any(|p| p == new.to_str().unwrap()));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn interrupted_move_undo_before_rename_is_aborted_safely() {
    let (root, home, watched, config) = setup("move-undo-abort", "");
    let old = watched.join("old.txt");
    let new = watched.join("new.txt");
    fs::write(&old, "move transaction").unwrap();
    assert!(daemon_once(&home, &config).status.success());
    fs::rename(&old, &new).unwrap();
    assert!(daemon_once(&home, &config).status.success());

    let journal = home.join("journal.tsv");
    let text = fs::read_to_string(&journal).unwrap();
    let move_id: u64 = text.lines().find_map(|line| {
        let fields: Vec<_> = line.split('\t').collect();
        (fields.get(2) == Some(&"move")).then(|| fields[0].parse().ok()).flatten()
    }).unwrap();
    let next_id = text.lines().filter_map(|line| line.split('\t').next()?.parse::<u64>().ok()).max().unwrap() + 1;
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    writeln!(file, "{next_id}\t1\tmove_undo_pending\t{move_id}\t").unwrap();
    file.sync_all().unwrap();

    let out = lifeline(&home, &["log"]);
    assert!(out.status.success(), "reconciliation failed: {}", String::from_utf8_lossy(&out.stderr));
    let log = stdout(&out);
    assert!(log.contains("move_undo_abort"), "{log}");
    assert!(new.exists() && !old.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn interrupted_move_redo_after_rename_is_completed_on_next_command() {
    let (root, home, watched, config) = setup("move-redo-reconcile", "");
    let old = watched.join("old.txt");
    let new = watched.join("new.txt");
    fs::write(&old, "move redo transaction").unwrap();
    assert!(daemon_once(&home, &config).status.success());
    fs::rename(&old, &new).unwrap();
    assert!(daemon_once(&home, &config).status.success());
    let undone = lifeline(&home, &["undo"]);
    assert!(undone.status.success(), "undo failed: {}", String::from_utf8_lossy(&undone.stderr));

    let journal = home.join("journal.tsv");
    let text = fs::read_to_string(&journal).unwrap();
    let restore_id: u64 = text.lines().find_map(|line| {
        let fields: Vec<_> = line.split('\t').collect();
        (fields.get(2) == Some(&"restore")).then(|| fields[0].parse().ok()).flatten()
    }).unwrap();
    let next_id = text.lines().filter_map(|line| line.split('\t').next()?.parse::<u64>().ok()).max().unwrap() + 1;
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    writeln!(file, "{next_id}\t1\tmove_redo_pending\t{restore_id}\t").unwrap();
    file.sync_all().unwrap();

    // Simulate a crash after moving to the destination but before metadata update.
    fs::rename(&old, &new).unwrap();
    let out = lifeline(&home, &["log"]);
    assert!(out.status.success(), "reconciliation failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(new.exists() && !old.exists());
    assert!(stdout(&out).contains("redone"));
    let _ = fs::remove_dir_all(&root);
}
