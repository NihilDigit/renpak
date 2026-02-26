"""AVIF encoding utilities for renpak."""

from __future__ import annotations

import os
from io import BytesIO
from pathlib import PurePosixPath

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
