# UndoOS

> **lifeline undo** — That's it. That's the whole recovery strategy.

Lifeline for your Linux PC. A system-wide undo button that remembers your files, so you can rewind a mistake in one command.

```console
$ lifeline snap ~/thesis.odt
$ rm ~/thesis.odt
$ lifeline undo
Restored /home/you/thesis.odt. Original history kept.
$ lifeline redo
Redid the undo of entry #1.
```

## Status: early development

| Feature | State |
|---|---|
| Manual file snapshots (`lifeline snap`) | Works |
| Undo saved files (`lifeline undo`) | Works |
| Redo the latest undo (`lifeline redo`) | Works |
| Append-only recovery history | Implemented |
| Interrupted undo/redo reconciliation | Implemented for files, tracked moves, and directory operations |
| Named checkpoints and selective rewind | Works |
| Change history (`lifeline log`) | Works |
| Folder watcher and background daemon | Works for configured folders |
| Recovery of files inside deleted directory trees | Works |
| Empty-directory creation/deletion history | Implemented |
| Independent empty-directory undo/redo | Implemented |
| Directory permission-mode detection and undo/redo | Implemented |
| User-level systemd service setup | Works where systemd user services are available |
| Privileged system-service socket and read-only status check | Implemented |\n| Administrator-authorized capture of the fixed `/etc/hostname` path | Initial end-to-end capture operation implemented; no system-file restore yet |
| Package installs and removals | Planned |
| System settings and dotfiles | Planned |
| Space-saving snapshots (dedup/deltas) | Planned |

The default configuration watches `~/Documents`. The installer creates that configuration if it does not already exist and attempts to enable the user-level background service. Existing configuration is preserved. Change watched folders in `~/.config/lifeline/config.toml`.

**Important:** snapshots are currently full file copies. Lifeline does not yet monitor every location or every kind of system change. Regular file contents, directory presence, and directory permission modes are tracked inside configured folders. Empty-directory creation and deletion can be undone and redone independently. Directory permission changes can be undone and redone with identity and conflict checks. Directory operations refuse to remove non-empty directories. Files above the configured size limit are skipped.

## Install

Requires a recent stable [Rust toolchain](https://rustup.rs).

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

The default install places binaries in `~/.local/bin`, creates a default configuration, and attempts to start `lifeline-daemon` as a user systemd service. Make sure `~/.local/bin` is on your `PATH`.

See [docs/INSTALL.md](docs/INSTALL.md) for installation options, [docs/TESTING.md](docs/TESTING.md) for safe local testing, [docs/ROADMAP.md](docs/ROADMAP.md) for the product direction, and [docs/SYSTEM_ARCHITECTURE.md](docs/SYSTEM_ARCHITECTURE.md) for the system-wide Wayback/Wayforward design.

## Commands

| Command | What it does |
|---|---|
| `lifeline snap <file>` | Remember a file as it is now |
| `lifeline log` | Show the complete activity and recovery history |
| `lifeline undo` | Restore the most recent active saved state |
| `lifeline redo` | Reverse the most recent undo |
| `lifeline checkpoint <name>` | Drop a named marker in the timeline |
| `lifeline back <name>` | Rewind active saved changes after that marker |
| `lifeline to <id>` | Rewind active saved changes after entry `#id` |
| `lifeline system status` | Check the privileged system service through its local socket |\n| `lifeline system capture-hostname` | Capture the single approved system file, `/etc/hostname`, after administrator authentication |

Full details: [docs/COMMANDS.md](docs/COMMANDS.md).

The command line is the primary interface during core development. A lightweight tray application and optional full dashboard are later-stage plans, after monitoring and recovery are dependable.

## How it works

Lifeline keeps a notebook (the *journal*), a shelf of saved copies (*snapshots*), and a baseline copy of each tracked file. Recovery actions append new journal entries; they do not erase the original entries or snapshots. See [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## Contributing

Issues and pull requests are welcome. `make check` checks formatting, builds the release binaries, and runs the test suite locally. `make test` runs only the test suite.

## License

[MIT](LICENSE)


### Renames

On Linux, Lifeline persists device/inode identity plus Linux file creation time with each baseline, allowing it to recognize a same-filesystem rename even when the daemon was stopped during the move. Filesystems that do not expose creation time are not used for move inference. `lifeline undo` moves the file back and `lifeline redo` reapplies the move. Directory moves are currently tracked per file. Empty-directory creation and deletion are recorded and independently reversible. Directory moves are still represented as individual file moves; a directory tree is not one atomic transaction. See [the roadmap](docs/ROADMAP.md) for the next recovery milestone and longer-term direction.
