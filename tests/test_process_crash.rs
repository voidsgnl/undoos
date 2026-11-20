use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lifeline-process-crash-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn lifeline(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .args(args)
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .output()
        .expect("could not run lifeline")
}

fn start_paused(home: &Path, args: &[&str], point: &str, marker: &Path) -> Child {
    start_paused_with_release(home, args, point, marker, None)
}

fn start_paused_with_release(
    home: &Path,
    args: &[&str],
    point: &str,
    marker: &Path,
    release: Option<&Path>,
) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lifeline"));
    command
        .args(args)
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .env("LIFELINE_TEST_PAUSE_AT", point)
        .env("LIFELINE_TEST_PAUSE_MARKER", marker);
    if let Some(release) = release {
        command.env("LIFELINE_TEST_CONTINUE_MARKER", release);
    }
    command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start paused lifeline process")
}

fn kill_at_pause(mut child: Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if marker.is_file() {
            child.kill().expect("SIGKILL test child");
            let status = child.wait().expect("wait for killed child");
            assert!(
                !status.success(),
                "paused child unexpectedly exited cleanly"
            );
            return;
        }
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("lifeline exited before pause marker (status {status})");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("lifeline never reached transaction pause point");
}

fn prepare_undo(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = root(name);
    let home = root.join("lifeline-home");
    let file = root.join("document.txt");
    fs::write(&file, "saved original").unwrap();
    let saved = lifeline(&home, &["snap", file.to_str().unwrap()]);
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    fs::write(&file, "new current version").unwrap();
    (root.clone(), home, file)
}

fn prepare_redo(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (root, home, file) = prepare_undo(name);
    let undone = lifeline(&home, &["undo"]);
    assert!(
        undone.status.success(),
        "{}",
        String::from_utf8_lossy(&undone.stderr)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "saved original");
    (root, home, file)
}

