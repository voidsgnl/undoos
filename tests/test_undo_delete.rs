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
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("undoos-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}
#[test]
fn undo_brings_back_deleted_file_and_keeps_history() {
    let r = scratch("undo-delete");
    let h = r.join("home");
    let f = r.join("important.txt");
    fs::write(&f, "do not lose me").unwrap();
    assert!(lifeline(&h, &["snap", f.to_str().unwrap()])
        .status
        .success());
    fs::remove_file(&f).unwrap();
    let o = lifeline(&h, &["undo"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read_to_string(&f).unwrap(), "do not lose me");
    let log = String::from_utf8_lossy(&lifeline(&h, &["log"]).stdout).to_string();
    assert!(log.contains("snap"));
    assert!(log.contains("restored"));
    assert!(h.join("snapshots/1").exists());
    let _ = fs::remove_dir_all(r);
}
#[test]
fn undo_with_nothing_saved_is_harmless() {
    let r = scratch("undo-empty");
    let o = lifeline(&r.join("home"), &["undo"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("Nothing to undo"));
    let _ = fs::remove_dir_all(r);
}
#[test]
fn redo_reverses_undo_of_deleted_file() {
    let r = scratch("redo-delete");
    let h = r.join("home");
    let f = r.join("important.txt");
    fs::write(&f, "keep me").unwrap();
    assert!(lifeline(&h, &["snap", f.to_str().unwrap()])
        .status
        .success());
    fs::remove_file(&f).unwrap();
    assert!(lifeline(&h, &["undo"]).status.success());
    assert!(f.exists());
    let o = lifeline(&h, &["redo"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!f.exists());
    let _ = fs::remove_dir_all(r);
}
#[test]
fn undo_of_modified_file_can_be_redone() {
    let r = scratch("redo-modified");
    let h = r.join("home");
    let f = r.join("notes.txt");
    fs::write(&f, "old").unwrap();
    assert!(lifeline(&h, &["snap", f.to_str().unwrap()])
        .status
        .success());
    fs::write(&f, "new").unwrap();
    assert!(lifeline(&h, &["undo"]).status.success());
    assert_eq!(fs::read_to_string(&f).unwrap(), "old");
    assert!(lifeline(&h, &["redo"]).status.success());
    assert_eq!(fs::read_to_string(&f).unwrap(), "new");
    let _ = fs::remove_dir_all(r);
}

#[test]
fn failed_restore_preparation_preserves_the_current_file() {
    let r = scratch("restore-preparation-failure");
    let h = r.join("home");
    let file = r.join("notes.txt");
    fs::write(&file, "saved version").unwrap();
    assert!(lifeline(&h, &["snap", file.to_str().unwrap()])
        .status
        .success());
    fs::remove_file(h.join("snapshots/1")).unwrap();
    fs::write(&file, "current user version").unwrap();

    let out = lifeline(&h, &["undo"]);
    assert!(
        !out.status.success(),
        "undo should fail when its recovery copy is missing"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "current user version");
    let leftovers: Vec<_> = fs::read_dir(&r).unwrap().filter_map(Result::ok).collect();
    assert!(!leftovers
        .iter()
        .any(|e| e.file_name().to_string_lossy().contains("lifeline-restore")));
    let _ = fs::remove_dir_all(r);
}

#[test]
fn restore_replaces_existing_file_with_complete_saved_contents() {
    let r = scratch("restore-atomic-replace");
    let h = r.join("home");
    let file = r.join("notes.txt");
    fs::write(&file, "original complete contents").unwrap();
    assert!(lifeline(&h, &["snap", file.to_str().unwrap()])
        .status
        .success());
    fs::write(&file, "new contents that must be replaced").unwrap();

    let out = lifeline(&h, &["undo"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "original complete contents"
    );
    let _ = fs::remove_dir_all(r);
}

#[cfg(unix)]
#[test]
fn atomic_restore_preserves_saved_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let r = scratch("restore-permissions");
    let h = r.join("home");
    let file = r.join("tool.sh");
    fs::write(&file, "#!/bin/sh\necho lifeline\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(lifeline(&h, &["snap", file.to_str().unwrap()])
        .status
        .success());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(lifeline(&h, &["undo"]).status.success());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let _ = fs::remove_dir_all(r);
}

#[test]
fn next_command_finishes_recording_an_interrupted_undo() {
    let r = scratch("reconcile-finished-undo");
    let h = r.join("home");
    let file = r.join("notes.txt");
    fs::create_dir_all(h.join("snapshots")).unwrap();
    fs::write(h.join("snapshots/1"), "saved version").unwrap();
    fs::write(h.join("snapshots/2"), "version before interrupted undo").unwrap();
    fs::write(&file, "saved version").unwrap(); // Atomic replacement already happened before the crash.
    fs::write(
        h.join("journal.tsv"),
        format!(
            "1\t1\tsnap\t{}\t1\n2\t2\tundo_pending\t1\t2\n",
            file.display()
        ),
    )
    .unwrap();

    let out = lifeline(&h, &["log"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("restored"), "{log}");
    assert!(log.contains("entry #1"), "{log}");
    let _ = fs::remove_dir_all(r);
}

#[test]
fn next_command_aborts_an_interrupted_undo_that_never_changed_the_file() {
    let r = scratch("reconcile-aborted-undo");
    let h = r.join("home");
    let file = r.join("notes.txt");
    fs::create_dir_all(h.join("snapshots")).unwrap();
    fs::write(h.join("snapshots/1"), "saved version").unwrap();
    fs::write(h.join("snapshots/2"), "version before interrupted undo").unwrap();
    fs::write(&file, "version before interrupted undo").unwrap();
    fs::write(
        h.join("journal.tsv"),
        format!(
            "1\t1\tsnap\t{}\t1\n2\t2\tundo_pending\t1\t2\n",
            file.display()
        ),
    )
    .unwrap();

    let out = lifeline(&h, &["log"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("undo_abort"), "{log}");
    assert!(!log.contains("restored"), "{log}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "version before interrupted undo"
    );
    let _ = fs::remove_dir_all(r);
}
