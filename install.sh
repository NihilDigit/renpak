#!/usr/bin/env bash
# Install renpak CLI to ~/.local/bin
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="${1:-$HOME/.local/bin}"

echo "Building renpak..."
(cd "$SCRIPT_DIR" && cargo build --release 2>&1)

mkdir -p "$INSTALL_DIR"

# Symlink so updates just need cargo build
ln -sf "$SCRIPT_DIR/target/release/renpak" "$INSTALL_DIR/renpak"

echo ""
echo "Installed: $INSTALL_DIR/renpak -> $SCRIPT_DIR/target/release/renpak"
echo ""
echo "Usage:"
echo "  renpak /path/to/game    Interactive TUI"
echo "  renpak build in.rpa out.rpa [options]   Headless"
