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
    let dir = std::env::temp_dir().join(format!("undoos-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn back_rewinds_to_a_checkpoint() {
    let root = scratch("checkpoint");
    let home = root.join("home");
    let file = root.join("notes.txt");
    let path = file.to_str().unwrap();

    fs::write(&file, "v1").unwrap();
    assert!(lifeline(&home, &["checkpoint", "before"]).status.success());
    assert!(lifeline(&home, &["snap", path]).status.success());

    fs::write(&file, "v2").unwrap();
    assert!(lifeline(&home, &["snap", path]).status.success());
    fs::write(&file, "v3").unwrap();

    let out = lifeline(&home, &["back", "before"]);
    assert!(out.status.success(), "back failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn log_lists_checkpoints() {
    let root = scratch("checkpoint-log");
    let home = root.join("home");
    assert!(lifeline(&home, &["checkpoint", "clean-slate"]).status.success());
    let out = lifeline(&home, &["log"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("clean-slate"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn back_to_unknown_checkpoint_fails() {
    let root = scratch("checkpoint-missing");
    let out = lifeline(&root.join("home"), &["back", "nope"]);
    assert!(!out.status.success());
    let _ = fs::remove_dir_all(&root);
}
