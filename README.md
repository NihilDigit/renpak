# renpak

AVIF compression toolchain for Ren'Py games. Shrinks RPA archives by re-encoding images to AVIF — games load them transparently at runtime, no engine patches needed.

<!-- TODO: TUI demo GIF -->

## Why bother

Ren'Py visual novels ship massive RPA archives full of WebP and PNG images. AVIF, built on the AV1 video codec, compresses them 3–5x smaller at comparable visual quality.

renpak handles the whole pipeline: crack open the RPA, re-encode every image in parallel, write a new archive with an embedded manifest, and drop in a tiny runtime plugin that hooks Ren'Py's file loading. The game never knows the difference.

## Results

<!-- TODO: comparison images, LPIPS/SSIM metrics, compression benchmarks across multiple games -->

## Install

```bash
# Linux / macOS
curl -fsSL https://renpak.vercel.app/install | bash

# Windows (PowerShell)
irm https://renpak.vercel.app/install.ps1 | iex
```

Downloads a static binary to `~/.local/bin` (Unix) or `%USERPROFILE%\.local\bin` (Windows).

### Build from source

Needs a Rust toolchain and system libavif:

```bash
# Arch
sudo pacman -S libavif

# Ubuntu / Debian
sudo apt install libavif-dev

# macOS
brew install libavif
```

```bash
git clone https://github.com/NihilDigit/renpak.git
cd renpak
cargo build --release
./install.sh  # symlinks to ~/.local/bin/renpak
```

## Usage

Point it at a game directory:

```bash
renpak ~/Games/MyGame-1.0-pc
```

Or just `cd` in and run `renpak` with no arguments.

The TUI walks you through everything — pick directories to compress, choose a quality preset, hit Start. When it's done, Install swaps in the compressed RPA and drops the runtime plugin into `game/`. Launch the game to verify, Revert if anything looks off.

Quality presets:

| Preset | Quality | Speed | Use case |
|--------|---------|-------|----------|
| High   | 75      | 6     | Archival, picky about artifacts |
| Medium | 60      | 8     | Default — good balance |
| Low    | 40      | 10    | Maximum compression, fast |

### Headless mode

```bash
renpak build input.rpa output.rpa [options]
```

| Flag | Description |
|------|-------------|
| `-p, --preset` | `high`, `medium`, or `low` |
| `-q, --quality` | AVIF quality 0–100 (overrides preset) |
| `-s, --speed` | Encoder speed 0–10 (overrides preset) |
| `-w, --workers` | Thread count (default: all cores) |
| `-x, --exclude` | Skip files matching prefix (repeatable) |

## How it works

**Build phase.** Reads the RPA-3.0 index, decodes each image to RGBA, encodes to AVIF via libaom (YUV444, full range, BT.709 color). Writes a new RPA with renamed entries and a JSON manifest. Encoding is parallelized with Rayon; already-encoded frames are cached to disk so re-runs skip them.

**Runtime phase.** Two files go into `game/`:

- `renpak_init.rpy` — bootstraps at `init -999`, before any game code
- `renpak_loader.py` — hooks `file_open_callback` (name remapping), `loadable_callback` (keeps declarations working), and monkey-patches `load_image` (fixes SDL2_image extension hint for AVIF)

No engine modifications. Standard Ren'Py extension points only.

## Project layout

```
crates/renpak-core/     Build engine: RPA I/O, AVIF encoding, TUI, CLI
crates/renpak-rt/       Runtime decoder: AVIS frame-level random access (C ABI)
python/runtime/         Ren'Py plugin (deployed to game/)
web/                    Install scripts (Vercel-hosted)
```

## License

[MPL-2.0](LICENSE)
