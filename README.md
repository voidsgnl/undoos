# UndoOS

> **`lifeline undo`** — That's it. That's the whole backup strategy.

Lifeline for your Linux PC. A system-wide undo button that remembers your files, so you can rewind a mistake in one command.

```console
$ lifeline snap ~/thesis.odt      # remember this file as it is right now
$ rm ~/thesis.odt                 # ...oops
$ lifeline undo                   # Hold on. Restore /home/you/thesis.odt? [y/N] y
Restored /home/you/thesis.odt
```

## Status: early scaffold

UndoOS is just getting started. Here is what works today and what doesn't.

| Feature | State |
|---|---|
| Snapshot a file (`lifeline snap`) | Works |
| Undo the last change (`lifeline undo`) | Works |
| Named checkpoints and rewinding (`checkpoint`, `back`, `to`) | Works |
| Change history (`lifeline log`) | Works |
| "Are you sure?" warnings | Works |
| Automatic background watching (`lifeline-daemon`) | Planned |
| Package installs and removals | Planned |
| System settings and dotfiles | Planned |
| Space-saving snapshots (dedup/deltas) | Planned |

Today a snapshot is a plain copy of the file, and you take it yourself with `lifeline snap`. The background watcher that does this for you is the next big piece.

## Install

Requires a recent stable [Rust toolchain](https://rustup.rs).

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

See [docs/INSTALL.md](docs/INSTALL.md) for other options.

## Commands

| Command | What it does |
|---|---|
| `lifeline snap <file>` | Remember a file as it is now |
| `lifeline log` | Show everything lifeline remembers |
| `lifeline undo` | Put back the most recently saved file |
| `lifeline checkpoint <name>` | Drop a named marker in the timeline |
| `lifeline back <name>` | Rewind everything saved after that marker |
| `lifeline to <id>` | Rewind everything saved after entry `#id` |

Full details and examples: [docs/COMMANDS.md](docs/COMMANDS.md).

## How it works

Lifeline keeps a notebook (the *journal*) and a shelf of saved copies (*snapshots*). Undoing means reading the notebook backwards and putting copies back. See [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## Contributing

Issues and pull requests are welcome. `make test` runs the test suite. Start with [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md).

## License

[MIT](LICENSE)
