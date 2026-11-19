//! The "Hold on, are you sure?" warnings.

use std::io::{self, Write};
use std::path::Path;

/// True if restoring would replace a file that exists right now.
pub fn would_overwrite(path: &str) -> bool {
    Path::new(path).exists()
}

/// Ask a yes/no question. Defaults to no. Set `LIFELINE_YES` to skip the prompt.
pub fn confirm(question: &str) -> bool {
    if std::env::var_os("LIFELINE_YES").is_some() {
        return true;
    }
    eprint!("Hold on. {question} [y/N] ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}
