# renpak

AVIF compression toolchain for Ren'Py games. Re-encodes images inside RPA archives to AVIF, dramatically reducing file size with transparent runtime loading.

## Install

```bash
# Linux / macOS
curl -fsSL https://renpak.vercel.app/install | bash

# Windows (PowerShell)
irm https://renpak.vercel.app/install.ps1 | iex
```

Or build from source (requires libavif with rav1e encoder):

```bash
cargo build --release
./install.sh  # symlinks binary to ~/.local/bin/
```

## Usage

```bash
cd /path/to/game/root
renpak
```

The interactive TUI lets you:
- Browse and toggle directory exclusions (GUI assets excluded by default)
- Adjust AVIF quality and encoder speed
- Monitor real-time build progress
- Install compressed RPA + runtime plugin
- Launch the game to verify, then revert or clean up backup

Headless mode for scripting:

```bash
renpak build input.rpa output.rpa -q 60 -s 8 -x images/ui/
```

## Results

| Game | Original | Compressed | Image ratio | Time |
|------|----------|------------|-------------|------|
| Agent17 0.25.9 | 2.3 GB | 1.3 GB | 33% | 5 min (16 cores) |
| Eternum 0.9.5 | 11.5 GB | 7.4 GB | 21% | 9 min (16 cores) |

## Architecture

```
crates/renpak-core/        Rust build engine + CLI + TUI
  src/lib.rs                 libavif FFI, AVIF/AVIS encoding
  src/rpa.rs                 RPA-3.0 reader/writer (pickle index)
  src/pipeline.rs            Parallel encoding pipeline (Rayon)
  src/tui.rs                 Interactive TUI (ratatui)
  src/main.rs                CLI entry point

python/runtime/            Ren'Py runtime plugin
  renpak_init.rpy            init -999 bootstrap hook
  renpak_loader.py           File interception + AVIF loading
```

The runtime plugin hooks into Ren'Py via `file_open_callback`, `loadable_callback`, and a `load_image` monkey-patch — no engine modifications required.

## License

MPL-2.0
