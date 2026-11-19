//! lifeline-daemon: the background service that always listens.
//!
//! Stub. It will read `config.toml`, watch the configured paths and call
//! lifeline whenever something changes. For now it only checks the config
//! exists and exits, so nobody thinks files are being watched when they aren't.

use std::{env, fs, path::PathBuf, process};

fn main() {
    let config = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("lifeline/config.toml"));

    match fs::read_to_string(&config) {
        Ok(_) => println!("lifeline-daemon: found config at {}", config.display()),
        Err(e) => eprintln!("lifeline-daemon: cannot read {}: {e}", config.display()),
    }
    eprintln!("lifeline-daemon: background watching is not implemented yet.");
    process::exit(1);
}
