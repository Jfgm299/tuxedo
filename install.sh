#!/usr/bin/env bash
# install.sh — build this fork in release mode and install it on your PATH.
#
# This is independent of the Homebrew-managed `tuxedo` (if you have one
# installed via `brew install tuxedo`) — it never touches that binary or its
# symlink. Point $INSTALL_DIR elsewhere if you don't want ~/.local/bin.
set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BIN_NAME="tuxedo"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "Building $BIN_NAME in release mode..."
cargo build --release

# Respect a custom build output location (CARGO_TARGET_DIR is the common way
# machines/CI set this) instead of assuming ./target — cargo build honors it
# silently, so hardcoding target/release/ here would build fine and then
# fail to find the binary on any machine with it set.
CARGO_OUT_DIR="${CARGO_TARGET_DIR:-target}"
BUILT_BIN="$CARGO_OUT_DIR/release/$BIN_NAME"
if [ ! -f "$BUILT_BIN" ]; then
  echo "error: expected $BUILT_BIN after build, but it's not there." >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
cp "$BUILT_BIN" "$INSTALL_DIR/$BIN_NAME"
chmod +x "$INSTALL_DIR/$BIN_NAME"

echo "Installed to $INSTALL_DIR/$BIN_NAME"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo
    echo "Note: $INSTALL_DIR is not on your PATH."
    echo "Add this to your shell config, then restart your shell:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

echo
echo "Run '$BIN_NAME --version' to confirm, or just '$BIN_NAME' to launch it."
echo "If you also have the Homebrew tuxedo installed, both binaries coexist —"
echo "this script never touches brew's install."
