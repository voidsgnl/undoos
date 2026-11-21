//! UndoOS: the library behind the `lifeline` and `lifeline-daemon` programs.

pub mod baseline;
pub mod cli;
pub mod config;
pub mod guard;
pub mod journal;
pub mod lock;
pub mod restore;
pub mod snapshot;
pub mod system_client;
pub mod watcher;
