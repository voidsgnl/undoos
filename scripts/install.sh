#!/usr/bin/env sh
# Build and install lifeline. Default prefix is ~/.local; override with PREFIX.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
CONFIG_DIR="$HOME/.config/lifeline"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install Rust first: https://rustup.rs" >&2
    exit 1
fi

cd "$(dirname "$0")/.."
cargo build --release

mkdir -p "$PREFIX/bin"
install -m 755 target/release/lifeline "$PREFIX/bin/lifeline"
install -m 755 target/release/lifeline-daemon "$PREFIX/bin/lifeline-daemon"

echo "Installed lifeline to $PREFIX/bin"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not on your PATH yet." ;;
esac

# Preserve any configuration the user has already customized.
mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/config.toml" ]; then
    install -m 644 lifeline/config.toml "$CONFIG_DIR/config.toml"
    echo "Created default configuration at $CONFIG_DIR/config.toml"
else
    echo "Keeping existing configuration at $CONFIG_DIR/config.toml"
fi

# The shipped unit uses %h/.local/bin, so only enable it for the default
# per-user installation. Custom prefixes can still run the daemon manually.
if [ "$PREFIX" = "$HOME/.local" ] && command -v systemctl >/dev/null 2>&1; then
    UNIT_DIR="$HOME/.config/systemd/user"
    mkdir -p "$UNIT_DIR"
    install -m 644 scripts/lifeline-daemon.service "$UNIT_DIR/lifeline-daemon.service"
    if systemctl --user daemon-reload && systemctl --user enable --now lifeline-daemon.service; then
        echo "Lifeline is now watching your configured folders in the background."
    else
        echo "Could not start the user service automatically." >&2
        echo "Start it later with: systemctl --user enable --now lifeline-daemon.service" >&2
    fi
else
    echo "Automatic user-service setup skipped (use the daemon manually or configure systemd)." 
fi
