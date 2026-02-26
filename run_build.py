#!/usr/bin/env python3
"""Call renpak_build via ctypes to run the full AVIF encoding pipeline."""

import ctypes
import sys
import time
from pathlib import Path

# Force line-buffered stdout so progress is visible in pipes/files
sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)

# --- Load library ---
LIB_PATH = Path(__file__).parent / "target/release/librenpak_core.so"
lib = ctypes.CDLL(str(LIB_PATH))

# --- Progress callback ---

class ProgressEvent(ctypes.Structure):
    _fields_ = [
        ("kind", ctypes.c_uint32),
        ("done", ctypes.c_uint32),
        ("total", ctypes.c_uint32),
        ("message", ctypes.c_char_p),
        ("original_bytes", ctypes.c_uint64),
        ("compressed_bytes", ctypes.c_uint64),
    ]

PROGRESS_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.POINTER(ProgressEvent))

_start_time = time.monotonic()

@PROGRESS_CB_TYPE
def progress_cb(ev_ptr):
    ev = ev_ptr.contents
    kind = ev.kind
    msg = ev.message.decode("utf-8", errors="replace") if ev.message else ""
    elapsed = time.monotonic() - _start_time

    if kind == 0:  # phase_start
        print(f"\n[{elapsed:7.1f}s] === {msg} ===")
    elif kind == 1:  # task_done
        pct = ev.done / ev.total * 100 if ev.total > 0 else 0
        orig_mb = ev.original_bytes / 1_048_576
        comp_mb = ev.compressed_bytes / 1_048_576
        ratio = comp_mb / orig_mb * 100 if orig_mb > 0 else 0
        # ETA
        if ev.done > 0:
            rate = ev.done / elapsed
            remaining = (ev.total - ev.done) / rate if rate > 0 else 0
            eta_str = f"ETA {remaining:.0f}s"
        else:
            eta_str = ""
        print(f"  [{elapsed:7.1f}s] {ev.done}/{ev.total} ({pct:.0f}%) "
              f"{orig_mb:.1f}MB->{comp_mb:.1f}MB ({ratio:.0f}%) {eta_str}  {msg}")
    elif kind == 2:  # phase_end
        print(f"[{elapsed:7.1f}s] === {msg} ===\n")
    elif kind == 3:  # warning
        print(f"  [{elapsed:7.1f}s] WARNING: {msg}", file=sys.stderr)

# --- Setup function signature ---
lib.renpak_build.restype = ctypes.c_int
lib.renpak_build.argtypes = [
    ctypes.c_char_p,   # input_rpa
    ctypes.c_char_p,   # output_rpa
    ctypes.c_int,      # quality
    ctypes.c_int,      # speed
    ctypes.c_int,      # workers
    PROGRESS_CB_TYPE,  # progress_cb
]

# --- Run ---
INPUT_RPA = b"/home/spencer/Games/Eternum-0.9.5-pc/game/archive_0.09.05.rpa"
OUTPUT_RPA = b"/home/spencer/Games/Eternum-0.9.5-pc/renpak/output/archive_0.09.05.rpa"

# Ensure output dir exists
Path(OUTPUT_RPA.decode()).parent.mkdir(parents=True, exist_ok=True)

quality = 60
speed = 8
workers = 0  # 0 = auto-detect

print(f"Input:   {INPUT_RPA.decode()}")
print(f"Output:  {OUTPUT_RPA.decode()}")
print(f"Quality: {quality}, Speed: {speed}, Workers: {workers} (0=auto)")
print()

ret = lib.renpak_build(INPUT_RPA, OUTPUT_RPA, quality, speed, workers, progress_cb)
elapsed = time.monotonic() - _start_time

if ret == 0:
    in_size = Path(INPUT_RPA.decode()).stat().st_size / 1_048_576
    out_size = Path(OUTPUT_RPA.decode()).stat().st_size / 1_048_576
    print(f"Success! {in_size:.0f}MB -> {out_size:.0f}MB ({out_size/in_size*100:.0f}%) in {elapsed:.0f}s")
else:
    print(f"Build failed with code {ret}", file=sys.stderr)
    sys.exit(1)
