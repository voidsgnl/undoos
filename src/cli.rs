//! Argument parsing and command dispatch.
use crate::{guard, journal, lock::StateLock, restore, snapshot};
use std::fs;
pub const USAGE: &str = "\
lifeline - a system-wide undo button for Linux

USAGE:
    lifeline <command> [args]

COMMANDS:
    snap <file>          Remember a file as it is right now
    log                  Show everything lifeline remembers
    undo                 Reverse the most recent saved change
    redo                 Reverse the most recent undo
    checkpoint <name>    Drop a named marker in the timeline
    back <name>          Rewind saved changes after a checkpoint
    to <id>              Rewind saved changes after entry #id
    system status        Check the privileged system service
    system capture-hostname  Capture the approved /etc/hostname file
    system history       Show captured /etc/hostname versions
    system restore <id>  Restore a captured /etc/hostname version
    help                 Show this message
    --version            Show the version";
pub enum Command {
    Help,
    Version,
    Log,
    Snap(String),
    Undo,
    Redo,
    Checkpoint(String),
    Back(String),
    To(String),
    SystemStatus,
    SystemCaptureHostname,
    SystemHistory,
    SystemRestore(String),
}
fn arg(a: &[String], u: &str) -> Result<String, String> {
    a.get(1).cloned().ok_or_else(|| format!("usage: {u}"))
}
pub fn parse(a: &[String]) -> Result<Command, String> {
    match a.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => Ok(Command::Help),
        Some("--version") | Some("-V") => Ok(Command::Version),
        Some("log") => Ok(Command::Log),
        Some("undo") => Ok(Command::Undo),
        Some("redo") => Ok(Command::Redo),
        Some("snap") => arg(a, "lifeline snap <file>").map(Command::Snap),
        Some("checkpoint") => arg(a, "lifeline checkpoint <name>").map(Command::Checkpoint),
        Some("back") => arg(a, "lifeline back <name>").map(Command::Back),
        Some("to") => arg(a, "lifeline to <id>").map(Command::To),
        Some("system") if a.len() == 2 && a[1] == "status" => Ok(Command::SystemStatus),
        Some("system") if a.len() == 2 && a[1] == "capture-hostname" => Ok(Command::SystemCaptureHostname),
        Some("system") if a.len() == 2 && a[1] == "history" => Ok(Command::SystemHistory),
        Some("system") if a.len() == 3 && a[1] == "restore" => {
            Ok(Command::SystemRestore(a[2].clone()))
        }
        Some("system") => Err("usage: lifeline system {status|capture-hostname|history|restore <id>}".to_string()),
        Some(x) => Err(format!("unknown command: {x}")),
    }
}
pub fn run(cmd: Command) -> Result<(), String> {
    match &cmd {
        Command::SystemStatus => return crate::system_client::status(),
        Command::SystemCaptureHostname => return crate::system_client::capture_hostname(),
        Command::SystemHistory => return crate::system_client::hostname_history(),
        Command::SystemRestore(id) => return crate::system_client::restore_hostname(id),
        _ => {}
    }
    if matches!(cmd, Command::Help | Command::Version) {
        return match cmd {
            Command::Help => {
                println!("{USAGE}");
                Ok(())
            }
            Command::Version => {
                println!("lifeline {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            _ => unreachable!(),
        };
    }
    // One lock spans reconciliation and the complete command. This prevents
    // another CLI process or daemon scan from interleaving journal mutations.
    let _state_lock = StateLock::acquire()?;
    restore::reconcile_pending()?;
    match cmd {
        Command::Help | Command::Version | Command::SystemStatus | Command::SystemCaptureHostname | Command::SystemHistory | Command::SystemRestore(_) => unreachable!(),
        Command::Log => {
            let es = journal::read_all()?;
            if es.is_empty() {
                println!("Nothing saved yet. Try: lifeline snap <file>");
            }
            for e in es {
                println!("{}", e.display());
            }
        }
        Command::Snap(p) => snap(&p)?,
        Command::Undo => {
            let e = journal::active_snapshots()?
                .into_iter()
                .chain(journal::active_moves()?.into_iter())
                .chain(journal::active_directories()?.into_iter())
                .chain(journal::active_directory_modes()?.into_iter())
                .max_by_key(|e| e.id);
            if let Some(e) = e {
                let question = match e.kind.as_str() {
                    "move" => format!("Move {} back to {}?", e.snapshot, e.path),
                    "dir_track" => format!(
                        "Remove the empty directory {}? Its contents will never be removed.",
                        e.path
                    ),
                    "dir_deleted" => format!("Recreate directory {}?", e.path),
                    "dir_mode" => {
                        let new_mode = e.snapshot.rsplit(':').next().unwrap_or("unknown");
                        let old_mode = e.snapshot.rsplit(':').nth(1).unwrap_or("unknown");
                        format!(
                            "Restore permissions on {} from {} to {}?",
                            e.path, new_mode, old_mode
                        )
                    }
                    _ if guard::would_overwrite(&e.path) => format!(
                        "Restore {} to its saved version? The current version will be overwritten.",
                        e.path
                    ),
                    _ => format!("Restore {}?", e.path),
                };
                if !guard::confirm(&question) {
                    println!("Cancelled. Nothing was changed.");
                } else {
                    restore::undo_entry(&e)?;
                    match e.kind.as_str() {
                        "move" => println!(
                            "Moved {} back to {}. Original history kept.",
                            e.snapshot, e.path
                        ),
                        "dir_track" => {
                            println!("Removed empty directory {}. Original history kept.", e.path)
                        }
                        "dir_deleted" => {
                            println!("Recreated directory {}. Original history kept.", e.path)
                        }
                        "dir_mode" => println!(
                            "Restored directory permissions on {}. Original history kept.",
                            e.path
                        ),
                        _ => println!("Restored {}. Original history kept.", e.path),
                    }
                }
            } else {
                println!("Nothing to undo.");
            }
        }
        Command::Redo => {
            if let Some(e) = restore::redo_last()? {
                println!("Redid the undo of entry #{}.", e.path);
            } else {
                println!("Nothing to redo.");
            }
        }
        Command::Checkpoint(n) => {
            let id = journal::next_id()?;
            journal::append(id, "checkpoint", &n, "")?;
            println!("Checkpoint '{n}' set at #{id}");
        }
        Command::Back(n) => {
            let id = journal::find_checkpoint(&n)?
                .ok_or_else(|| format!("no checkpoint named '{n}'"))?;
            rewind(id)?;
        }
        Command::To(t) => {
            let id: u64 = t
                .trim_start_matches('#')
                .parse()
                .map_err(|_| format!("'{t}' is not a valid entry id"))?;
            if id != 0 && !journal::read_all()?.iter().any(|e| e.id == id) {
                return Err(format!("no entry #{id} (see: lifeline log)"));
            }
            rewind(id)?;
        }
    }
    Ok(())
}
fn snap(path: &str) -> Result<(), String> {
    let abs = fs::canonicalize(path).map_err(|e| format!("cannot find {path}: {e}"))?;
    if !abs.is_file() {
        return Err(format!("{} is not a regular file", abs.display()));
    }
    let text = abs.to_str().ok_or("path is not valid UTF-8")?;
    let id = journal::next_id()?;
    let name = snapshot::store(id, &abs)?;
    if let Err(e) = journal::append(id, "snap", text, &name) {
        snapshot::remove(&name);
        return Err(e);
    }
    println!("Saved #{id}: {text}");
    Ok(())
}
fn rewind(id: u64) -> Result<(), String> {
    let snaps = journal::active_snapshots()?;
    let moves = journal::active_moves()?;
    let directories = journal::active_directories()?;
    let directory_modes = journal::active_directory_modes()?;
    let count = snaps
        .iter()
        .chain(moves.iter())
        .chain(directories.iter())
        .chain(directory_modes.iter())
        .filter(|e| e.id > id)
        .count();
    if count == 0 {
        println!("Already there. Nothing to rewind.");
        return Ok(());
    }
    if !guard::confirm(&format!("Rewind {count} saved change(s) back to #{id}? Each recovery will be recorded; history will be kept.")){
        println!("Cancelled. Nothing was changed.");return Ok(());
    }
    let done = restore::rewind_to(id)?;
    println!(
        "Rewound {done} entr{}; original history kept.",
        if done == 1 { "y" } else { "ies" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system_status() {
        let args = vec!["system".to_string(), "status".to_string()];
        assert!(matches!(parse(&args), Ok(Command::SystemStatus)));
    }

    #[test]
    fn parses_fixed_system_capture_command() {
        let args = vec!["system".to_string(), "capture-hostname".to_string()];
        assert!(matches!(parse(&args), Ok(Command::SystemCaptureHostname)));
    }

    #[test]
    fn parses_system_history_command() {
        let args = vec!["system".to_string(), "history".to_string()];
        assert!(matches!(parse(&args), Ok(Command::SystemHistory)));
    }

    #[test]
    fn parses_system_restore_command() {
        let args = vec![
            "system".to_string(),
            "restore".to_string(),
            "0123456789abcdef0123456789abcdef".to_string(),
        ];
        assert!(matches!(parse(&args), Ok(Command::SystemRestore(_))));
    }

    #[test]
    fn rejects_incomplete_system_command() {
        let args = vec!["system".to_string()];
        assert_eq!(
            parse(&args).err().as_deref(),
            Some("usage: lifeline system {status|capture-hostname|history|restore <id>}")
        );
    }

    #[test]
    fn rejects_extra_system_arguments() {
        let args = vec![
            "system".to_string(),
            "capture-hostname".to_string(),
            "anything".to_string(),
        ];
        assert_eq!(
            parse(&args).err().as_deref(),
            Some("usage: lifeline system {status|capture-hostname}")
        );
    }
}
