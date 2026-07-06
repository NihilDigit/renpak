# renpak

Corpus-aware asset optimizer for Ren'Py games.
Rust CLI/TUI plus Ren'Py runtime hooks, with an Android runtime path for mobile builds.

License: MPL-2.0

## Project Direction

renpak v1 is an AVIF image compressor. The v2 direction is broader:

- Visual novels made from Koikatsu/HS2-style 3D pipelines have high cross-file redundancy.
- The main win is not better per-file compression. It is clustering related assets and encoding repeated scenes once.
- Android/mobile output is a first-class target. Prefer formats that work well through Android's media stack.

Primary v2 strategy:

- Images: cluster similar full-screen CGs and encode each cluster as a VP9/WebM frame bundle with bounded GOPs. The manifest maps the original image name to a bundle path and frame index.
- Videos: use selective mobile re-encoding, not cross-video interleaving by default. Baseline is VP9/WebM, 720p, original frame rate, realtime encoder, CRF 38, audio copy.
- Audio: copy by default. Optional Opus conversion should only handle high-bitrate outliers and must keep the original if savings are small.
- Android: add a Kotlin/JNI runtime for high-performance VP9 bundle frame extraction through MediaCodec plus disk/LRU cache.

AVIF remains useful as legacy support and a desktop/single-image fallback, but it is not the main mobile compression direction.

## Current Architecture

Two Rust crates plus runtime plugin:

- `crates/renpak-core/` - RPA read/write, current AVIF pipeline, parallel build engine, CLI, TUI. New asset profiles and transform backends belong here.
- `crates/renpak-rt/` - current native AVIS decoder exported as a C ABI. Future native desktop frame decode can live here, but Android should prefer MediaCodec.
- `python/runtime/` - deployed to a game's `game/` directory. Installs Ren'Py hooks for file redirection and loadability.
- `install.sh` - builds and symlinks `renpak` to `~/.local/bin/`.

Important: the checked-in code may still implement the v1 AVIF path. Do not assume the v2 VP9 bundle, video profile, or Android runtime already exists unless you have verified it in code.

## Build

```bash
cargo build --release
```

Static linking for distribution:

```bash
RENPAK_STATIC=1 cargo build --release
```

Common local commands:

```bash
cargo test
renpak
renpak /path/to/game
renpak build in.rpa out.rpa -q 60
```

## Asset Profiles

### Images

Image compression should be corpus-aware.

- Use perceptual fingerprints, dimensions, path proximity, and naming patterns to find clusters.
- Favor conservative clusters. Wrong clustering is worse than leaving assets alone.
- Exclude by default: `gui/`, UI sprites, icons, masks, transparent PNGs, text-heavy images, maps, phone screenshots, and other assets where downscaling or chroma subsampling can hurt readability.
- Mobile profile may downscale normal full-screen CGs from 1080p to 720p.
- Desktop profile should preserve original resolution unless explicitly configured.
- VP9 bundles must use bounded GOPs for random access. Start testing with GOP 8 and 16. Long GOP is for analysis only, not runtime default.
- Keep bundle sizes small enough for cache and seek behavior. A practical starting point is 8-32 frames per bundle.
- Always keep enough manifest metadata to validate dimensions, frame count, codec, profile, and fallback behavior.

The new manifest format should be structured and versioned. Keep backward compatibility with the legacy JSON shape:

```json
{
  "original.png": "compressed.avif"
}
```

New entries should record fields such as:

```json
{
  "version": 2,
  "assets": {
    "images/foo.jpg": {
      "kind": "image",
      "mode": "vp9_bundle_frame",
      "target": "renpak/bundles/b001.webm",
      "frame": 7,
      "width": 1280,
      "height": 720,
      "codec": "vp9",
      "gop": 8,
      "profile": "mobile-720"
    }
  }
}
```

### Videos

Do not reduce video frame rate by default. 3D VN animation often looks bad at 30 fps.

Mobile video baseline:

```bash
ffmpeg -i input.webm \
  -vf "scale=-2:720:flags=lanczos" \
  -c:v libvpx-vp9 \
  -deadline realtime \
  -cpu-used 8 \
  -row-mt 1 \
  -crf 38 \
  -b:v 0 \
  -pix_fmt yuv420p \
  -c:a copy \
  output.webm
```

Rules:

- Preserve original FPS unless a low-motion detector explicitly marks a clip as safe to reduce.
- Keep WebM/VP9 as the portable baseline.
- Only re-encode videos that are worth it: high bitrate, above 720p, or large outliers.
- If the source is already 720p or lower and low bitrate, copy it.
- Keep the original if output savings are below a threshold such as 8-10%.
- Do not use cross-video frame interleaving as a default. It harms normal continuous playback and complicates runtime.
- Concatenating related clips can be an optional experiment, but expect modest gains compared with the complexity.

