# renpak

AVIF compression toolchain for Ren'Py games. Shrinks RPA archives by re-encoding images to AVIF — games load them transparently at runtime, no engine patches needed.

## Why

Ren'Py visual novels ship large RPA archives full of WebP/PNG images. AVIF (based on the AV1 video codec) compresses these 3–5x smaller with comparable visual quality. renpak handles the entire pipeline: reads the RPA, re-encodes every image in parallel, writes a new RPA with an embedded name-mapping manifest, and installs a tiny runtime plugin that intercepts Ren'Py's file loading so the game never knows the difference.

## Results

| Game | Original RPA | Compressed | Image ratio | Time |
|------|-------------|------------|-------------|------|
| Agent17 0.25.9 | 2.3 GB | 1.3 GB | 33% | 5 min (16 cores) |
| Eternum 0.9.5 | 11.5 GB | 7.4 GB | 21% | 9 min (16 cores) |

Image ratio = compressed size / original size. Lower is better. Non-image assets (scripts, video, audio) pass through unchanged.

## Install

```bash
# Linux / macOS
curl -fsSL https://renpak.vercel.app/install | bash

# Windows (PowerShell)
irm https://renpak.vercel.app/install.ps1 | iex
```

This downloads a pre-built static binary to `~/.local/bin` (Linux/macOS) or `%USERPROFILE%\.local\bin` (Windows) and adds it to PATH.

### Build from source

Requires Rust toolchain and libavif (with rav1e encoder):

```bash
# Arch Linux
sudo pacman -S libavif

# Ubuntu/Debian
sudo apt install libavif-dev

# macOS
brew install libavif
```

Then:

```bash
git clone https://github.com/NihilDigit/renpak.git
cd renpak
cargo build --release
./install.sh  # symlinks to ~/.local/bin/renpak
```

## Usage

Navigate to a Ren'Py game directory and run:

```bash
cd ~/Games/MyGame-1.0-pc
renpak
```

### TUI workflow

The interactive TUI guides you through the full process:

**1. Analyze** — scans the RPA and lists all image directories as a collapsible tree with file counts and sizes. Navigate with arrow keys, Space to toggle exclusions (UI directories are excluded by default), Enter to expand/collapse. Tab cycles between four blocks:

- **Directories** — tree view of image directories with checkbox toggles
- **Quality** — three presets: High (q75/s6), Medium (q60/s8), Low (q40/s10)
- **Performance** — three tiers: Low, Medium, High (maps to ¼, ½, all CPU cores)
- **Actions** — summary stats, Start and Quit buttons

Mouse clicks work everywhere. The TUI checks available disk space before starting and detects already-compressed RPAs (skips straight to Done screen).

**2. Build** — encodes images in parallel using all available cores. Shows a live progress bar with compression ratio, encoding rate, and ETA.

**3. Done** — displays final stats (timing breakdown, compression ratio, encoding rate). From here you can:
- **Install** — backs up the original RPA, swaps in the compressed one, writes the runtime plugin
- **Launch** — starts the game so you can verify everything works
- **Revert** — restores the original RPA and removes the runtime plugin
- **Delete backup** — cleans up the backup once you're satisfied
- **Quit**

Navigate with Left/Right, activate with Enter. If the build was cancelled, you can resume from where it left off (cached frames are reused).

### Headless mode

For scripting or CI:

```bash
renpak build input.rpa output.rpa [options]
```

Options:
- `-q, --quality <N>` — AVIF quality 0–63 (default: 60, lower = larger + sharper)
- `-s, --speed <N>` — Encoder speed 0–10 (default: 8, higher = faster)
- `-w, --workers <N>` — Worker threads (default: all cores)
- `-x, --exclude <prefix>` — Skip files matching prefix (repeatable)

## How it works

### Build phase

1. Reads the RPA-3.0 archive index (zlib-compressed pickle dict at the end of the file)
2. Classifies entries: images (.webp, .png, .jpg) are candidates for AVIF encoding; everything else passes through
3. Decodes each image to RGBA, encodes to AVIF using libavif (rav1e encoder, YUV444, full range)
4. Writes a new RPA with AVIF-encoded images renamed (e.g., `foo.webp` → `foo.webp._renpak_avif`) and a JSON manifest mapping original names to new names
5. Encoding runs in parallel via Rayon with configurable worker count, streaming writes through a `Mutex<RpaWriter>`
6. Encoded frames are cached to disk — re-running a build skips already-encoded images

### Runtime phase

Two small files are deployed to the game's `game/` directory:

- `renpak_init.rpy` — runs at `init -999` to bootstrap the loader before any game code
- `renpak_loader.py` — installs three Ren'Py hooks:
  - `file_open_callback` — when the game requests `foo.webp`, checks the manifest (case-insensitive), opens `foo.webp._renpak_avif` instead
  - `loadable_callback` — tells Ren'Py that original filenames are still "loadable" so declarations don't break
  - `load_image` monkey-patch — fixes the file extension hint passed to SDL2_image so it correctly decodes AVIF data

No engine source code is modified. The hooks are standard Ren'Py extension points.

## Project structure

```
crates/renpak-core/        Build engine, CLI, and TUI
  src/lib.rs                 libavif FFI bindings, AVIF/AVIS encoding
  src/rpa.rs                 RPA-3.0 reader/writer (pickle index, XOR key)
  src/pipeline.rs            Parallel encoding pipeline (Rayon)
  src/tui.rs                 Interactive TUI (ratatui + crossterm)
  src/main.rs                CLI entry: TUI or headless mode
  build.rs                   pkg-config / AVIF_PREFIX linking

python/runtime/            Ren'Py runtime plugin (deployed to game)
  renpak_init.rpy            init -999 bootstrap
  renpak_loader.py           File interception + AVIF transparent loading

web/                       Vercel-hosted install scripts
  install.sh                 Linux/macOS installer
  install.ps1                Windows PowerShell installer

.github/workflows/         CI
  release.yml                Static builds for Linux/macOS/Windows on tag push
```

## License

[MPL-2.0](LICENSE)
