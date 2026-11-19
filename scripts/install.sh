#!/usr/bin/env sh
# Build and install lifeline. Default prefix is ~/.local; override with PREFIX.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"

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
