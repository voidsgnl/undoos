# UndoOS roadmap

This document records the product direction and the order in which capabilities are intended to grow. It is a guide, not a promise that every planned item is already implemented or has a fixed release date.

## Product purpose

UndoOS is intended to become a dependable undo and recovery layer for a Linux computer. Its job is to notice meaningful changes, keep enough history to understand them, and help the user reverse mistakes safely.

The product should feel like a safety net, not a system-administration tool that users must constantly manage.

## Design principles

- **Linux first.** Build and validate the Linux experience before considering other operating systems.
- **Automatic by default.** After installation, the background monitor should work with sensible, clearly explained defaults. Users should not need to manually create a snapshot for every ordinary change.
- **Simple commands.** Keep the everyday interface short and predictable. The command line should feel approachable; complex recovery internals belong behind it.
- **Safety before cleverness.** Never overwrite a newer or unrelated change just to complete an undo. When the state is ambiguous, stop, explain the conflict, and preserve the user's data.
- **History is retained.** Recovery should add records of what it changed rather than silently erase the original history.
- **Incremental scope.** Add one reliable kind of recovery at a time. Test normal use, conflicts, interruptions, and restart behavior before expanding coverage.
- **Clear boundaries.** Tell users what is monitored, what is not monitored, and what UndoOS can and cannot recover.

## Current foundation

The current prototype provides:

- A Rust command-line application named `lifeline` and a background `lifeline-daemon`.
- An opt-in root-owned systemd socket/service with typed status, capture, history, and restore operations restricted to `/etc/hostname`.
- A separate root-owned inotify observer that automatically establishes a hostname baseline, captures completed content writes and atomic replacements of `/etc/hostname`, and periodically reconciles the latest file state as a fallback when events are missed.
- Configurable watched folders, with `~/Documents` as the default.
- File baselines and saved copies for observed file changes and deletions.
- `lifeline log`, `undo`, and `redo`, plus manual snapshots and named checkpoints.
- An append-only journal for recovery history.
- Conflict checks and reconciliation for interrupted file undo/redo, tracked file moves, and empty-directory operations.
- Linux file-identity support for recognizing certain same-filesystem renames.
- Independent empty-directory undo/redo, including crash reconciliation and non-empty-directory protection.
- Directory permission-mode detection and independent undo/redo with identity checks, conflict protection, and interrupted-operation reconciliation.

This is not yet whole-system protection. The current watcher tracks regular file contents and directory presence inside configured folders; it does not automatically monitor every system location or every type of system change.

## Completed milestone: directory structure recovery

This milestone extended file recovery to preserve empty directories and empty subdirectories, not just the files they contain.

The completed capability now:

1. Detect directory creation and deletion within configured watched roots.
2. Preserve empty directories as well as directory structure around tracked files.
3. Restore deleted directory structure in a safe parent-before-child order.
4. Support undo and redo without overwriting an unrelated file or directory.
5. Avoid treating an unreadable directory, missing mount, or temporarily unavailable watched root as a mass deletion.
6. Preserve the existing simple CLI and append-only history model.
7. Include tests for empty directories, nested empty directories, populated trees, conflicts, and daemon restarts.

A directory tree is not one atomic transaction: its individual directory and file events are reversed in timeline order.

## Next system recovery milestone: deletion and recreation

Before adding more protected system paths, complete deletion recovery for the existing `/etc/hostname` adapter. This is a separate recovery state, not an empty-file snapshot. The implementation must:

1. Persist an explicit, checksummed deletion event in the append-only system history while retaining the preceding content object.
2. Distinguish a genuinely missing target from permission errors, I/O errors, an unavailable `/etc`, or an unsafe replacement such as a symlink. Only confirmed `ENOENT` for the exact allowlisted entry is a deletion.
3. Restore a deleted hostname by safely creating the exact allowlisted file with the captured ownership and mode, without following links or overwriting a file that appeared after deletion.
4. Use a durable transaction record that represents an originally absent target, so service startup can remove only a partially created file from an interrupted restore. Existing in-place restore rollback behavior must remain intact.
5. Sync the new file and containing directory, verify the restored bytes and metadata, and retain all earlier recovery history.
6. Test deletion detection, repeated deletion notifications, recreation, concurrent recreation conflicts, symlink substitution, permission failures, interrupted creation, service restart, and compatibility with existing catalog records.

