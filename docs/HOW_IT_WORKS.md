# How it works

Written for contributors. No jargon.

## The idea

Lifeline keeps two things:

1. **A notebook** (the *journal*). Every saved change and recovery action gets a line: what, where, when.
2. **A shelf** (the *snapshots*). Saved file contents, labelled with journal entry IDs.

The journal is append-only during normal operation. Undoing does not delete the original entry or its snapshot. Instead, Lifeline appends a `restore` event that points back to the saved entry. Redo appends a `redo` event that points to the restore event it reverses.

## Where the code lives

| File | Job |
|---|---|
| `src/main.rs` | Starts the program |
| `src/cli.rs` | Reads commands and dispatches them |
| `src/journal.rs` | Reads and appends journal events; determines which snapshots are active |
| `src/snapshot.rs` | Stores saved file copies |
| `src/restore.rs` | Applies snapshots and records undo/redo events |
| `src/guard.rs` | Asks before overwriting |
| `src/watcher.rs` | Scans watched folders for changed and deleted files |
| `lifeline/daemon.rs` | Runs the watcher in the background |

## Journal format

`journal.tsv`, one entry per line, tab-separated:

```
id  unix_time  kind  path_or_target_id  snapshot
```

Kinds currently include:
- `snap`: a saved file version.
- `checkpoint`: a named timeline marker.
- `restore`: an undo action; its path field identifies the snapshot entry it compensates.
- `redo`: a redo action; its path field identifies the restore event it reverses.

For a restore event, an empty snapshot field means the file did not exist immediately before the restore. This is important when undoing a deletion: redo must delete the restored file again.

## Rewinding

To rewind to entry `N`, Lifeline selects saved entries newer than `N` that are currently active and restores them newest first. If the same file was saved several times, the oldest relevant version ends up on disk. Every restoration is appended to the journal. The old entries and their recovery copies are retained.

## Safety notes and limitations

- Snapshots are full copies, so they use real disk space. Deduplication or deltas are planned.
- Recovery currently operates on regular files; it does not yet reverse package installs, system settings, directories, permissions, or remote actions.
- The journal is append-only through normal CLI recovery flows. It is not yet tamper-evident or protected against manual edits or disk failure.
- Recovery copies are retained; automatic retention and garbage collection are not implemented.
- Restores currently use file copies and are not a fully transactional filesystem operation. A power loss during a restore can still require manual inspection.
- Paths and checkpoint names cannot contain tabs or newlines.

## Running the tests

```sh
make test
```

Tests run the real `lifeline` binary against a temporary `LIFELINE_HOME`, so they never touch your actual data.
