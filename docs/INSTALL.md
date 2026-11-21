# Installing UndoOS

UndoOS (the `lifeline` command) is currently installed from source. **There is no apt package or prebuilt release package yet.**

## Requirements

- Linux (Ubuntu is a supported development target).
- A recent stable Rust toolchain, including `cargo`.
- `systemd --user` for automatic background startup (optional).

Install Rust using the official instructions: <https://rustup.rs>. After installation, open a new terminal and check:

```sh
rustc --version
cargo --version
```

## Install from GitHub

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

The script builds the release binaries and installs:

- `lifeline`
- `lifeline-daemon`

By default, both are placed in `~/.local/bin`. The script also creates `~/.config/lifeline/config.toml` if it does not already exist. It does not overwrite your existing configuration.

If the command is not found, add the install directory to your PATH:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

For zsh, use `~/.zshrc` instead of `~/.bashrc`.

Check the installation:

```sh
lifeline --version
command -v lifeline
```

## Background monitoring

For the default installation under `~/.local`, the installer attempts to enable the user-level systemd service.

Check it:

```sh
systemctl --user status lifeline-daemon.service
```

If it is not running, try:

```sh
systemctl --user enable --now lifeline-daemon.service
```

View its recent log messages:

```sh
journalctl --user -u lifeline-daemon.service -n 50 --no-pager
```

The default config watches `~/Documents`. Lifeline only watches folders listed in `~/.config/lifeline/config.toml`; it does not monitor your entire computer. Review and edit that file before relying on background monitoring.

## Build and install to a custom location

To build without installing:

```sh
cargo build --release
```

To install under `/usr/local/bin`:

```sh
make build
sudo make install
```

The Makefile installation installs binaries only. It does not create the user configuration or enable the systemd service.

You can also use a custom prefix with the install script:

```sh
PREFIX="$HOME/.local/opt/lifeline" ./scripts/install.sh
```

Add that prefix's `bin` directory to your PATH. Automatic systemd setup is only attempted for the default `~/.local` prefix because the shipped service file expects that location.

## Optional privileged system-service foundation

UndoOS includes an **early, opt-in system service**. It is separate from the user-level file watcher and is not required for ordinary file recovery.

Build and install it:

```sh
make system-install
```

The installer creates a system group named `lifeline`, adds the invoking login to that group, installs the root-owned service, and enables its socket. The socket is owned by `root:lifeline` with mode `0660`. A systemd-tmpfiles rule creates `/run/lifeline` as `root:root` with mode `0711` on boot and applies the same permissions during installation. The parent directory permits path traversal, while the socket permissions restrict who can connect. The same rule prepares `/var/lib/lifeline/system` as a root-owned `0700` directory for system recovery evidence. Before accepting requests, the system service verifies that `/var`, `/var/lib`, and both UndoOS-managed directories are real directories owned by root, that no path component is a symbolic link, and that the managed directories have mode `0700`. It initializes `/var/lib/lifeline/system/objects` with mode `0700`. The storage engine can preserve immutable, randomly identified objects with mode `0600` and verify their length and CRC-32 checksum when read. Its append-only catalog records original paths and file metadata with checksummed records, serialized durable appends, and interrupted-tail recovery. The service validates the catalog at startup. The service exposes fixed-path capture, history, and restore operations for `/etc/hostname`; it does not accept arbitrary paths. A separate root-owned observer service watches the `/etc` directory and reacts only to completed writes or atomic replacements of the exact `hostname` entry. It establishes an initial baseline and stores a new immutable content version when the file's bytes change. It has read-only access to `/etc` and write access only to `/var/lib/lifeline/system`. If the storage path is missing or has unsafe ownership or permissions, the service refuses to start.

After installation, **log out and back in** so your login receives the new group membership. Then check the socket:

```sh
systemctl status lifeline-system.socket
```

Check communication with the actual service:

```sh
lifeline system status
```

Expected output:

```text
System service: running
Control protocol: version 1
```

This command sends a fixed, versioned, read-only status request over the Unix socket. The first request starts the service through systemd socket activation. The service accepts health checks, one narrowly scoped capture request (`lifeline system capture-hostname`), a read-only hostname history request (`lifeline system history`), and an administrator-authorized restore request (`lifeline system restore <id>`). That request requires Polkit administrator authentication and captures only the exact allowlisted `/etc/hostname` file; the CLI cannot supply a path. It stores and verifies an immutable recovery object and appends its metadata to the protected catalog. This is a narrow system-file recovery adapter, not general system-file monitoring or arbitrary-path restore. The service can list the latest 20 hostname captures with `lifeline system history` and restore one with `lifeline system restore <id>`. Restore requires the separate Polkit administrator action. Before writing, it captures the current hostname as a rollback object, checks the selected catalog record and object integrity, and refuses symlinks or changed inode/metadata. It writes the existing file in place to preserve ownership, mode, and inode, and uses a durable pending transaction record to roll back an interrupted write during the next service startup. The systemd sandbox is read-only except for the dedicated recovery store and the exact `/etc/hostname` file; it does not grant write access to the rest of `/etc`. This restores the persistent hostname configuration file; it does not immediately change the running kernel hostname. It does not accept shell commands or arbitrary filesystem paths. The installer installs separate Polkit actions for manual system-file capture and restore. The background observer does not prompt for each change because it runs as a dedicated root-owned system service. It records content changes only: deletion, metadata-only changes, and intermediate states lost to event coalescing or inotify queue overflow are not captured. No other system-file restore operation is available.

Inspect service messages with:

```sh
sudo journalctl -u lifeline-system.service -n 30 --no-pager
```

Inspect automatic observer status and messages with:

```sh
systemctl status lifeline-system-observer.service
sudo journalctl -u lifeline-system-observer.service -n 30 --no-pager
```

Remove this optional service with:

```sh
make system-uninstall
```

Uninstallation disables and removes both privileged systemd services, their binaries, and the control socket unit. It leaves the `lifeline` group in place and does not remove the user daemon, configuration, journal, or recovery copies.

## Where Lifeline stores data

- Configuration: `~/.config/lifeline/config.toml`
- Journal, baselines, and snapshots: `~/.local/share/lifeline`

The data location can be changed for an individual command or test by setting `LIFELINE_HOME`.

## Uninstall

Remove the programs and stop the default user service while keeping saved recovery data:

```sh
./scripts/uninstall.sh
```

To also permanently delete Lifeline's saved recovery data:

```sh
./scripts/uninstall.sh --purge
```

**Warning:** `--purge` deletes Lifeline's saved copies and journal. Use it only when you are certain you no longer need them.

## Before relying on it

UndoOS is early-development software. It currently tracks regular files inside configured folders and stores full copies. Package installs/removals, system settings, and many other system-wide changes are not supported yet. Keep normal backups of important data.

See [TESTING.md](TESTING.md) for safe manual checks and the automated test suite.
