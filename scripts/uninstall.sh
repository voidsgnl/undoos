#!/usr/bin/env sh
# Remove lifeline. Keeps saved data unless you pass --purge.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"

if [ "$PREFIX" = "$HOME/.local" ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --user disable --now lifeline-daemon.service >/dev/null 2>&1 || true
    rm -f "$HOME/.config/systemd/user/lifeline-daemon.service"
    systemctl --user daemon-reload >/dev/null 2>&1 || true
fi

rm -f "$PREFIX/bin/lifeline" "$PREFIX/bin/lifeline-daemon"
echo "Removed lifeline from $PREFIX/bin"

DATA="${LIFELINE_HOME:-$HOME/.local/share/lifeline}"
if [ "${1:-}" = "--purge" ]; then
    rm -rf "$DATA"
    echo "Deleted saved data in $DATA"
else
    echo "Your saved data is still in $DATA (use --purge to delete it)"
fi
