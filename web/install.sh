#!/bin/sh
# renpak installer — curl -fsSL https://renpak.vercel.app/install | bash
set -e

REPO="NihilDigit/renpak"
INSTALL_DIR="${RENPAK_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS
case "$(uname -s)" in
  Linux*)  OS="linux" ;;
  Darwin*) OS="macos" ;;
  *)       echo "Unsupported OS: $(uname -s)"; exit 1 ;;
esac

# Detect arch
case "$(uname -m)" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64)  ARCH="aarch64" ;;
  *)              echo "Unsupported arch: $(uname -m)"; exit 1 ;;
esac

# Get latest release tag
LATEST=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep '"tag_name"' | head -1 | cut -d'"' -f4)

if [ -z "$LATEST" ]; then
  echo "Failed to fetch latest release"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$LATEST/renpak-${OS}-${ARCH}.tar.gz"

echo "renpak $LATEST ($OS-$ARCH)"
echo ""

# Download and install
mkdir -p "$INSTALL_DIR"
echo "Downloading $URL"
curl -fsSL "$URL" | tar xz -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/renpak"

echo ""
echo "Installed to $INSTALL_DIR/renpak"

# Check PATH
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo ""
    echo "Add to your PATH:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

echo ""
echo "Run 'renpak' in a Ren'Py game directory to start."
