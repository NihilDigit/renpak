"""AVIF/AVIS encoding utilities for renpak."""

from __future__ import annotations

import ctypes
import os
import re
from collections import defaultdict
from io import BytesIO
from pathlib import Path, PurePosixPath

# Try to import pillow-avif-plugin for older Pillow versions
try:
    import pillow_avif  # noqa: F401
except ImportError:
    pass

from PIL import Image

IMAGE_EXTENSIONS: set[str] = {'.jpg', '.jpeg', '.png', '.webp', '.bmp'}


def is_image(name: str) -> bool:
    """Check if a filename has a recognized image extension."""
    ext = PurePosixPath(name).suffix.lower()
    return ext in IMAGE_EXTENSIONS


SKIP_PREFIXES: tuple[str, ...] = ('gui/',)


def should_encode(name: str) -> bool:
    """Check if a file should be AVIF-encoded (image and not in skip list)."""
    return is_image(name) and not any(name.startswith(p) for p in SKIP_PREFIXES)


def encode_avif(data: bytes, quality: int = 50, speed: int = 6) -> bytes:
    """Encode image data (any supported format) to AVIF.

    Args:
        data: Raw image file bytes (JPG, PNG, etc.)
        quality: AVIF quality 1-63 (lower = smaller file, more loss)
        speed: Encoding speed 0-10 (higher = faster, slightly larger)

    Returns:
        AVIF-encoded bytes
    """
    img = Image.open(BytesIO(data))
    # Preserve alpha channel if present
    if img.mode == 'RGBA':
        pass
    elif img.mode != 'RGB':
        img = img.convert('RGB')
    buf = BytesIO()
    img.save(buf, 'AVIF', quality=quality, speed=speed)
    return buf.getvalue()


def get_avif_name(original_name: str) -> str:
    """Convert a filename to use .avif extension.

    Example: "images/01/ale 1.jpg" → "images/01/ale 1.avif"
    """
    p = PurePosixPath(original_name)
    return str(p.with_suffix('.avif'))


# --- AVIS sequence support ---

SEQUENCE_THRESHOLD = 5  # Minimum group size to use AVIS

_NUM_RE = re.compile(r'^(.*?)(\d+)(\.[^.]+)$')


def group_by_prefix(names: list[str]) -> tuple[dict[str, list[str]], list[str]]:
    """Group filenames by prefix, extracting trailing numeric index.

    Returns (groups, ungrouped) where groups maps prefix → sorted list of names,
    and ungrouped contains names that don't match or belong to small groups.
    """
    groups: dict[str, list[tuple[int, str]]] = defaultdict(list)
    ungrouped: list[str] = []

    for name in names:
        m = _NUM_RE.match(name)
        if m:
            prefix = m.group(1)
            num = int(m.group(2))
            groups[prefix].append((num, name))
        else:
            ungrouped.append(name)

    result: dict[str, list[str]] = {}
    for prefix, items in groups.items():
        items.sort()
        if len(items) >= SEQUENCE_THRESHOLD:
            result[prefix] = [name for _, name in items]
        else:
            ungrouped.extend(name for _, name in items)

    return result, ungrouped


# --- ctypes wrapper for renpak-core ---

_core_lib: ctypes.CDLL | None = None


def _load_core_lib() -> ctypes.CDLL:
    global _core_lib
    if _core_lib is not None:
        return _core_lib

    # Look for librenpak_core.so next to this file, or in target/release/
    candidates = [
        Path(__file__).parent.parent.parent / "target" / "release" / "librenpak_core.so",
        Path(__file__).parent / "librenpak_core.so",
    ]
    for p in candidates:
        if p.exists():
            _core_lib = ctypes.CDLL(str(p))
            _setup_core_lib(_core_lib)
            return _core_lib

    raise FileNotFoundError(
        f"librenpak_core.so not found. Searched: {[str(p) for p in candidates]}"
    )


def _setup_core_lib(lib: ctypes.CDLL) -> None:
    lib.renpak_encode_avis.argtypes = [
        ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),  # frames_rgba
        ctypes.c_uint32,  # frame_count
        ctypes.c_uint32,  # width
        ctypes.c_uint32,  # height
        ctypes.c_int32,   # quality
        ctypes.c_int32,   # speed
        ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),  # out_data
        ctypes.POINTER(ctypes.c_size_t),                  # out_len
    ]
    lib.renpak_encode_avis.restype = ctypes.c_int32

    lib.renpak_free.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
    lib.renpak_free.restype = None


def encode_avis(
    frames_rgba: list[tuple[bytes, int, int]],
    quality: int = 50,
    speed: int = 6,
) -> bytes:
    """Encode RGBA frames into an AVIS sequence.

    Args:
        frames_rgba: List of (rgba_bytes, width, height) tuples.
                     All frames must have the same dimensions.
        quality: AVIF quality 0-100
        speed: Encoding speed 0-10

    Returns:
        AVIS container bytes
    """
    if not frames_rgba:
        raise ValueError("No frames to encode")

    _, w0, h0 = frames_rgba[0]
    for i, (_, w, h) in enumerate(frames_rgba):
        if w != w0 or h != h0:
            raise ValueError(
                f"Frame {i} has size {w}x{h}, expected {w0}x{h0}"
            )

    lib = _load_core_lib()
    n = len(frames_rgba)

    # Build array of pointers to RGBA data
    ArrayType = ctypes.c_char_p * n
    frame_bufs = ArrayType(*[rgba for rgba, _, _ in frames_rgba])
    frame_ptrs = ctypes.cast(frame_bufs, ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)))

    out_data = ctypes.POINTER(ctypes.c_uint8)()
    out_len = ctypes.c_size_t(0)

    ret = lib.renpak_encode_avis(
        frame_ptrs, n, w0, h0, quality, speed,
        ctypes.byref(out_data), ctypes.byref(out_len),
    )
    if ret != 0:
        raise RuntimeError(f"renpak_encode_avis failed with code {ret}")

    # Copy result and free Rust buffer
    result = bytes(ctypes.cast(out_data, ctypes.POINTER(ctypes.c_uint8 * out_len.value)).contents)
    lib.renpak_free(out_data, out_len)
    return result
