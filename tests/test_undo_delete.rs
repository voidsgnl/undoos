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
fn undo_brings_back_a_deleted_file() {
    let root = scratch("undo-delete");
    let home = root.join("home");
    let file = root.join("important.txt");
    fs::write(&file, "do not lose me").unwrap();

    let out = lifeline(&home, &["snap", file.to_str().unwrap()]);
    assert!(out.status.success(), "snap failed: {}", String::from_utf8_lossy(&out.stderr));

    fs::remove_file(&file).unwrap();
    assert!(!file.exists());

    let out = lifeline(&home, &["undo"]);
    assert!(out.status.success(), "undo failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&file).unwrap(), "do not lose me");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn undo_with_nothing_saved_is_harmless() {
    let root = scratch("undo-empty");
    let out = lifeline(&root.join("home"), &["undo"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Nothing to undo"));
    let _ = fs::remove_dir_all(&root);
}
