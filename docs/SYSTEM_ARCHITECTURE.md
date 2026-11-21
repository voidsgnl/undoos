# System-wide architecture

This document defines how UndoOS can grow from its current folder watcher into a Linux-wide Wayback/Wayforward system. It describes the target architecture, not features that are already implemented.

## The goal

UndoOS should automatically build a trustworthy timeline of meaningful changes made to a Linux computer. A user should be able to inspect that timeline, move backwards, and move forwards without having to remember to create a manual snapshot first.

"System-wide" does not mean blindly copying every byte under `/`. Linux contains virtual filesystems, runtime state, mounted devices, secrets, sockets, and data that cannot safely be restored as ordinary files. It means covering the important change domains through the right observation and recovery mechanism for each domain.

## One timeline, multiple change sources

The user should see one coherent history. Internally, different subsystems must observe different kinds of changes:

| Change domain | Suitable source of evidence | Recovery information |
|---|---|---|
| User and system files | Filesystem event notifications plus periodic reconciliation | Previous content, metadata, identity, and path |
| Package operations | Package-manager transaction hooks and package database records | Packages and versions before and after, package files, dependencies, transaction result |
| Services | systemd D-Bus or manager state plus unit-file changes | Previous enabled/running state and unit configuration |
| System and desktop settings | Targeted configuration observation and application-specific adapters | Previous setting value or configuration version |
| Accounts and permissions | Dedicated account/permission adapters and filesystem metadata | Previous identity, ownership, mode, ACL, and related state |
| Storage layout | Filesystem and volume-manager-specific adapters | Snapshot or metadata needed to restore the prior layout safely |

A filesystem watcher alone cannot reliably understand package transactions, service state, or application settings. Likewise, audit logs can tell us that an operation happened but usually do not contain enough information to reverse it. Each domain needs an adapter that can capture the prior state and define a safe inverse operation.

## The change record contract

Every recoverable change source must provide a common record with:

- A unique event ID and transaction ID.
- When it happened and which source observed it.
- The affected object and its scope (user, system, or a specific mounted volume).
- The state before the change and the state after it, or durable references to that evidence.
- The operation needed to reverse it and the preconditions for doing so.
- Dependencies on other changes when order matters.
- A clear recovery capability: reversible, informational-only, or unsupported.
- Enough pending-operation information to reconcile safely after a crash or reboot.

The journal remains append-only during normal operation. Undo and redo append compensating events; they do not erase the original timeline. A change is not considered safely reversible merely because UndoOS can describe it.

## Process and privilege boundaries

The target design separates observation and recovery by privilege:

1. **User interface and CLI:** Runs as the logged-in user. It displays history and requests operations.
2. **User agent:** Watches that user's files and settings, and stores user-scoped recovery data.
3. **System services:** A narrowly scoped, root-owned control service handles authorized system recovery. Separate observer services watch only explicitly approved resources and write evidence to protected storage; observation does not grant those services permission to modify the watched system files.
4. **Authorization boundary:** The CLI communicates with the system service through a local, permission-restricted IPC interface. The kernel enforces the `root:lifeline` socket access policy, and the service reads the connecting process's PID, UID, and GID with Linux `SO_PEERCRED` before handling a request. Read-only history is available to socket members; capture and restore each require their own Polkit action bound to the kernel-identified caller's PID, process start time, and UID. The entire CLI must never run as root just to perform one recovery.
5. **Storage boundary:** System recovery data is protected from ordinary users; user recovery data remains owned by the relevant user. Journal and snapshot paths must not be writable by untrusted processes.

The system service must validate every request itself. It must not accept arbitrary shell commands or unrestricted filesystem paths from the user process. The first two operation-specific actions are `org.undoos.system.capture-file` and `org.undoos.system.recover-file`; both require administrator authentication in an active session and deny inactive or non-local requests by default. The service exposes typed operations, not a general-purpose root command runner. The control service's systemd sandbox uses `ProtectSystem=strict` and grants write access only to the protected recovery store and the exact approved `/etc/hostname` file through `ReadWritePaths`. The separate automatic observer has read-only access to `/etc` and write access only to the protected recovery store.

## Observation is not the same as recovery

Linux mechanisms such as inotify/fanotify, audit, filesystem journals, and package-manager logs provide different kinds of evidence. None is a universal undo engine:

