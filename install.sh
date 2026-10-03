#!/usr/bin/env bash
# install.sh — build tasq in release mode and install it on your PATH.
#
# Installs to ~/.local/bin/tasq (override with $INSTALL_DIR and
# $INSTALL_NAME). Your tasks live in ~/.local/share/tasq/tasq.db; the first
# run imports the todo.txt it finds via $TODO_FILE, $TODO_DIR or ./todo.txt,
# and your tuxedo settings are copied from ~/.config/tuxedo.
set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CARGO_BIN_NAME="tasq" # fixed by Cargo.toml's package name
BIN_NAME="${INSTALL_NAME:-tasq}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "Building $CARGO_BIN_NAME in release mode..."
cargo build --release

# Respect a custom build output location (CARGO_TARGET_DIR) instead of
# assuming ./target.
CARGO_OUT_DIR="${CARGO_TARGET_DIR:-target}"
BUILT_BIN="$CARGO_OUT_DIR/release/$CARGO_BIN_NAME"
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

# The fork used to install as tuxedo-w-notes; that copy is now stale.
if [ -f "$INSTALL_DIR/tuxedo-w-notes" ]; then
  echo
  echo "An old build is still at $INSTALL_DIR/tuxedo-w-notes; remove it with:"
  echo "  rm \"$INSTALL_DIR/tuxedo-w-notes\""
fi

echo
echo "Run '$BIN_NAME' to launch it, '$BIN_NAME --help' for commands."
