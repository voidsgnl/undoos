//! Argument parsing and command dispatch: snap, log, undo, to, checkpoint, back.

use std::fs;

use crate::{guard, journal, restore, snapshot};

pub const USAGE: &str = "\
lifeline - a system-wide undo button for Linux

USAGE:
    lifeline <command> [args]

COMMANDS:
    snap <file>          Remember a file as it is right now
    log                  Show everything lifeline remembers
    undo                 Put back the most recently saved file
    checkpoint <name>    Drop a named marker in the timeline
    back <name>          Rewind everything saved after a checkpoint
    to <id>              Rewind everything saved after entry #id
    help                 Show this message
    --version            Show the version";

pub enum Command {
    Help,
    Version,
    Log,
    Snap(String),
    Undo,
    Checkpoint(String),
    Back(String),
    To(String),
}

fn arg(args: &[String], usage: &str) -> Result<String, String> {
    args.get(1)
        .cloned()
        .ok_or_else(|| format!("usage: {usage}"))
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => Ok(Command::Help),
        Some("--version") | Some("-V") => Ok(Command::Version),
        Some("log") => Ok(Command::Log),
        Some("undo") => Ok(Command::Undo),
        Some("snap") => arg(args, "lifeline snap <file>").map(Command::Snap),
        Some("checkpoint") => arg(args, "lifeline checkpoint <name>").map(Command::Checkpoint),
        Some("back") => arg(args, "lifeline back <name>").map(Command::Back),
        Some("to") => arg(args, "lifeline to <id>").map(Command::To),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

pub fn run(cmd: Command) -> Result<(), String> {
    match cmd {
        Command::Help => println!("{USAGE}"),
        Command::Version => println!("lifeline {}", env!("CARGO_PKG_VERSION")),
        Command::Log => {
            let entries = journal::read_all()?;
            if entries.is_empty() {
                println!("Nothing saved yet. Try: lifeline snap <file>");
            }
            for entry in entries {
                println!("{}", entry.display());
            }
        }
        Command::Snap(path) => snap(&path)?,
        Command::Undo => undo()?,
        Command::Checkpoint(name) => {
            let id = journal::next_id()?;
            journal::append(id, "checkpoint", &name, "")?;
            println!("Checkpoint '{name}' set at #{id}");
        }
        Command::Back(name) => {
            let id = journal::find_checkpoint(&name)?
                .ok_or_else(|| format!("no checkpoint named '{name}'"))?;
            rewind(id)?;
        }
        Command::To(text) => {
            let id: u64 = text
                .trim_start_matches('#')
                .parse()
                .map_err(|_| format!("'{text}' is not a valid entry id"))?;
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
    let abs_str = abs.to_str().ok_or("path is not valid UTF-8")?;
    let id = journal::next_id()?;
    let name = snapshot::store(id, &abs)?;
    if let Err(err) = journal::append(id, "snap", abs_str, &name) {
        snapshot::remove(&name);
        return Err(err);
    }
    println!("Saved #{id}: {abs_str}");
    Ok(())
}

fn undo() -> Result<(), String> {
    let entries = journal::read_all()?;
    let last = match entries.iter().rev().find(|e| e.kind == "snap") {
        Some(entry) => entry.clone(),
        None => {
            println!("Nothing to undo.");
            return Ok(());
        }
    };
    let question = if guard::would_overwrite(&last.path) {
        format!(
            "Restore {} to its saved version? The current version will be overwritten.",
            last.path
        )
    } else {
        format!("Restore {}?", last.path)
    };
    if !guard::confirm(&question) {
        println!("Cancelled. Nothing was changed.");
        return Ok(());
    }
    restore::undo_last()?;
    println!("Restored {}", last.path);
    Ok(())
}

fn rewind(id: u64) -> Result<(), String> {
    let count = journal::read_all()?.iter().filter(|e| e.id > id).count();
    if count == 0 {
        println!("Already there. Nothing to rewind.");
        return Ok(());
    }
    let question = format!(
        "Rewind {count} saved change(s) back to #{id}? Current versions of those files will be overwritten."
    );
    if !guard::confirm(&question) {
        println!("Cancelled. Nothing was changed.");
        return Ok(());
    }
    let done = restore::rewind_to(id)?;
    println!("Rewound {done} entr{}.", if done == 1 { "y" } else { "ies" });
    Ok(())
}
