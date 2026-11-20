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
fn back_rewinds_to_checkpoint_without_erasing_history() {
    let r = scratch("checkpoint");
    let h = r.join("home");
    let f = r.join("notes.txt");
    let p = f.to_str().unwrap();
    fs::write(&f, "v1").unwrap();
    assert!(lifeline(&h, &["checkpoint", "before"]).status.success());
    assert!(lifeline(&h, &["snap", p]).status.success());
    fs::write(&f, "v2").unwrap();
    assert!(lifeline(&h, &["snap", p]).status.success());
    fs::write(&f, "v3").unwrap();
    let o = lifeline(&h, &["back", "before"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read_to_string(&f).unwrap(), "v1");
    let log = String::from_utf8_lossy(&lifeline(&h, &["log"]).stdout).to_string();
    assert!(log.contains("checkpoint"));
    assert!(log.contains("snap"));
    assert!(log.contains("restored"));
    let _ = fs::remove_dir_all(r);
}
#[test]
fn log_lists_checkpoints() {
    let r = scratch("checkpoint-log");
    assert!(lifeline(&r.join("home"), &["checkpoint", "clean-slate"])
        .status
        .success());
    let o = lifeline(&r.join("home"), &["log"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("clean-slate"));
    let _ = fs::remove_dir_all(r);
}
#[test]
fn back_to_unknown_checkpoint_fails() {
    let r = scratch("checkpoint-missing");
    let o = lifeline(&r.join("home"), &["back", "nope"]);
    assert!(!o.status.success());
    let _ = fs::remove_dir_all(r);
}
