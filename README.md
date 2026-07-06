# renpak

AVIF compression toolchain for Ren'Py games. Shrinks RPA archives by re-encoding images to AVIF — games load them transparently at runtime, no engine patches needed.

https://github.com/user-attachments/assets/46800098-0db3-4de7-a442-b2dbd0fb5fb1

## Why bother

Ren'Py visual novels ship massive RPA archives full of WebP and PNG images. AVIF, built on the AV1 video codec, compresses them 3–5x smaller at comparable visual quality.

renpak handles the whole pipeline: crack open the RPA, re-encode every image in parallel, write a new archive with an embedded manifest, and drop in a tiny runtime plugin that hooks Ren'Py's file loading. The game never knows the difference.

## Results

Tested on [Eternum](https://caribdis.itch.io/eternum) v0.9.5 (latest public release), Medium preset (quality 60, speed 8):

| | Original | Compressed | |
|---|---|---|---|
| RPA archive | 11.3 GiB | 7.2 GiB | 1.56x smaller |
| Image data | 5.11 GiB | 1.08 GiB | 4.73x smaller |

12,732 images (12,243 JPG + 379 WebP + 110 PNG → AVIF) in 9 min 28 sec on an i7-12650H.

| Metric | Mean | Median | Range | |
|--------|------|--------|-------|-|
| SSIM ↑ | 0.950 | 0.957 | 0.892 – 1.000 | 1.0 = identical, >0.95 visually indistinguishable |
| LPIPS ↓ | 0.071 | 0.065 | 0.000 – 0.180 | 0.0 = identical, <0.1 near-transparent to human eyes |

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

## Quickstart

Open a terminal in the game directory and run:

```bash
renpak
```

1. Select which RPA archives to compress
2. Pick a quality preset (Medium is a good default)
3. Hit Start — renpak encodes all images in parallel

When encoding finishes, hit Install. The original RPA is backed up to `.renpak_backup/`, and the compressed archive takes its place. You'll then see:

- Launch — start the game to verify everything looks right
- Revert — restore the original RPA from backup
- Delete — remove the backup to free disk space (only do this after you've verified the game)
- Quit — exit renpak

## Usage

`cd` into a game directory and run `renpak` with no arguments, or pass a path explicitly.

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

### Experimental VP9 Android path

The v2 MVP path bundles related image frames into VP9/WebM files, re-encodes
video outliers for mobile, and stores a v2 manifest that maps original asset
names to optimized targets. This path is currently CLI-first and intended for
Android packaging experiments. Image bundles currently keep source resolution
and record the `source-resolution` profile; mobile downscale image profiles need
runtime/display handling before they should be enabled.

Build a strip archive that can actually shrink:

```bash
renpak build-vp9 input.rpa output.rpa \
  --limit 64 \
  --bundle-size 8 \
  --strip-optimized \
  --work-dir /mnt/data/renpak-work \
  --android-assets-dir /mnt/data/renpak-android-assets
```

Plan candidate scale before encoding:

```bash
renpak plan-vp9 input.rpa \
  --bundle-size 8 \
  --json
```

`plan-vp9` scans the RPA and estimates selected frame count, original bytes, dimension groups, and planned bundle count before the output-size guard. It uses the same candidate ordering as `build-vp9`, so `--limit` estimates match the frames that the build command will try first. The default fast mode reads image headers only, so it is suitable for full-corpus planning but cannot prove transparency exclusions. Pass `--exact` to decode candidate images and make transparency/decode behavior match `build-vp9`.

Useful flags:

| Flag | Description |
|------|-------------|
| `--limit` | Maximum number of image frames to process. Omit for all candidates. |
| `--video-limit` | Maximum number of video files to re-encode. Omit for all videos. |
| `--no-video` | Disable mobile video re-encoding for this build. |
| `--bundle-size` | Maximum frames per WebM bundle, clamped to 1-32. |
| `--min-width`, `--min-height` | Skip small UI-like images. Defaults are 640x360. |
| `--allow-transparent` | Include transparent images. Transparent images are skipped by default. |
| `--min-savings-percent` | Keep a bundle only if it saves at least this much. Default is 8. |
| `--strip-optimized` | Remove original files accepted into the manifest. This is required for archive size reduction. |
| `--android-assets-dir` | Also write loose `renpak_manifest.json` and `renpak/bundles/*.webm` assets for Android packaging experiments. |
| `--json` | Emit machine-readable `plan-vp9` output. |
| `--exact` | Decode candidate images during `plan-vp9` so transparent images are skipped exactly like `build-vp9`. |

Verify a generated archive before packaging:

```bash
python3 scripts/verify_vp9_archive.py output.rpa \
  --android-assets-dir /mnt/data/renpak-android-assets \
  --expect-stripped
```

The mobile video profile keeps frame rate, caps height at 720p without
upscaling, encodes VP9/WebM with realtime settings (`crf=38`, `cpu-used=8`,
`row-mt=1`, `pix_fmt=yuv420p`), copies audio, and keeps the original video when
the output fails the same savings guard used for image bundles.

Prepare a Ren'Py project directory for Android packaging:

```bash
python3 scripts/prepare_android_game.py /path/to/project \
  --archive output.rpa \
  --archive-name archive_0.09.05.rpa \
  --android-assets-dir /mnt/data/renpak-android-assets \
  --prune-android-assets \
  --verify-archive \
  --expect-stripped
```

Use `--prune-android-assets` when replacing loose Android assets from a smaller or different VP9 run. It moves stale `game/renpak` files into `.renpak_prepare_backups/` before copying the new assets, so APKs do not silently retain old bundles.

Check the local Android/RAPT toolchain:

```bash
ANDROID_HOME=/opt/android-sdk renpak doctor android \
  --renpy-sdk /path/to/renpy-sdk-8.4.1
```

Build, install, and launch the Android package through RAPT:

```bash
python3 scripts/build_android_package.py /path/to/project \
  --renpy-sdk /path/to/renpy-sdk-8.4.1 \
  --android-home /opt/android-sdk \
  --java-home /path/to/jdk-21 \
  --output-apk /mnt/data/renpak-output.apk
```

The build script installs the Android runtime bridge into RAPT, copies the
project into a clean build directory, ignores `*.bak.*` and
`.renpak_prepare_backups`, and then calls RAPT. Use `--no-install --no-launch`
to build an APK without touching a connected device.

For a reproducible MVP smoke test that builds VP9 assets, prepares/prunes a
Ren'Py project, builds an APK, verifies the packaged assets, and optionally
checks a connected device for MediaCodec VP9 decode:

```bash
python3 scripts/android_vp9_smoke.py \
  --source-rpa /path/to/archive_0.09.05.rpa \
  --project /path/to/renpy-project \
  --renpy-sdk /path/to/renpy-sdk-8.4.1 \
  --output-dir /mnt/data/renpak-android-smoke \
  --android-home /opt/android-sdk \
  --java-home /path/to/jdk-21
```

The lower-level runtime bridge installer is also available:

```bash
python3 scripts/install_android_runtime.py /path/to/renpy-sdk-8.4.1/rapt
```

Current MVP validation on the Eternum 0.9.5 reference archive:

| Run | Manifest assets | WebM bundles | Result |
|-----|-----------------|--------------|--------|
| `--limit 128 --bundle-size 8` | 128 | 16 | APK asset verification passed; connected Android device decoded VP9 frames through `c2.qti.vp9.decoder`. |
| `--limit 512 --bundle-size 8` | 512 | 64 | APK asset verification passed; connected Android device decoded VP9 frames through `c2.qti.vp9.decoder`. |
| Full archive, `--bundle-size 8` | 12225 | 1585 | Archive verification passed with stripped originals; 12.09 GB source RPA became a 7.32 GB output RPA. |

The full-corpus run wrote 1585 Android WebM bundles totaling about 626 MB. The
`limit512` smoke packaged 64 VP9 bundles into the APK and verified
`RenpakRuntime decoded frame=...` log lines on device.

Android runtime cache behavior is bounded for MVP use:

- Bundle files extracted by the Python runtime are stored under `renpak_cache`
  and pruned as an LRU-like disk cache with a 256 MB / 4096 file ceiling.
- Decoded PNG frames written by the Java `MediaCodec` bridge are stored under
  the app cache directory and pruned with a 256 MB / 4096 file ceiling.
- `renpak doctor android` checks ffmpeg, JDK, Android SDK, adb device state,
  device VP9 codec availability, Ren'Py SDK, and RAPT.

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