- Event notifications can be coalesced, overflow, or arrive after a write has already happened. A periodic reconciliation can recover the latest visible state, but cannot recreate intermediate versions that were overwritten before they were captured.
- Audit records describe operations but do not automatically preserve overwritten contents.
- Package logs describe transactions but do not guarantee that reinstalling an old package version recreates all prior state.
- A system service can change state through several interfaces, so observing only its unit file is insufficient.

For every supported domain, UndoOS must capture enough prior state before it is lost, confirm the resulting state, and periodically reconcile its view with reality. If it cannot prove what happened, it must record uncertainty and refuse unsafe recovery rather than guess.

## Recovery model: Wayback and Wayforward

Undo and redo are compensating operations over the same ordered history:

- **Wayback:** Reverse eligible changes newest-first, respecting dependencies.
- **Wayforward:** Reapply the changes that were reversed, oldest-first where dependencies require it.
- **Conflict handling:** Check that the live state still matches the state UndoOS expects. If another process has made a newer or unrelated change, stop and explain the conflict.
- **Crash handling:** Write a durable intent before changing the system. On the next start, compare the live state with the expected-before and expected-after states; complete or abort only when the result is unambiguous.
- **Partial recovery:** Report exactly which operations succeeded and which were skipped or blocked. Never describe a partially completed rewind as fully successful.

A system timeline is not necessarily one atomic transaction. Some changes have external effects that cannot be rolled back (for example, data already sent to a remote server). Such events may be recorded for context but must be labelled non-reversible.

## Scope and exclusions

The system service must not treat every path as ordinary user data. Initial filesystem scope should explicitly exclude or specially handle virtual and transient locations such as `/proc`, `/sys`, `/dev`, `/run`, sockets, and ephemeral mounts. It must identify filesystem and mount boundaries rather than accidentally traversing removable or network filesystems.

Large caches, build outputs, temporary files, and rapidly changing runtime data need explicit policy. Sensitive files require careful access controls and retention rules. A mount disappearing must never be interpreted as mass deletion. Recovery must never recursively remove user data merely to make a directory operation succeed.

UndoOS must not assume that the machine uses Btrfs, LVM, or any one snapshot-capable filesystem. Native filesystem snapshots can become an optional acceleration and consistency backend where available, not a prerequisite for basic operation.

## Development sequence

The system-wide transition should happen in verifiable stages:

1. **Privilege and storage foundation:** Define the root-owned system service, user-to-system IPC, authorization, and separate system recovery storage. Do not enable whole-root monitoring yet.
2. **System filesystem coverage:** Add a carefully scoped system filesystem adapter with mount-boundary handling, exclusions, baseline migration, and conflict-safe file recovery.
3. **Package transactions:** Add package-manager adapters that capture transaction intent and before/after package state. Begin with the supported distribution's package manager and test upgrades, installs, removals, interrupted transactions, and dependency interactions.
4. **Service state:** Capture and reverse supported systemd unit state changes with explicit dependency handling.
5. **Settings and other domains:** Add focused adapters only when their prior state can be captured and safely restored.
6. **Unified timeline:** Make cross-domain rewind and forward operations dependency-aware, with partial-result reporting and crash reconciliation.

Each stage must include normal, conflict, permission, interruption, restart, and failure tests before the next stage begins.

## Current implementation boundary