### Audio

Audio is not the main compression target.

- Default profile: copy audio.
- Optional profile: Opus in Ogg for high-bitrate outliers only.
- Do not transcode already-low-bitrate Vorbis/MP3 just to normalize formats.
- Use an output-size guard. If Opus output is not at least 8-10% smaller, keep the original.

Suggested optional profiles:

- `opus-quality`: 128 kbps, only if source bitrate is meaningfully higher.
- `opus-mobile`: music 96 kbps, SFX/ambience 64-80 kbps, voice 48-64 kbps.

Record the actual codec in the manifest. `.ogg` is a container and may contain Vorbis or Opus.

## Runtime Constraints

### Ren'Py Python Runtime

- Runtime plugin runs inside Ren'Py's bundled Python, not system Python.
- Must remain compatible with Python 2.7 for Ren'Py 7.x and Python 3.9 for Ren'Py 8.x.
- No third-party Python packages. Use only stdlib and Ren'Py builtins.
- No Python 3-only or 3.10+ syntax in `python/runtime/`.
- Integrate through Ren'Py hooks. Do not modify Ren'Py engine source.
- Existing hook points include `config.file_open_callback`, `config.loadable_callback`, and image loader monkey-patches.
- Avoid heavy decode work in Python callbacks. Use native/runtime services and caching.
- Ren'Py may preload images on background threads. Runtime state must be thread-safe.

### Android Runtime

The Android path should use platform media capabilities.

- Implement Android-specific frame extraction in Kotlin, with JNI only where it clearly helps.
- Prefer `MediaExtractor` + `MediaCodec` for VP9 bundle frame decode.
- Do not build the first version around software VP9 decode on Android.
- Decode requested bundle frames into a bounded disk cache and an in-memory LRU cache.
- Python should resolve an original filename to a cached image file or bytes with minimal copying.
- Do not start with zero-copy texture injection. First make file/cache based image resolution stable and measurable.
- Do not block the Android UI thread.
- Add a doctor/check command before APK build support. It should report Ren'Py/RAPT, JDK, Android SDK, signing, and codec/profile assumptions.

## Code Standards

### Rust

- Edition 2021. MSRV follows current stable unless otherwise documented.
- Core library APIs should return `Result<T, String>` where that is the established local pattern.
- FFI boundaries return integer status codes and expose pure C ABI functions.
- Do not use PyO3 for Ren'Py runtime integration. Ren'Py loads native code through ctypes or platform-specific bridges.
- Rust-allocated buffers crossing FFI must be freed by a matching renpak free function, never by Python.
- Keep transforms profile-driven instead of hardcoding one codec path through the pipeline.

### Python Runtime

- Keep runtime code small, defensive, and dependency-free.
- Preserve original filename semantics through manifest mapping.
- Normalize lookup keys carefully, but do not lose original paths in manifests or diagnostics.
- Log failures through Ren'Py logging and fall back when possible.

### Kotlin/JNI Runtime

- Keep the public Kotlin API narrow: load manifest, resolve image, decode/cache frame, clear cache, report stats.
- Treat decoder instances and cache mutation as concurrent resources.
- Put measurable limits on memory cache size, disk cache size, open decoders, and decode queue length.
- Surface decode errors to Python without crashing the game.

## Research Baselines

Eternum 0.9.5 was used as the current reference workload.

Observed RPA composition:

- Images: about 12.9k files, 5.1 GiB.
- Videos: 449 WebM files, 5.36 GiB, mostly 1080p60 VP9.
- Audio: about 1.3k files, 0.86 GiB.

Image findings:

- Most full-screen images are 1920x1080.
- Coarse visual clustering covered about 60% of full-screen CGs at a strong similarity threshold.
- Small VP9 inter-frame bundle tests were far smaller than single-frame AVIF on highly similar clusters.

Video findings:

- Some source videos are extremely high bitrate.
- 720p VP9 realtime CRF 38 is the current baseline direction.
- Full-quality VP9 (`deadline=good`) is too slow as a default.

Audio findings:

- Many Ogg/Vorbis files are already near 100-140 kbps.
- Opus is fast to encode and useful for high-bitrate outliers, but default copy is simpler and safer for v1 mobile profiles.

## Do Not

- Do not treat AVIF as the primary mobile path.
- Do not blindly downscale GUI, text-heavy images, masks, or transparent assets.
- Do not use long or chain GOPs as runtime defaults for image bundles.
- Do not reduce video to 30 fps by default.
- Do not cross-video interleave frames in the default video pipeline.
- Do not re-encode low-bitrate audio just to normalize codec/container.
- Do not import third-party Python packages in Ren'Py runtime code.
- Do not modify Ren'Py engine source.
- Do not write large build output to `/tmp`; this machine's `/tmp` is tmpfs and too small for large RPA outputs.
