#!/usr/bin/env sh
# Install the opt-in, root-owned UndoOS system-service scaffold.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this script with sudo: sudo ./scripts/install-system-service.sh" >&2
    exit 1
fi

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
BINARY="$ROOT/target/release/lifeline-systemd"
UNIT_DIR="/etc/systemd/system"

if [ ! -x "$BINARY" ]; then
    echo "Missing $BINARY. Build it first with: cargo build --release" >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "systemctl is required to install the system service." >&2
    exit 1
fi

install -D -o root -g root -m 0755 "$BINARY" /usr/local/libexec/lifeline-systemd
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline-system.socket" "$UNIT_DIR/lifeline-system.socket"
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline-system.service" "$UNIT_DIR/lifeline-system.service"

systemctl daemon-reload
systemctl enable --now lifeline-system.socket

echo "Installed the root-only UndoOS system-service scaffold."
echo "The socket is restricted to root (0600); no system changes can be requested yet."
echo "Check it with: systemctl status lifeline-system.socket"
