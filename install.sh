#!/usr/bin/env bash
# install.sh — build this fork in release mode and install it on your PATH.
#
# This is independent of the Homebrew-managed `tuxedo` (if you have one
# installed via `brew install tuxedo`) — it never touches that binary or its
# symlink. Point $INSTALL_DIR elsewhere if you don't want ~/.local/bin.
#
# Installed under a different command name (tuxedo-w-notes, override with
# $INSTALL_NAME) on purpose: on a machine that also has Homebrew's tuxedo
# installed, /opt/homebrew/bin typically comes before ~/.local/bin on PATH,
# so a plain `tuxedo` would keep resolving to the Homebrew build no matter
# where this one is installed. A distinct name sidesteps that entirely,
# rather than relying on the user reordering their PATH (which would affect
# every other same-named tool they have installed via both, not just this).
set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CARGO_BIN_NAME="tuxedo" # fixed by Cargo.toml's package name
BIN_NAME="${INSTALL_NAME:-tuxedo-w-notes}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "Building $CARGO_BIN_NAME in release mode..."
cargo build --release

# Respect a custom build output location (CARGO_TARGET_DIR is the common way
# machines/CI set this) instead of assuming ./target — cargo build honors it
# silently, so hardcoding target/release/ here would build fine and then
# fail to find the binary on any machine with it set.
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

echo
echo "Run '$BIN_NAME --version' to confirm, or just '$BIN_NAME' to launch it."
echo "Installed under its own name ($BIN_NAME), not 'tuxedo' — so it never"
echo "shadows or gets shadowed by a Homebrew-installed 'tuxedo' on PATH."
echo "Override the installed name with \$INSTALL_NAME if you want something else."
