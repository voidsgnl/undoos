#!/usr/bin/env sh
# Remove the optional UndoOS system-service scaffold without touching recovery data.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this script with sudo: sudo ./scripts/uninstall-system-service.sh" >&2
    exit 1
fi

systemctl disable --now lifeline-system.socket 2>/dev/null || true
systemctl disable --now lifeline-system-observer.service 2>/dev/null || true
systemctl stop lifeline-system.service 2>/dev/null || true

rm -f /etc/systemd/system/lifeline-system.socket
rm -f /etc/systemd/system/lifeline-system.service
rm -f /etc/systemd/system/lifeline-system-observer.service
rm -f /usr/local/libexec/lifeline-systemd
rm -f /usr/local/libexec/lifeline-system-observer

systemctl daemon-reload
systemctl reset-failed lifeline-system.socket lifeline-system.service lifeline-system-observer.service 2>/dev/null || true

echo "Removed the UndoOS system service and automatic observer. Recovery data was not changed."
