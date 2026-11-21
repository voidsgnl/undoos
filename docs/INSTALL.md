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

The installer creates a system group named `lifeline`, adds the invoking login to that group, installs the root-owned service, and enables its socket. The socket is owned by `root:lifeline` with mode `0660`. A systemd-tmpfiles rule creates `/run/lifeline` as `root:root` with mode `0711` on boot and applies the same permissions during installation. The parent directory permits path traversal, while the socket permissions restrict who can connect. The same rule prepares `/var/lib/lifeline/system` as a root-owned `0700` directory for future system recovery evidence. Before accepting requests, the system service verifies that `/var`, `/var/lib`, and both UndoOS-managed directories are real directories owned by root, that no path component is a symbolic link, and that the managed directories have mode `0700`. It initializes `/var/lib/lifeline/system/objects` with mode `0700`. The storage engine can preserve immutable, randomly identified objects with mode `0600` and verify their length and CRC-32 checksum when read. Its append-only catalog records original paths and file metadata with checksummed records, serialized durable appends, and interrupted-tail recovery. The service validates the catalog at startup. These APIs are not yet exposed through the service protocol or connected to a system-file observer, so they do not currently capture system recovery evidence. If the storage path is missing or has unsafe ownership or permissions, the service refuses to start.

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

This command sends a fixed, versioned, read-only status request over the Unix socket. The first request starts the service through systemd socket activation. The service currently accepts only health-check requests; it does not monitor system files, change system settings, or perform recovery operations. Its systemd sandbox keeps the filesystem read-only except for the dedicated `/var/lib/lifeline/system` recovery-storage directory. It does not accept shell commands or arbitrary filesystem paths. The system-service installer also installs the Polkit action definition for future protected system-file recovery. The authorization helper is not yet connected to a recovery request, so no privileged recovery operation is available.

Inspect service messages with:

```sh
sudo journalctl -u lifeline-system.service -n 30 --no-pager
```

Remove this optional service with:

```sh
make system-uninstall
```

Uninstallation removes the systemd units and service binary. It leaves the `lifeline` group in place and does not remove the user daemon, configuration, journal, or recovery copies.

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
