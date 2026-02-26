# renpak

AVIF compression toolchain for Ren'Py games.
Rust CLI with interactive TUI + runtime Ren'Py plugin.

License: MPL-2.0

## Architecture

Two Rust crates + runtime plugin:

- `crates/renpak-core/` — Build engine: RPA read/write, AVIF/AVIS encoding, parallel pipeline, CLI, TUI
- `crates/renpak-rt/` — Runtime decoder: AVIS frame-level random access, exports extern "C" API for ctypes
- `python/runtime/` — Deployed to game `game/` directory (.rpy + .py)
- `install.sh` — Builds and symlinks `renpak` binary to ~/.local/bin/

## Build

```bash
cargo build --release
```

Static linking for distribution (used by CI):

```bash
RENPAK_STATIC=1 cargo build --release
```

## Technical Constraints

### Ren'Py Runtime Environment

- Runtime plugin runs on Ren'Py's bundled Python 3.9.10 (CPython), not system Python
- Native libraries loaded via ctypes.CDLL — must export pure C ABI (extern "C"), no PyO3
- Runtime Python code cannot depend on any third-party packages — stdlib + Ren'Py builtins only
- Ren'Py's image preloader runs on background threads — Rust decoder must be thread-safe (per-thread context, no global state)

### Encoding Constraints

- AVIF color space: must explicitly set CICP (primaries=1, transfer=13, matrix=1, range=full) to avoid color shifts
- Resolution: pad to multiple of 8 before encoding, crop back after decoding (Ren'Py Issue #5061)
- AVIS GOP: star pattern (frame 0 = I-frame, rest = P-frames referencing frame 0) for O(1) random access
- Video re-encoding keeps .webm container — Ren'Py's ffmpeg auto-detects AV1 codec, no runtime hooks needed

### Hook Mechanism

Runtime integrates via Ren'Py hooks without engine modifications:

- `config.file_open_callback` — intercepts file requests for name mapping and sequence decoding
- `config.loadable_callback` — reports original filenames as loadable
- monkey-patch `renpy.display.pgrender.load_image` — fixes AVIF extension hint for SDL2_image

### Cross-Platform

Runtime .so/.dll/.dylib must be pre-built for each target:
- Linux x86_64 / aarch64
- Windows x86_64 / aarch64
- macOS aarch64

CI builds static binaries via GitHub Actions (tag `v*` to trigger).

## Code Standards

### Rust

- Edition 2021, MSRV follows current stable
- `renpak-rt` compiles as cdylib, exported functions use `#[no_mangle] pub extern "C"`
- Error handling: core library uses `Result<T, String>`; FFI boundary uses return codes (0=ok, negative=error)
- FFI memory: Rust-allocated buffers must be freed via `renpak_free_buffer`, never by Python

### Python (runtime plugin only)

- Must be compatible with Python 3.9 — no 3.10+ syntax (match/case, type union X|Y, etc.)
- No third-party dependencies — stdlib + Ren'Py builtins only

## Common Commands

```bash
cargo build --release                    # build
cargo test                               # test
renpak                                   # TUI (current directory)
renpak /path/to/game                     # TUI (specified directory)
renpak build in.rpa out.rpa -q 60        # headless build
```

## Do Not

- Modify Ren'Py engine source — all integration via hooks and monkey-patches
- Import third-party packages in runtime Python code
- Use PyO3 — ctypes is the only FFI path
- Use chain GOP as default — random access latency becomes unpredictable
- Assume Limited Range YUV — always specify Full Range explicitly
- Write build output to /tmp — this machine's /tmp is tmpfs (12GB), too small for RPA output (single RPA can be 12GB+)