#[test]
fn sigkill_after_undo_intent_aborts_without_changing_file() {
    let (root, home, file) = prepare_undo("undo-before-apply");
    let marker = root.join("paused");
    let child = start_paused(&home, &["undo"], "undo_after_intent", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "new current version");
    let log = String::from_utf8_lossy(&recovered.stdout);
    assert!(log.contains("undo_abort"), "{log}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_undo_apply_finishes_transaction_on_next_command() {
    let (root, home, file) = prepare_undo("undo-after-apply");
    let marker = root.join("paused");
    let child = start_paused(&home, &["undo"], "undo_after_apply", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "saved original");
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("restored"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_redo_intent_aborts_without_changing_file() {
    let (root, home, file) = prepare_redo("redo-before-apply");
    let marker = root.join("paused");
    let child = start_paused(&home, &["redo"], "redo_after_intent", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "saved original");
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("redo_abort"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_redo_apply_finishes_transaction_on_next_command() {
    let (root, home, file) = prepare_redo("redo-after-apply");
    let marker = root.join("paused");
    let child = start_paused(&home, &["redo"], "redo_after_apply", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "new current version");
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("redone"));
    fs::remove_dir_all(root).unwrap();
}

fn daemon_once(home: &Path, config: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lifeline-daemon"))
        .args(["--config", config.to_str().unwrap(), "--once"])
        .env("LIFELINE_HOME", home)
        .output()
        .expect("could not run lifeline-daemon")
}

fn prepare_move(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = root(name);
    let home = root.join("lifeline-home");
    let watched = root.join("watched");
    fs::create_dir_all(&watched).unwrap();
    let config = root.join("config.toml");
    fs::write(
        &config,
        format!(
            "[watch]\npaths = [\"{}\"]\nsettle_ms = 0\n",
            watched.display()
        ),
    )
    .unwrap();

    let old = watched.join("old.txt");
    let new = watched.join("new.txt");
    fs::write(&old, "move crash payload").unwrap();
    let first_scan = daemon_once(&home, &config);
    assert!(
        first_scan.status.success(),
        "initial scan failed: {}",
        String::from_utf8_lossy(&first_scan.stderr)
    );
    fs::rename(&old, &new).unwrap();
    let second_scan = daemon_once(&home, &config);
    assert!(
        second_scan.status.success(),
        "move scan failed: {}",
        String::from_utf8_lossy(&second_scan.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second_scan.stdout).contains("moved"),
        "daemon did not record move: {}",
        String::from_utf8_lossy(&second_scan.stdout)
    );
    (root, home, old, new)
}

fn prepare_move_redo(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let (root, home, old, new) = prepare_move(name);
    let undone = lifeline(&home, &["undo"]);
    assert!(
        undone.status.success(),
        "move undo failed: {}",
        String::from_utf8_lossy(&undone.stderr)
    );
    assert!(old.exists() && !new.exists());
    (root, home, old, new)
}

#[test]
fn sigkill_after_move_undo_intent_aborts_without_renaming() {
    let (root, home, old, new) = prepare_move("move-undo-before-rename");
    let marker = root.join("paused");
    let child = start_paused(&home, &["undo"], "move_undo_after_intent", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "reconciliation failed: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(new.exists() && !old.exists());
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("move_undo_abort"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_move_undo_rename_completes_transaction() {
    let (root, home, old, new) = prepare_move("move-undo-after-rename");
    let marker = root.join("paused");
    let child = start_paused(&home, &["undo"], "move_undo_after_rename", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "reconciliation failed: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(old.exists() && !new.exists());
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("restored"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_move_redo_intent_aborts_without_renaming() {
    let (root, home, old, new) = prepare_move_redo("move-redo-before-rename");
    let marker = root.join("paused");
    let child = start_paused(&home, &["redo"], "move_redo_after_intent", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "reconciliation failed: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(old.exists() && !new.exists());
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("move_redo_abort"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_after_move_redo_rename_completes_transaction() {
    let (root, home, old, new) = prepare_move_redo("move-redo-after-rename");
    let marker = root.join("paused");
    let child = start_paused(&home, &["redo"], "move_redo_after_rename", &marker);
    kill_at_pause(child, &marker);

    let recovered = lifeline(&home, &["log"]);
    assert!(
        recovered.status.success(),
        "reconciliation failed: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(new.exists() && !old.exists());
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("redone"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_move_undo_does_not_adopt_an_unrelated_replacement_file() {
    let (root, home, old, new) = prepare_move("move-undo-replaced-destination");
    let marker = root.join("paused");
    let child = start_paused(&home, &["undo"], "move_undo_after_rename", &marker);
    kill_at_pause(child, &marker);

    // Simulate another process replacing the renamed file before Lifeline
    // gets a chance to reconcile its interrupted transaction.
    fs::remove_file(&old).unwrap();
    fs::write(&old, "unrelated replacement").unwrap();

    let recovered = lifeline(&home, &["log"]);
    assert!(!recovered.status.success(), "reconciliation must refuse an unrelated file");
    assert_eq!(fs::read_to_string(&old).unwrap(), "unrelated replacement");
    assert!(!new.exists());
    let error = String::from_utf8_lossy(&recovered.stderr);
    assert!(error.contains("not the original moved file"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sigkill_move_redo_does_not_adopt_an_unrelated_replacement_file() {
    let (root, home, old, new) = prepare_move_redo("move-redo-replaced-destination");
    let marker = root.join("paused");
    let child = start_paused(&home, &["redo"], "move_redo_after_rename", &marker);
    kill_at_pause(child, &marker);

    // A replacement at the destination must not be mistaken for the file
    // that Lifeline renamed before the crash.
    fs::remove_file(&new).unwrap();
    fs::write(&new, "unrelated replacement").unwrap();

    let recovered = lifeline(&home, &["log"]);
    assert!(!recovered.status.success(), "reconciliation must refuse an unrelated file");
    assert_eq!(fs::read_to_string(&new).unwrap(), "unrelated replacement");
    assert!(!old.exists());
    let error = String::from_utf8_lossy(&recovered.stderr);
    assert!(error.contains("not the original moved file"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

fn wait_for_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if marker.is_file() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("lifeline exited before pause marker (status {status})");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("lifeline never reached transaction pause point");
}

#[test]
fn concurrent_edit_during_undo_preparation_is_not_overwritten() {
    let (root, home, file) = prepare_undo("concurrent-edit-undo");
    let marker = root.join("paused");
    let release = root.join("continue");
    let mut child = start_paused_with_release(
        &home, &["undo"], "apply_before_commit", &marker, Some(&release)
    );
    wait_for_marker(&mut child, &marker);
    fs::write(&file, "concurrent application edit").unwrap();
    fs::write(&release, "continue").unwrap();
    let status = child.wait().expect("wait for lifeline");
    assert!(!status.success(), "undo must refuse the concurrent edit");
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrent application edit");
    let recovered = lifeline(&home, &["log"]);
    assert!(!recovered.status.success(), "unresolved conflict must remain visible");
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrent application edit");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_edit_during_redo_preparation_is_not_overwritten() {
    let (root, home, file) = prepare_redo("concurrent-edit-redo");
    let marker = root.join("paused");
    let release = root.join("continue");
    let mut child = start_paused_with_release(
        &home, &["redo"], "apply_before_commit", &marker, Some(&release)
    );
    wait_for_marker(&mut child, &marker);
    fs::write(&file, "concurrent application edit").unwrap();
    fs::write(&release, "continue").unwrap();
    let status = child.wait().expect("wait for lifeline");
    assert!(!status.success(), "redo must refuse the concurrent edit");
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrent application edit");
    let recovered = lifeline(&home, &["log"]);
    assert!(!recovered.status.success(), "unresolved conflict must remain visible");
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrent application edit");
    fs::remove_dir_all(root).unwrap();
}



#[test]
fn concurrent_cli_waits_for_recovery_transaction_lock() {
    let (root, home, _file) = prepare_undo("state-lock-serialization");
    let marker = root.join("paused");
    let mut first = start_paused(&home, &["undo"], "undo_after_intent", &marker);
    wait_for_marker(&mut first, &marker);

    let mut second = Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .args(["log"])
        .env("LIFELINE_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start competing Lifeline command");

    thread::sleep(Duration::from_millis(150));
    assert!(
        second.try_wait().unwrap().is_none(),
        "second command must wait while the first process owns the state lock"
    );

    first.kill().expect("kill first Lifeline process");
    let first_status = first.wait().expect("wait for first Lifeline process");
    assert!(!first_status.success());
    let second_output = second
        .wait_with_output()
        .expect("wait for second Lifeline process");
    assert!(
        second_output.status.success(),
        "second command should acquire the released lock and reconcile: {}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
