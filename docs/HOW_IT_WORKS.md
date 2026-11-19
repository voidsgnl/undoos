# How it works

Written for contributors. No jargon.

## The idea

Lifeline keeps two things:

1. **A notebook** (the *journal*). Every time something is saved, a line is added: what, where, when.
2. **A shelf** (the *snapshots*). A saved copy of each file, labelled with the same number as its notebook line.

Undoing is reading the notebook backwards and putting copies from the shelf back where they came from.

## Where the code lives

| File | Job |
|---|---|
| `src/main.rs` | Starts the program |
| `src/cli.rs` | Reads what you typed and runs the right thing |
| `src/journal.rs` | The notebook |
| `src/snapshot.rs` | The shelf |
| `src/restore.rs` | Puts things back |
| `src/guard.rs` | Asks "are you sure?" before overwriting |
| `src/watcher.rs` | Notices changes (not wired up yet) |
| `lifeline/daemon.rs` | Background service that will use the watcher (stub) |

## The journal format

`journal.tsv`, one entry per line, tab-separated:

```
id  unix_time  kind  path_or_name  snapshot
```

`kind` is `snap` (a saved file) or `checkpoint` (a named marker).

## Rewinding

To rewind to entry `N`, lifeline takes every entry newer than `N` and restores them **newest first**. A file saved several times ends up with its oldest saved copy, which is its state at `N`. Then the newer entries and their snapshots are deleted.

## Known limitations

- Snapshots are full copies, so they use real disk space. Deduplication or deltas are planned.
- You have to run `lifeline snap` yourself. The daemon that does it automatically is not built.
- Restoring overwrites the current file without saving it first, so there is no "undo the undo" yet. The confirmation prompt is the safety net.
- Only regular files are tracked. No package installs, settings, directories or permissions yet.
- Paths and checkpoint names cannot contain tabs or newlines.

## Running the tests

```sh
make test
```

Tests run the real `lifeline` binary against a temporary `LIFELINE_HOME`, so they never touch your actual data.