Do not report deletion as recoverable until both the deletion event and the crash-safe recreation path pass tests. Keep the adapter restricted to `/etc/hostname` throughout this milestone.

## Later recovery coverage

After file and directory recovery are dependable, the longer-term goal is to expand beyond ordinary files. Candidate areas include:

- Application and package installation or removal.
- User and desktop settings, including selected configuration files and dotfiles.
- Ownership, ACL, and extended-attribute changes.
- System and user services.
- Other reversible system changes where UndoOS can reliably capture the prior state and detect conflicts.

These areas require different kinds of evidence and recovery logic. They should not be treated as ordinary file snapshots, and they should be added only when the operation can be reversed safely and tested thoroughly.

## System-wide Wayback/Wayforward direction

The long-term product is one coherent timeline of meaningful Linux changes, not a collection of unrelated backup tools. The same user-facing history should eventually include files, package transactions, service state, and supported system settings.

This requires more than expanding the current folder list or running the existing user daemon as root. Different change domains need dedicated observers and recovery adapters. The target design separates the unprivileged CLI and per-user agent from a narrowly privileged system service, with authenticated local IPC and separately protected system recovery storage.

The implementation order is:

1. Establish the privileged service, authorization boundary, IPC protocol, and system-owned storage. **Foundation implemented.** The root-owned socket service uses `SO_PEERCRED`, Polkit for user-requested capture and restore, protected object storage, an append-only catalog, and crash reconciliation. Its typed operations are restricted to `/etc/hostname`.
2. Add carefully scoped system filesystem coverage with mount-boundary and virtual-filesystem exclusions. **Initial one-file prototype implemented:** a separate root-owned inotify observer establishes a baseline and records completed writes or atomic replacements of `/etc/hostname`. It does not yet observe deletion, metadata-only changes, arbitrary paths, or every intermediate version when events coalesce or the queue overflows. General filesystem coverage, mount-boundary handling, and broader exclusions remain future work.
3. Add package-manager transaction capture and recovery.
4. Add supported systemd service-state recovery.
5. Add settings, account, ownership, ACL, and other domains only when their previous state can be captured and safely restored.
6. Integrate all supported domains into dependency-aware Wayback and Wayforward operations.

The architecture and safety requirements are documented in [System-wide architecture](SYSTEM_ARCHITECTURE.md). This is the target design; none of the privileged system-wide capabilities above should be considered implemented yet.

## User experience

### Command line first

The CLI remains the primary interface during core development. Everyday commands should stay short and understandable. New capabilities should not force users to learn a large collection of flags or internal terminology.

The intended experience is that monitoring runs in the background while the user can inspect activity and recover from mistakes with a small set of commands.

### Tray and desktop interface later

A lightweight system-tray application is a later phase, not a prerequisite for the recovery engine. It should provide a quick status view and a simple way to open a fuller desktop dashboard when the user chooses.

The dashboard may eventually show current monitoring status, recent activity, alerts, evidence, and recovery controls. This work should begin only after the core monitoring and recovery behavior is dependable.

## Periodic review

A future optional monthly review should explain what UndoOS has helped with in plain language. Possible information includes:

- Changes recorded and recoveries performed.
- Estimated time saved by undoing mistakes.
- Repeated patterns that may be worth reviewing.
- Preferences or protected locations the user has explicitly chosen or consistently preserved.
- One optional, practical suggestion for simplifying or protecting a recurring workflow.

This should feel like a helpful personal report, not a surveillance dashboard. It should be transparent about how observations are used, avoid exposing unnecessary file contents, and leave the user in control.

## Storage and reliability

The current implementation stores full file copies. Future work may add content hashing, deduplication, or delta storage to reduce disk use. These optimizations must preserve integrity checks and must not weaken recovery reliability.

Retention and cleanup policies are also future work. They must be explicit and conservative so that space management does not unexpectedly remove the only recovery copy a user expects to have.

## How we develop

For each milestone:

1. Define the user-visible problem and its boundaries.
2. Choose the smallest reliable design that fits the existing architecture.
3. Implement the capability without breaking existing behavior.
4. Test ordinary cases, conflicts, interruptions, and restart/reconciliation cases where relevant.
5. Update the user and contributor documentation.
6. Move to the next capability only after the checks pass.

The guiding rule is simple: make the recovery engine trustworthy before making the product bigger.
