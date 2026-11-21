#!/usr/bin/env sh
# Run the same core checks locally without relying on hosted CI runners.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install the stable Rust toolchain first: https://rustup.rs" >&2
    exit 1
fi

echo "[1/3] Formatting Rust source..."
cargo fmt --all
echo "Rust formatting complete."

echo "[2/3] Building release binaries..."
cargo build --release

echo "[3/3] Running tests..."
cargo test

echo "All local checks passed."
