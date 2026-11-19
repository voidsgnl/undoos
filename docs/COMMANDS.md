# Commands

Every command is `lifeline <command>`.

## `snap <file>`

Remember a file exactly as it is right now.

```console
$ lifeline snap ~/thesis.odt
Saved #1: /home/you/thesis.odt
```

Take a snapshot *before* you change or delete something. Automatic snapshots will come with the background daemon.

## `log`

Show everything lifeline remembers, oldest first.

```console
$ lifeline log
#1       5m ago  snap  /home/you/thesis.odt
#2       2m ago  checkpoint  before-cleanup
```

## `undo`

Put back the most recently saved file. Lifeline asks first if that would overwrite a file that exists right now.

```console
$ lifeline undo
Hold on. Restore /home/you/thesis.odt to its saved version? The current version will be overwritten. [y/N] y
Restored /home/you/thesis.odt
```

## `checkpoint <name>`

Drop a named marker in the timeline, like a save point in a game.

```console
$ lifeline checkpoint before-cleanup
Checkpoint 'before-cleanup' set at #2
```

## `back <name>`

Rewind everything saved after that checkpoint. The checkpoint itself stays.

```console
$ lifeline back before-cleanup
```

## `to <id>`

Same as `back`, but by entry number from `lifeline log`.

```console
$ lifeline to 7
```

`to 0` rewinds everything.

## Environment variables

| Variable | Effect |
|---|---|
| `LIFELINE_HOME` | Where the journal and snapshots are stored |
| `LIFELINE_YES` | If set, skip the "Hold on" confirmation prompts |
