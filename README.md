# UndoOS

> **`lifeline undo`** — That's it. That's the whole recovery strategy.

Lifeline for your Linux PC. A system-wide undo button that remembers your files, so you can rewind a mistake in one command.

```console
$ lifeline snap ~/thesis.odt      # remember this file as it is right now
$ rm ~/thesis.odt                 # ...oops
$ lifeline undo                   # Hold on. Restore /home/you/thesis.odt? [y/N] y
Restored /home/you/thesis.odt
```

## Status: early development

Here is what works today and what is still being built.

| Feature | State |
|---|---|
| Manual file snapshots (`lifeline snap`) | Works |
| Undo the last saved file (`lifeline undo`) | Works |
| Named checkpoints and rewinding (`checkpoint`, `back`, `to`) | Works |
| Change history (`lifeline log`) | Works |
| Folder watcher and background daemon | Works for configured folders |
| User-level systemd service setup during default install | Works where systemd user services are available |
| Package installs and removals | Planned |
| System settings and dotfiles | Planned |
| Space-saving snapshots (dedup/deltas) | Planned |

The default configuration watches `~/Documents`. The installer creates that configuration if it does not already exist and attempts to enable the user-level background service. Existing configuration is preserved. You can change watched folders in `~/.config/lifeline/config.toml`.

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
| `lifeline log` | Show everything Lifeline remembers |
| `lifeline undo` | Put back the most recently saved file |
| `lifeline checkpoint <name>` | Drop a named marker in the timeline |
| `lifeline back <name>` | Rewind everything saved after that marker |
| `lifeline to <id>` | Rewind everything saved after entry `#id` |

Full details and examples: [docs/COMMANDS.md](docs/COMMANDS.md).

## How it works

Lifeline keeps a notebook (the *journal*), a shelf of saved copies (*snapshots*), and a baseline copy of each tracked file so it can preserve the previous version when a later scan notices a change. See [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## Contributing

Issues and pull requests are welcome. `make test` runs the test suite. Start with [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## License

[MIT](LICENSE)