The current prototype does **not** provide system-wide protection. Its user-level daemon watches configured folders (default: `~/Documents`). It tracks regular-file contents, file moves, directory presence, and directory permission modes within those configured roots. A separate privileged system service is installed through systemd socket activation. Its local socket is owned by `root:lifeline` with mode `0660`; the installer adds the invoking login to the `lifeline` group. The CLI command `lifeline system status` uses a typed, versioned, read-only request to confirm that the service is responding. Before reading the request, the service obtains kernel-authenticated peer PID/UID/GID credentials with `SO_PEERCRED`; socket permissions remain the connection authorization boundary. The service obtains the caller's process start time from `/proc/<pid>/stat` and supplies the complete PID/start-time/UID subject tuple required by `pkcheck`, preventing PID-reuse ambiguity. Fixed-path capture and restore requests invoke separate Polkit actions; read-only status and history requests do not prompt for administrator authentication. The installer prepares `/var/lib/lifeline/system` as a root-only `0700` directory. Before accepting requests, the system service validates the fixed storage path, rejects symbolic links, and checks root ownership and restrictive permissions on each path component. It also initializes and validates a private `objects` subdirectory. The storage engine can write immutable, randomly identified object files using temporary files, `fsync`, and no-replace hard-link publication; each object carries a length and CRC-32 checksum that is checked when read. Object files use mode `0600`, and the object directory uses mode `0700`. An append-only `catalog.log` records each object's identifier, capture timestamp, original absolute path, size, mode, UID/GID, device, and inode; paths are stored as hexadecimal bytes so non-UTF-8 Linux filenames remain representable. Each catalog record has its own CRC-32 checksum. Appends are serialized with file locking and synced to disk; an interrupted final record is discarded before the next append, while malformed complete records cause startup to fail closed. The catalog is size-limited and rejects duplicate object identifiers. The checksums detect accidental corruption; root-only ownership and permissions are the access-control boundary, not a defense against a malicious root user. The object and catalog APIs are exposed through narrowly typed capture, history, and restore IPC operations for the single approved hostname file. A separate root-owned observer binary uses inotify on `/etc` and reacts only to completed writes or atomic replacements of the exact `hostname` entry; it stores an initial baseline and appends a new object only when the file content differs from the latest catalogued hostname version. An internal capture coordinator now demonstrates the complete store-and-catalog sequence for one regular file selected by an exact-path policy beneath a configured root. The policy accepts only non-empty relative paths explicitly listed by its caller. Capture traverses from an open root directory descriptor, opens each intermediate directory with `openat` and `O_NOFOLLOW`, and opens the final file with `O_NOFOLLOW | O_NONBLOCK`; this prevents symlink traversal and closes the path-prefix replacement window. It checks that the opened object is a regular file, checks file identity and metadata before and after reading, enforces a 64 MiB limit, verifies the stored object by reading it back, and only then appends catalog metadata. If catalog registration fails after object publication, the object is retained as an orphan rather than deleted under an uncertain durability outcome. The coordinator is used by the fixed-path manual capture operation and by the automatic hostname observer. The observer reads the current file without following a final symlink, checks file identity and metadata around the read, and then uses the same bounded capture coordinator to store and verify changed content. A read-only reconciliation pass now runs during service startup: it inventories published objects and leftover temporary files, verifies every catalog-referenced object's format, checksum, and recorded content length, detects duplicate catalog references, and reports unreferenced objects and interrupted temporary files. Missing or corrupt referenced objects prevent startup. Reconciliation never deletes objects or edits the catalog; orphan cleanup is deliberately deferred until a separately reviewed retention policy exists. The system observer does not monitor all of `/`; automatic system observation is currently limited to content writes and atomic replacements of `/etc/hostname`. Deletion, metadata-only changes, and intermediate versions lost to inotify event coalescing or queue overflow are not captured. Privileged restoration remains limited to the explicit `/etc/hostname` adapter. The first typed capture request is now available as `lifeline system capture-hostname`: the client can request only the fixed `CAPTURE-HOSTNAME` operation, and the service requires the separate Polkit action `org.undoos.system.capture-file` before capturing the exact allowlisted `/etc/hostname` file. The path is selected by the service, not supplied by the client. The existing safe-open, 64 MiB bound, source-state check, immutable object publication, read-back verification, and durable catalog append are used. The corresponding `HISTORY-HOSTNAME` and `RESTORE-HOSTNAME <id>` requests expose the latest 20 captures and an administrator-authorized restore for that same fixed path. Before an in-place restore, the service captures a fresh preimage and persists a checksummed pending transaction record. It writes through a no-follow descriptor only if the hostname file's inode and metadata still match the selected capture, verifies the resulting bytes, and removes the pending record only after syncing the file. If interrupted, service startup restores the preimage before accepting IPC. systemd grants write access to `/etc/hostname` only, not the rest of `/etc`. This remains a narrow first adapter; arbitrary paths and other system domains are not accepted. Package, service, account, and general system-setting changes are not currently reversible.

The existing CLI, journal, conflict checks, and crash-reconciliation work are foundations to build on. They are not evidence that whole-system recovery is already available.
