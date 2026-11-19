//! lifeline: the command-line face of UndoOS.

mod cli;
mod guard;
mod journal;
mod restore;
mod snapshot;
mod watcher;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(cmd) => {
            if let Err(err) = cli::run(cmd) {
                eprintln!("lifeline: {err}");
                std::process::exit(1);
            }
        }
        Err(msg) => {
            eprintln!("lifeline: {msg}\n");
            eprintln!("{}", cli::USAGE);
            std::process::exit(2);
        }
    }
}
