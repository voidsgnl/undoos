# Testing Lifeline

This guide explains how to check Lifeline on your own Linux machine without touching your real Documents folder or recovery history.

## 1. Run the automated test suite

Install the Rust toolchain first if needed: <https://rustup.rs>.

From the repository root:

```sh
cargo test
```

This runs the integration and unit tests. The integration tests launch the real `lifeline` and `lifeline-daemon` binaries and use temporary directories plus a separate `LIFELINE_HOME`. They should not access your normal Lifeline data.

Run the full local check used by the project:

```sh
make check
```

That checks formatting, builds both release binaries, and runs the tests. You can also run the same sequence with:

```sh
sh scripts/check.sh
```

## 2. Try Lifeline manually in a disposable test area

After building with `cargo build --release`, copy and paste this block into a terminal. It creates a temporary folder, a separate Lifeline data directory, and one sample file.

```sh
TEST_ROOT="$(mktemp -d)"
export LIFELINE_HOME="$TEST_ROOT/lifeline-data"
TEST_FILE="$TEST_ROOT/hello.txt"

printf 'first version\n' > "$TEST_FILE"
./target/release/lifeline snap "$TEST_FILE"

printf 'second version\n' > "$TEST_FILE"
./target/release/lifeline undo
cat "$TEST_FILE"

./target/release/lifeline redo
cat "$TEST_FILE"

./target/release/lifeline log
```

Expected behavior:
- The snapshot saves the first version.
- Undo restores the first version.
- Redo reapplies the second version.
- The log shows the saved state and recovery actions.

The test uses a separate `LIFELINE_HOME`; it does not use the data in `~/.local/share/lifeline`.

When finished, remove only the temporary test directory:

```sh
rm -rf "$TEST_ROOT"
unset LIFELINE_HOME
```

## 3. Test the folder watcher separately

Create a temporary watched folder and config:

```sh
TEST_ROOT="$(mktemp -d)"
export LIFELINE_HOME="$TEST_ROOT/lifeline-data"
mkdir -p "$TEST_ROOT/watched"
printf 'version one\n' > "$TEST_ROOT/watched/notes.txt"

cat > "$TEST_ROOT/config.toml" <<EOF
[watch]
paths = ["$TEST_ROOT/watched"]
settle_ms = 0

[ignore]
patterns = []
EOF
```

Run one scan to establish the initial baseline:

```sh
./target/release/lifeline-daemon --config "$TEST_ROOT/config.toml" --once
```

Change the file and scan again:

```sh
printf 'version two\n' > "$TEST_ROOT/watched/notes.txt"
./target/release/lifeline-daemon --config "$TEST_ROOT/config.toml" --once
./target/release/lifeline log
```

If the change was detected and saved, undo it:

```sh
./target/release/lifeline undo
cat "$TEST_ROOT/watched/notes.txt"
```

Clean up when finished:

```sh
rm -rf "$TEST_ROOT"
unset LIFELINE_HOME
```

## 4. What the current recovery tests cover

The automated suite includes checks for:

- Restoring deleted files and deleted directory trees.
- Undo and redo of saved file changes.
- Preserving history rather than deleting old journal entries.
- Refusing to overwrite edits made after Lifeline's last observed state.
- Atomic file replacement and preservation of file permissions.
- Detecting file renames, including after a daemon restart, where filesystem identity information is available.
- Avoiding false deletion reports when a watched directory cannot be inspected.
- Reconciling interrupted file and tracked-move transactions from their durable journal intent.

### Real process-crash tests

The `test_process_crash` integration test starts real Lifeline child processes, waits until they reach explicit transaction boundaries, kills them, then starts a fresh Lifeline process to verify recovery. Run it directly with:

```sh
cargo test --test test_process_crash -- --nocapture
```

It covers ten cases: SIGKILL after the durable intent and after the filesystem operation for file undo/redo and tracked-move undo/redo, plus two cases where an unrelated file replaces a move destination before reconciliation. These are genuine process-termination tests, not simulated journal states. The pause hooks are inert in release builds. They do not simulate sudden power loss, storage-controller failure, or every filesystem-specific durability behavior; those require separate testing on real filesystems and hardware.

Directory moves are currently tracked as individual moves for files inside the directory. Empty directories are not independently journaled, and directory-wide recovery is not yet one atomic transaction. Do not use Lifeline as the only copy of important data.

## 5. Report a test failure

Run the failing test with output enabled:

```sh
cargo test test_name -- --nocapture
```

Replace `test_name` with the name shown in the failure. Include the complete error output, operating system, and filesystem type when reporting a bug.
