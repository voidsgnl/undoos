//! lifeline-daemon: watches folders and quietly saves the old version of
//! anything that changes or disappears. Get it back with `lifeline undo`.

use std::{env, path::PathBuf, process, sync::mpsc, thread, time::Duration};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use undoos::config::Config;
use undoos::journal;
use undoos::watcher::Watcher;

const USAGE: &str = "\
lifeline-daemon - saves the old version of files as they change

USAGE:
    lifeline-daemon [--config <file>] [--once]

OPTIONS:
    --config <file>   Config file (default: $LIFELINE_CONFIG, then
                      ~/.config/lifeline/config.toml)
    --once            Scan one time and exit instead of running forever
    -h, --help        Show this message";

fn fail(message: &str) -> ! {
    eprintln!("lifeline-daemon: {message}");
    process::exit(2);
}

fn default_config() -> Option<PathBuf> {
    if let Ok(p) = env::var("LIFELINE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let home = env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/lifeline/config.toml");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn print_scan(watcher: &mut Watcher) {
    for event in watcher.scan() {
        println!("{event}");
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut config_path: Option<PathBuf> = None;
    let mut once = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => once = true,
            "--config" => {
                i += 1;
                match args.get(i) {
                    Some(p) => config_path = Some(PathBuf::from(p)),
                    None => fail("--config needs a file path"),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown option: {other}")),
        }
        i += 1;
    }

    let path = config_path.or_else(default_config).unwrap_or_else(|| {
        fail("no config file found. Copy lifeline/config.toml to ~/.config/lifeline/config.toml")
    });
    let cfg = Config::load(&path).unwrap_or_else(|e| fail(&e));

    let mut watcher = Watcher::new(&cfg);
    if watcher.roots().is_empty() {
        fail("none of the folders in the config exist");
    }

    println!(
        "lifeline-daemon: watching {} folder(s), keeping history in {}",
        watcher.roots().len(),
        journal::home().display()
    );
    for root in watcher.roots() {
        println!("  {}", root.display());
    }

    if once {
        print_scan(&mut watcher);
        return;
    }

    // Register filesystem watches before the first scan. This closes the gap
    // where a change could otherwise happen between the initial scan and
    // watch registration. Periodic scans remain enabled as reconciliation:
    // they catch missed events, watcher errors, and changes during downtime.
    let (tx, rx) = mpsc::channel();
    let mut fs_watcher: RecommendedWatcher =
        notify::recommended_watcher(move |result| {
            let _ = tx.send(result);
        })
        .unwrap_or_else(|e| fail(&format!("cannot initialize filesystem watcher: {e}")));

    let mut watched_roots = 0usize;
    for root in watcher.roots() {
        match fs_watcher.watch(root, RecursiveMode::Recursive) {
            Ok(()) => watched_roots += 1,
            Err(e) => eprintln!(
                "lifeline-daemon: event watch unavailable for {}: {e}; periodic scanning remains active",
                root.display()
            ),
        }
    }
    if watched_roots == 0 {
        eprintln!("lifeline-daemon: no event watches active; using periodic scans only");
    }

    print_scan(&mut watcher);
    loop {
        // Any filesystem event triggers a reconciliation scan. Multiple events
        // are naturally coalesced, avoiding one full walk per individual write.
        // The timeout guarantees a full scan even if notifications are lost.
        match rx.recv_timeout(Duration::from_millis(cfg.poll_ms)) {
            Ok(Ok(_event)) => {
                while rx.try_recv().is_ok() {}
                print_scan(&mut watcher);
            }
            Ok(Err(error)) => {
                eprintln!("lifeline-daemon: filesystem notification error: {error}");
                print_scan(&mut watcher);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => print_scan(&mut watcher),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("lifeline-daemon: filesystem event channel closed; periodic scanning only");
                loop {
                    thread::sleep(Duration::from_millis(cfg.poll_ms));
                    print_scan(&mut watcher);
                }
            }
        }
    }
}
