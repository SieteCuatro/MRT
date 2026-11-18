#!/bin/bash
# Builds the Linux release archive against glibc 2.17, so one binary runs on
# every x86_64 distribution from 2014 on (RHEL/CentOS 7, Ubuntu 14.04, Debian 8)
# up to the newest. glibc is backward compatible: a program linked against an
# old version runs on newer ones, not the other way round.
#
# Needs Zig and cargo-zigbuild on PATH:
#   https://ziglang.org/download/ (unpack the tarball, add it to PATH)
#   cargo install cargo-zigbuild --locked
#
# Usage: scripts/release/build-linux.sh v0.2.2   (writes mrt-v0.2.2-x86_64-unknown-linux-gnu.tar.gz)
set -euo pipefail

TAG="${1:?usage: build-linux.sh <tag>}"
GLIBC="2.17"
TARGET="x86_64-unknown-linux-gnu"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

command -v zig >/dev/null || { echo "zig is not on PATH" >&2; exit 1; }
command -v cargo-zigbuild >/dev/null || { echo "cargo-zigbuild is not on PATH" >&2; exit 1; }

cargo zigbuild --release --locked -p mrt-app --target "$TARGET.$GLIBC"

BIN="${CARGO_TARGET_DIR:-$ROOT/target}/$TARGET/release/mrt"

# The newest glibc symbol version the binary asks for must not exceed $GLIBC.
NEWEST="$(objdump -T "$BIN" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -Vu | tail -1)"
if [ "$(printf '%s\n%s\n' "$NEWEST" "$GLIBC" | sort -V | tail -1)" != "$GLIBC" ]; then
    echo "the binary needs glibc $NEWEST, more than $GLIBC" >&2
    exit 1
fi
echo "newest glibc symbol: $NEWEST"

# The binary, plus the desktop entry and its icon for a menu entry.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp "$BIN" "$STAGE/mrt"
cp "$ROOT/packaging/linux/mrt.desktop" "$ROOT/packaging/linux/mrt.png" "$STAGE/"
ARCHIVE="$ROOT/mrt-$TAG-$TARGET.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGE" mrt mrt.desktop mrt.png
echo "wrote $ARCHIVE"
