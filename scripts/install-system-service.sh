#!/usr/bin/env sh
# Install the opt-in, root-owned UndoOS system-service scaffold.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this script with sudo: sudo ./scripts/install-system-service.sh" >&2
    exit 1
fi

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
CLI_BINARY="$ROOT/target/release/lifeline"
BINARY="$ROOT/target/release/lifeline-systemd"
OBSERVER_BINARY="$ROOT/target/release/lifeline-system-observer"
UNIT_DIR="/etc/systemd/system"

if [ ! -x "$CLI_BINARY" ] || [ ! -x "$BINARY" ] || [ ! -x "$OBSERVER_BINARY" ]; then
    echo "Missing an UndoOS binary. Build them first with: cargo build --release" >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "systemctl is required to install the system service." >&2
    exit 1
fi

if [ ! -x /usr/bin/pkcheck ]; then
    echo "Polkit is required for the system authorization foundation. Install the 'polkitd' package first." >&2
    exit 1
fi

if ! getent group lifeline >/dev/null 2>&1; then
    groupadd --system lifeline
fi

INSTALL_USER=${SUDO_USER:-}
if [ -n "$INSTALL_USER" ] && [ "$INSTALL_USER" != "root" ]; then
    if id "$INSTALL_USER" >/dev/null 2>&1; then
        usermod -a -G lifeline "$INSTALL_USER"
    else
        echo "Warning: could not find invoking user '$INSTALL_USER'; add your login to the lifeline group manually." >&2
    fi
else
    echo "Warning: no non-root invoking user detected; add your login to the lifeline group manually." >&2
fi

systemctl stop lifeline-system-observer.service 2>/dev/null || true
install -D -o root -g root -m 0755 "$CLI_BINARY" /usr/local/bin/lifeline
install -D -o root -g root -m 0755 "$BINARY" /usr/local/libexec/lifeline-systemd
install -D -o root -g root -m 0755 "$OBSERVER_BINARY" /usr/local/libexec/lifeline-system-observer
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline-system.socket" "$UNIT_DIR/lifeline-system.socket"
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline-system.service" "$UNIT_DIR/lifeline-system.service"
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline-system-observer.service" "$UNIT_DIR/lifeline-system-observer.service"
install -D -o root -g root -m 0644 "$ROOT/systemd/lifeline.conf" /etc/tmpfiles.d/lifeline.conf
install -D -o root -g root -m 0644 "$ROOT/polkit/org.undoos.policy" /usr/share/polkit-1/actions/org.undoos.policy

systemctl stop lifeline-system.socket lifeline-system.service 2>/dev/null || true
systemctl daemon-reload
systemd-tmpfiles --create /etc/tmpfiles.d/lifeline.conf
systemctl enable --now lifeline-system.socket
systemctl enable --now lifeline-system-observer.service

echo "Installed the UndoOS privileged system service and automatic /etc/hostname observer."
echo "The control socket is accessible to root and members of the lifeline group (0660)."
if [ -n "$INSTALL_USER" ] && [ "$INSTALL_USER" != "root" ]; then
    echo "Log out and back in before using the lifeline system commands."
fi
echo "Check the control socket with: systemctl status lifeline-system.socket"
echo "Check automatic observation with: systemctl status lifeline-system-observer.service"
