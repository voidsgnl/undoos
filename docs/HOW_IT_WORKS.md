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
| `lifeline/daemon.rs` | Runs native filesystem event monitoring and periodic reconciliation |

## Directory history (initial stage)

The watcher records directory presence transitions in the same append-only journal:
- `dir_track`: a directory was first observed or reappeared.
- `dir_deleted`: a previously observed directory is now missing.

This includes empty directories and nested empty directories. Deletions are recorded deepest first so a future recovery operation can recreate parents before children. This stage records directory history only; directory undo/redo is not implemented yet. Unreadable ancestors and unavailable watched roots must not be interpreted as directory deletions.

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
- `move`: a same-filesystem move; its path and snapshot fields hold the original and new paths.
- `undo_pending` / `redo_pending`: durable intents written before replacing a file.
- `undo_abort` / `redo_abort`: records that an interrupted file operation had not changed the live file.
- `move_undo_pending` / `move_redo_pending`: durable intents written before renaming a tracked file.
- `move_undo_abort` / `move_redo_abort`: records that an interrupted move did not happen.

For a restore event, an empty snapshot field means the file did not exist immediately before the restore. This is important when undoing a deletion: redo must delete the restored file again.

## Rewinding

To rewind to entry `N`, Lifeline selects saved entries newer than `N` that are currently active and restores them newest first. If the same file was saved several times, the oldest relevant version ends up on disk. Every restoration is appended to the journal. The old entries and their recovery copies are retained.

## Safety notes and limitations

- Snapshots are full copies, so they use real disk space. Deduplication or deltas are planned.
- Baseline and snapshot files are staged in temporary files and renamed into place, reducing the chance that an interrupted write leaves a partial recovery copy. Baseline reads are retried if the source changes while being copied.
- Every distinct version the watcher observes can now become a recovery point; the legacy `min_gap_s` setting is ignored. Very short-lived intermediate writes that occur entirely between observations cannot be recovered.
- Recovery currently operates on regular files; it does not yet reverse package installs, system settings, permissions, service changes, or remote actions. Directory moves are represented as individual file moves; empty directories are not independently journaled. Files inside a deleted directory tree can be recovered, but empty directory entries are not yet independently preserved.
- The journal is append-only through normal CLI recovery flows.
- Every stateful CLI command takes one exclusive process lock before reconciliation and holds it until the command finishes. The daemon takes the same lock around each scan, not for its whole lifetime. This prevents two Lifeline processes from allocating the same journal ID, reconciling the same pending transaction, or changing baselines at the same time. The lock uses the operating system's `flock`; the kernel releases it automatically if a process exits or is killed. The lock file itself may remain on disk and is not evidence that Lifeline is still running. It is not yet tamper-evident or protected against manual edits or disk failure.
- Recovery copies are retained; automatic retention and garbage collection are not implemented.
- File restoration is staged in a temporary file beside the destination, flushed, and installed with an atomic rename. The containing directory is synced after the rename, reducing the chance of a partial file being exposed after interruption.
- File undo and redo now write a durable pending-intent record after saving the pre-operation contents and before replacing the live file. On the next Lifeline CLI command, reconciliation compares the live file with both the intended snapshot and the pre-operation copy. If it matches the intended state, Lifeline records the missing completion; if it still matches the pre-operation state, Lifeline records an abort. If it matches neither, Lifeline stops and leaves the file untouched for manual attention. Move undo/redo also write durable intent records before renaming. On the next CLI command, reconciliation checks which of the two paths exists: the original source means the move did not happen and is recorded as aborted; the destination means the rename happened and is recorded as complete. If both or neither path exists, Lifeline stops rather than guessing. Baseline metadata relocation is also made repeatable so a restart can finish it if the process stopped part-way through.
- For watched files with a baseline, undo and redo compare the live file with the last state Lifeline observed. If it differs or has disappeared unexpectedly, recovery stops and preserves the newer filesystem state. After a successful restore, the baseline is refreshed so successive undos remain consistent. Move recovery also checks persisted file identity when the filesystem provides it. Explicit one-off snapshots without a watcher baseline cannot yet receive this same content-conflict check.
- Paths and checkpoint names cannot contain tabs or newlines.

## Product direction

UndoOS is being developed as a Linux-first recovery layer with automatic background monitoring and a deliberately small command-line interface. The current implementation is limited to configured folders; system-wide coverage is a longer-term goal, not a current capability. Package changes, selected settings and dotfiles, permissions, and service changes are future recovery areas that require their own safe capture and conflict-handling logic. A tray application, optional dashboard, and plain-language monthly review are also later-stage ideas. See [the roadmap](ROADMAP.md) for the agreed direction and milestone order.

## Running the tests

```sh
make test
```

Tests run the real `lifeline` binary against a temporary `LIFELINE_HOME`, so they never touch your actual data.


## Rename and move handling

On Linux, Lifeline stores each tracked file's device/inode identity and Linux creation time (statx birth time) beside its baseline. Creation time remains stable across a rename, unlike change time, and helps prevent a reused inode from being mistaken for the same file. On every scan, Lifeline can match a missing old path to a newly found path with the same identity, including on the first scan after a daemon restart. It records a reversible `move` event and relocates the baseline association. `lifeline undo` moves it back; `lifeline redo` reapplies the move. Existing baselines from older versions receive identity metadata when their original file is next scanned.

A directory move is represented as one move event per tracked file inside it. Empty directories are not independently journaled. The destination must be inside a watched folder on the same filesystem. Cross-filesystem moves and moves outside watched roots are treated as ordinary disappearance/new-file events. If multiple hard links make the identity ambiguous, or the filesystem does not expose birth time, Lifeline does not guess. Undo refuses to overwrite an existing path.
