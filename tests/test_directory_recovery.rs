use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn lifeline(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .args(args)
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .output()
        .expect("failed to run lifeline")
}

fn daemon_once(home: &Path, config: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap(), "--once"])
        .env("LIFELINE_HOME", home)
        .output()
        .expect("failed to run daemon")
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "undoos-directory-recovery-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let watched = root.join("watched");
    fs::create_dir_all(&watched).unwrap();
    let config = root.join("config.toml");
    fs::write(
        &config,
        format!(
            "[watch]\npaths = [\"{}\"]\nsettle_ms = 0\nmin_gap_s = 0\n\n[ignore]\npatterns = []\n",
            watched.display()
        ),
    )
    .unwrap();
    (root, home, watched, config)
}

fn assert_success(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn undo_and_redo_deleted_empty_directory() {
    let (root, home, watched, config) = setup("delete-empty");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "initial scan");
    fs::remove_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "deletion scan");

    let undo = lifeline(&home, &["undo"]);
    assert_success(&undo, "undo deleted directory");
    assert!(directory.is_dir());

    let redo = lifeline(&home, &["redo"]);
    assert_success(&redo, "redo deleted directory");
    assert!(!directory.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn undo_and_redo_creation_of_empty_directory() {
    let (root, home, watched, config) = setup("create-empty");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");

    let undo = lifeline(&home, &["undo"]);
    assert_success(&undo, "undo directory creation");
    assert!(!directory.exists());

    let redo = lifeline(&home, &["redo"]);
    assert_success(&redo, "redo directory creation");
    assert!(directory.is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn undo_refuses_to_remove_directory_that_gained_contents() {
    let (root, home, watched, config) = setup("nonempty-conflict");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");
    let file = directory.join("new-user-file.txt");
    fs::write(&file, "keep this").unwrap();

    let undo = lifeline(&home, &["undo"]);
    assert!(
        !undo.status.success(),
        "undo must refuse to remove a directory that is no longer empty"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep this");
    assert!(directory.is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn nested_empty_directory_tree_undoes_and_redoes_in_safe_order() {
    let (root, home, watched, config) = setup("nested-empty");
    let parent = watched.join("parent");
    let child = parent.join("child");
    fs::create_dir_all(&child).unwrap();
    assert_success(&daemon_once(&home, &config), "initial scan");
    fs::remove_dir_all(&parent).unwrap();
    assert_success(&daemon_once(&home, &config), "deletion scan");

    // Deletions are journaled child-first, so undo recreates the parent first.
    assert_success(&lifeline(&home, &["undo"]), "restore parent");
    assert!(parent.is_dir());
    assert!(!child.exists());
    assert_success(&lifeline(&home, &["undo"]), "restore child");
    assert!(child.is_dir());

    // Redo follows the inverse order: child first, then parent.
    assert_success(&lifeline(&home, &["redo"]), "remove child");
    assert!(!child.exists());
    assert!(parent.is_dir());
    assert_success(&lifeline(&home, &["redo"]), "remove parent");
    assert!(!parent.exists());
    let _ = fs::remove_dir_all(root);
}

fn start_paused_undo(home: &Path, point: &str, marker: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .arg("undo")
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .env("LIFELINE_TEST_PAUSE_AT", point)
        .env("LIFELINE_TEST_PAUSE_MARKER", marker)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start paused Lifeline process")
}

fn kill_at_pause(mut child: Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if marker.is_file() {
            child.kill().expect("kill Lifeline at transaction boundary");
            let _ = child.wait();
            return;
        }
        if let Some(status) = child.try_wait().expect("poll Lifeline process") {
            panic!("Lifeline exited before pause marker (status {status})");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("Lifeline never reached transaction pause point");
}

#[test]
fn interrupted_directory_removal_is_reconciled_after_restart() {
    let (root, home, watched, config) = setup("crash-remove");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");

    let marker = root.join("paused");
    let child = start_paused_undo(&home, "dir_undo_after_remove", &marker);
    kill_at_pause(child, &marker);
    assert!(!directory.exists());

    let recovered = lifeline(&home, &["log"]);
    assert_success(&recovered, "reconcile interrupted directory removal");
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("dir_restore"));
    assert!(!directory.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn interrupted_directory_creation_is_reconciled_after_restart() {
    let (root, home, watched, config) = setup("crash-create");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");
    fs::remove_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "deletion scan");

    let marker = root.join("paused");
    let child = start_paused_undo(&home, "dir_undo_after_rename", &marker);
    kill_at_pause(child, &marker);
    assert!(directory.is_dir());

    let recovered = lifeline(&home, &["log"]);
    assert_success(&recovered, "reconcile interrupted directory creation");
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("dir_restore"));
    assert!(directory.is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn watcher_respects_directory_undo_and_redo_state_without_duplicate_events() {
    let (root, home, watched, config) = setup("watcher-after-recovery");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");

    let journal_path = home.join("journal.tsv");
    let initial_count = fs::read_to_string(&journal_path).unwrap().lines().count();

    assert_success(&lifeline(&home, &["undo"]), "undo creation");
    assert!(!directory.exists());
    let after_undo = daemon_once(&home, &config);
    assert_success(&after_undo, "scan after undo");
    assert_eq!(
        fs::read_to_string(&journal_path).unwrap().lines().count(),
        initial_count + 2,
        "the scan must not add a duplicate directory-deletion event"
    );

    assert_success(&lifeline(&home, &["redo"]), "redo creation");
    assert!(directory.is_dir());
    let after_redo = daemon_once(&home, &config);
    assert_success(&after_redo, "scan after redo");
    assert_eq!(
        fs::read_to_string(&journal_path).unwrap().lines().count(),
        initial_count + 3,
        "the scan must not add a duplicate directory-creation event"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn watcher_respects_restoration_of_a_deleted_directory() {
    let (root, home, watched, config) = setup("watcher-after-restore");
    let directory = watched.join("empty");
    fs::create_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "creation scan");
    fs::remove_dir(&directory).unwrap();
    assert_success(&daemon_once(&home, &config), "deletion scan");

    let journal_path = home.join("journal.tsv");
    let before_undo = fs::read_to_string(&journal_path).unwrap().lines().count();
    assert_success(&lifeline(&home, &["undo"]), "undo deletion");
    assert!(directory.is_dir());

    let after_undo = daemon_once(&home, &config);
    assert_success(&after_undo, "scan after restoring directory");
    assert_eq!(
        fs::read_to_string(&journal_path).unwrap().lines().count(),
        before_undo + 1,
        "the scan must not add a duplicate directory-creation event"
    );
    let _ = fs::remove_dir_all(root);
}
