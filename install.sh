#!/usr/bin/env bash
# renpak install — one-click compress + deploy for Ren'Py games
# Usage: ./install.sh /path/to/game/root [-q quality] [-s speed] [-j workers] [-x prefix]...
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RENPAK_BIN="$SCRIPT_DIR/target/release/renpak"
RUNTIME_DIR="$SCRIPT_DIR/python/runtime"

# --- Parse args ---
if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <game_root> [options]"
    echo ""
    echo "  game_root   Directory containing game/ with .rpa files"
    echo ""
    echo "Options:"
    echo "  -q N        AVIF quality 1-63 (default: 60, lower=bigger+sharper)"
    echo "  -s N        Encoding speed 0-10 (default: 8, higher=faster)"
    echo "  -j N        Worker threads (default: auto)"
    echo "  -x PREFIX   Exclude files starting with PREFIX (repeatable)"
    echo "              gui/ is always excluded"
    echo ""
    echo "Example:"
    echo "  $0 ~/Games/Agent17-0.25.9-pc"
    echo "  $0 ~/Games/Agent17-0.25.9-pc -q 50 -x images/ui/"
    exit 1
fi

GAME_ROOT="$(realpath "$1")"
shift
EXTRA_ARGS=("$@")

GAME_DIR="$GAME_ROOT/game"
if [[ ! -d "$GAME_DIR" ]]; then
    echo "ERROR: $GAME_DIR not found"
    exit 1
fi

# Find .rpa files
RPA_FILES=()
while IFS= read -r f; do
    RPA_FILES+=("$f")
done < <(find "$GAME_DIR" -maxdepth 1 -name '*.rpa' -type f | sort)

if [[ ${#RPA_FILES[@]} -eq 0 ]]; then
    echo "ERROR: No .rpa files in $GAME_DIR"
    exit 1
fi

echo "renpak install"
echo "  game:    $GAME_ROOT"
echo "  rpa:     ${RPA_FILES[*]}"
echo ""

# --- Step 1: Build renpak if needed ---
if [[ ! -x "$RENPAK_BIN" ]]; then
    echo "=== Building renpak ==="
    (cd "$SCRIPT_DIR" && cargo build --release 2>&1)
    echo ""
fi

# --- Step 2: Compress each RPA ---
WORK_DIR="$GAME_DIR/.renpak_work"
mkdir -p "$WORK_DIR"

for rpa in "${RPA_FILES[@]}"; do
    rpa_name="$(basename "$rpa")"
    out_rpa="$WORK_DIR/$rpa_name"

    echo "=== Compressing $rpa_name ==="
    "$RENPAK_BIN" "$rpa" "$out_rpa" "${EXTRA_ARGS[@]}"
    echo ""
done

# --- Step 3: Backup originals + swap ---
BACKUP_DIR="$GAME_DIR/.renpak_backup"
mkdir -p "$BACKUP_DIR"

for rpa in "${RPA_FILES[@]}"; do
    rpa_name="$(basename "$rpa")"
    backup="$BACKUP_DIR/$rpa_name"
    compressed="$WORK_DIR/$rpa_name"

    if [[ ! -f "$backup" ]]; then
        echo "Backup: $rpa_name -> .renpak_backup/"
        mv "$rpa" "$backup"
    else
        echo "Backup exists, removing old compressed RPA"
        rm -f "$rpa"
    fi

    mv "$compressed" "$rpa"
    echo "Installed: $rpa_name ($(du -h "$rpa" | cut -f1))"
done

rmdir "$WORK_DIR" 2>/dev/null || true

# --- Step 4: Install runtime plugin ---
echo ""
echo "=== Installing runtime plugin ==="

for f in renpak_init.rpy renpak_loader.py; do
    src="$RUNTIME_DIR/$f"
    dst="$GAME_DIR/$f"
    if [[ -f "$src" ]]; then
        cp "$src" "$dst"
        echo "  $f"
    else
        echo "  WARNING: $src not found"
    fi
done

# --- Summary ---
echo ""
orig_size=0
comp_size=0
for rpa in "${RPA_FILES[@]}"; do
    rpa_name="$(basename "$rpa")"
    backup="$BACKUP_DIR/$rpa_name"
    if [[ -f "$backup" ]]; then
        orig_size=$((orig_size + $(stat -c%s "$backup")))
    fi
    comp_size=$((comp_size + $(stat -c%s "$rpa")))
done
orig_mb=$((orig_size / 1048576))
comp_mb=$((comp_size / 1048576))
echo "Done."
echo "  ${orig_mb}MB -> ${comp_mb}MB"
echo "  Originals backed up to: $BACKUP_DIR"
echo "  To revert: cd $GAME_DIR && mv .renpak_backup/*.rpa . && rm -f renpak_init.rpy renpak_loader.py"
