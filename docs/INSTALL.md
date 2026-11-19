# Installing UndoOS

UndoOS is built from source with Rust. There are no prebuilt packages yet.

## Requirements

- Linux
- A recent stable Rust toolchain (<https://rustup.rs>)
- systemd user services for automatic background startup (optional)

## Quick install

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

The installer:

1. Builds the Rust binaries.
2. Installs `lifeline` and `lifeline-daemon` to `~/.local/bin`.
3. Creates `~/.config/lifeline/config.toml` from the bundled defaults, but never overwrites an existing config.
4. Installs and enables the user-level systemd service when systemd is available.

The bundled configuration watches `~/Documents`. Edit `~/.config/lifeline/config.toml` to add or remove folders. Lifeline only watches folders listed there.

Make sure `~/.local/bin` is on your `PATH`.

## Run local checks

GitHub-hosted CI is optional. To check formatting, compile the release binaries, and run the test suite on your own Linux machine:

```sh
make check
```

The same checks can be run directly with `sh scripts/check.sh` from the repository root.

## Check it works

```sh
lifeline --version
systemctl --user status lifeline-daemon
```

If automatic service setup was unavailable, start it manually after installation:

```sh
systemctl --user enable --now lifeline-daemon.service
```

To see the daemon's recent messages:

```sh
journalctl --user -u lifeline-daemon -f
```

## Install somewhere else

```sh
PREFIX=/usr/local sudo -E ./scripts/install.sh
```

A custom prefix installs the binaries but skips automatic user-service setup because the bundled service points to `~/.local/bin`. Configure a service manually if you use a custom prefix.

## With make

```sh
make build
sudo make install        # installs to /usr/local/bin
```

The Makefile path installs binaries only; it does not configure the user service.

## Where your data lives

Lifeline stores its notebook, baselines, and saved copies in `~/.local/share/lifeline`. Set `LIFELINE_HOME` to use another folder.

## Uninstall

```sh
./scripts/uninstall.sh            # stops service, removes programs, keeps saved data
./scripts/uninstall.sh --purge    # also deletes saved data
```

The uninstall script removes the default user service and binaries. It leaves your configuration and saved recovery data in place unless you explicitly use `--purge` for the saved data.
