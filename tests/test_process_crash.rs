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
    Command::new(env!("CARGO_BIN_EXE_lifeline"))
        .args(args)
        .env("LIFELINE_HOME", home)
        .env("LIFELINE_YES", "1")
        .env("LIFELINE_TEST_PAUSE_AT", point)
        .env("LIFELINE_TEST_PAUSE_MARKER", marker)
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
