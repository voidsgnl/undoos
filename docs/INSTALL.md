# Installing UndoOS

UndoOS is built from source with Rust. There are no prebuilt packages yet.

## Requirements

- Linux
- A recent stable Rust toolchain (<https://rustup.rs>)

## Quick install

```sh
git clone https://github.com/voidsgnl/undoos.git
cd undoos
./scripts/install.sh
```

This builds a release binary and installs `lifeline` to `~/.local/bin`. Make sure that folder is on your `PATH`.

To install somewhere else:

```sh
PREFIX=/usr/local sudo -E ./scripts/install.sh
```

## With make

```sh
make build
sudo make install        # installs to /usr/local/bin
```

## Check it works

```sh
lifeline --version
```

## Where your data lives

Lifeline stores its notebook and saved copies in `~/.local/share/lifeline`. Set `LIFELINE_HOME` to use another folder.

## Uninstall

```sh
./scripts/uninstall.sh            # removes the programs, keeps your saved data
./scripts/uninstall.sh --purge    # also deletes saved data
```
