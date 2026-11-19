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
| Named checkpoints and selective rewind | Works |
| Change history (`lifeline log`) | Works |
| Folder watcher and background daemon | Works for configured folders |
| User-level systemd service setup | Works where systemd user services are available |
| Package installs and removals | Planned |
| System settings and dotfiles | Planned |
| Space-saving snapshots (dedup/deltas) | Planned |

The default configuration watches `~/Documents`. The installer creates that configuration if it does not already exist and attempts to enable the user-level background service. Existing configuration is preserved. Change watched folders in `~/.config/lifeline/config.toml`.

**Important:** snapshots are currently full file copies. Lifeline does not yet monitor every location or every kind of system change. Only regular files inside configured folders are tracked, and files above the configured size limit are skipped.

## Install

Requires a recent stable [Rust toolchain](https://rustup.rs).

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

The default install places binaries in `~/.local/bin`, creates a default configuration, and attempts to start `lifeline-daemon` as a user systemd service. Make sure `~/.local/bin` is on your `PATH`.

See [docs/INSTALL.md](docs/INSTALL.md) for other options.

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

Full details: [docs/COMMANDS.md](docs/COMMANDS.md).

## How it works

Lifeline keeps a notebook (the *journal*), a shelf of saved copies (*snapshots*), and a baseline copy of each tracked file. Recovery actions append new journal entries; they do not erase the original entries or snapshots. See [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## Contributing

Issues and pull requests are welcome. `make check` checks formatting, builds the release binaries, and runs the test suite locally. `make test` runs only the test suite.

## License

[MIT](LICENSE)


### Renames

On Linux, Lifeline persists device/inode identity plus Linux file creation time with each baseline, allowing it to recognize a same-filesystem rename even when the daemon was stopped during the move. Filesystems that do not expose creation time are not used for move inference. `lifeline undo` moves the file back and `lifeline redo` reapplies the move. Directory moves are tracked per file; empty directories and cross-filesystem moves are not yet modeled as single operations.
