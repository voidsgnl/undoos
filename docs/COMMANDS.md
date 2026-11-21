# Commands

Every command is `lifeline <command>`.

## `snap <file>`

Remember a file exactly as it is right now.

```console
$ lifeline snap ~/thesis.odt
Saved #1: /home/you/thesis.odt
```

Use this before changing or deleting a file. The background watcher can also save earlier versions for configured folders.

## `log`

Show the complete journal, including original snapshots and later recovery actions.

```console
$ lifeline log
#1       5m ago  snap  /home/you/thesis.odt
#2       2m ago  restored    entry #1
```

## `undo`

Reverse the most recent active saved change. This can restore a saved file version, reverse a tracked file move, recreate a deleted directory, or remove a directory that Lifeline recorded being created. A directory is removed only when empty; Lifeline never recursively deletes its contents. The original history remains, and a new recovery entry is appended.

```console
$ lifeline undo
Restore /home/you/thesis.odt to its saved version? [y/N] y
Restored /home/you/thesis.odt. Original history kept.
```

## `redo`

Reverse the most recent undo. This reapplies the state that existed immediately before that undo, including directory creation or deletion where applicable.

```console
$ lifeline redo
Redid the undo of entry #1.
```

## `checkpoint <name>`

Drop a named marker in the timeline, like a save point in a game.

```console
$ lifeline checkpoint before-cleanup
Checkpoint 'before-cleanup' set at #2
```

## `back <name>`

Rewind active saved changes after that checkpoint. The checkpoint and the original history stay in the journal. Each restored change is recorded as a new recovery event.

## `to <id>`

Same as `back`, but by entry number from `lifeline log`. `to 0` rewinds all active saved changes.

## `system capture-hostname`

Capture the single system file currently approved for privileged capture: `/etc/hostname`.

```console
$ lifeline system capture-hostname
Authentication is required to capture an approved system file.
Captured /etc/hostname: 7 bytes
Recovery object: 0123456789abcdef0123456789abcdef
```

The service selects the path itself. It requires Polkit administrator authentication, checks the exact allowlist, safely opens the file without following symlinks, stores and verifies an immutable recovery object, and appends metadata to the protected catalog. Repeating the command creates another version.

## `system history`

Show up to the 20 most recent captured versions of `/etc/hostname`. Each row includes the recovery object ID, capture timestamp in Unix epoch nanoseconds, and original size.

```console
$ lifeline system history
System file history: /etc/hostname
0123456789abcdef0123456789abcdef  timestamp=1795260000000000000  size=7 bytes
```

## `system restore <id>`

Restore a previously captured `/etc/hostname` version by its 32-character recovery object ID.

```console
$ lifeline system restore 0123456789abcdef0123456789abcdef
Authentication is required to restore a protected system file.
Restored /etc/hostname from recovery object: 0123456789abcdef0123456789abcdef
Previous hostname saved as recovery object: fedcba9876543210fedcba9876543210
```

Before changing the file, the service captures the current contents as a new recovery object. It validates the selected object's catalog entry and integrity, refuses to follow symlinks or overwrite a file whose inode or metadata no longer matches the captured identity, and writes only to the fixed hostname path. A durable pending transaction record allows the service to restore the pre-change contents if the operation is interrupted; startup resolves that transaction before accepting requests. The file's inode, owner, group, and mode are preserved.

This is an initial system-file recovery adapter, not general system-wide restore. Only `/etc/hostname` is accepted. Package, service, account, and general system-setting changes are not currently reversible.

## `system history`

Show the 20 most recent captured versions of the approved system file, `/etc/hostname`. Each row contains the recovery object ID, capture timestamp (nanoseconds since the Unix epoch), and captured size.

```console
$ lifeline system history
System file history: /etc/hostname
0123456789abcdef0123456789abcdef  timestamp=...  size=12 bytes
```

History is read from the protected system catalog. It does not modify the hostname and does not require administrator authentication.

## `system restore <id>`

Restore one version listed by `lifeline system history`.

```console
$ lifeline system restore 0123456789abcdef0123456789abcdef
Authentication is required to restore a protected system file.
Restored /etc/hostname from recovery object: 0123456789abcdef0123456789abcdef
Previous hostname saved as recovery object: fedcba9876543210fedcba9876543210
```

The service validates the object ID and confirms that the selected object has a catalog entry for exactly `/etc/hostname`. It requires the separate Polkit administrator authorization action. Before changing the file, it captures the current hostname as a new immutable recovery object and writes a durable pending-restore record. It then updates the existing regular file in place, syncs it, and verifies the resulting bytes. Updating in place preserves the file's inode, owner, mode, and hard-link identity; the operation is not an atomic rename. If the service is interrupted during the write, startup uses the durable pre-restore object to recover the previous contents before accepting new requests. If automatic rollback cannot be completed, the pending record is retained and the service refuses to proceed until recovery succeeds.

This is a deliberately narrow first restore adapter. It does not restore arbitrary system files, package state, services, or settings. Keep normal backups; this early-development feature should not be treated as a substitute for system backup software.

## Environment variables

| Variable | Effect |
|---|---|
| `LIFELINE_HOME` | Where the journal and snapshots are stored |
| `LIFELINE_YES` | If set, skip confirmation prompts (use carefully) |

## `system status`

Check that the privileged UndoOS system service is reachable and responding. This command contacts the service through `/run/lifeline/control.sock`; it does not infer service health from the socket file alone.

```console
$ lifeline system status
System service: running
Control protocol: version 1
```

The system-service installer creates the `lifeline` system group and adds the invoking login to it when installed through `sudo`. Log out and back in after installation so the new group membership applies. The socket is owned by `root:lifeline` with mode `0660`, so only root and members of that group can connect.

This is currently a read-only health check. It does not grant the CLI permission to perform privileged changes, and the service does not accept shell commands or arbitrary filesystem operations.
