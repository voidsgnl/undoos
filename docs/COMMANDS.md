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

Restore the most recent saved state that has not already been undone. Lifeline asks before changing the file. The original snapshot and journal entry remain in the history, and a new recovery entry is appended.

```console
$ lifeline undo
Restore /home/you/thesis.odt to its saved version? [y/N] y
Restored /home/you/thesis.odt. Original history kept.
```

## `redo`

Reverse the most recent undo. This reapplies the state that existed immediately before that undo.

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

## Environment variables

| Variable | Effect |
|---|---|
| `LIFELINE_HOME` | Where the journal and snapshots are stored |
| `LIFELINE_YES` | If set, skip confirmation prompts (use carefully) |
