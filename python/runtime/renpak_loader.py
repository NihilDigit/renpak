"""renpak runtime loader — hooks into Ren'Py to serve AVIF-compressed images.

This module runs inside Ren'Py's embedded Python 3.9.
No third-party dependencies. No 3.10+ syntax.
"""

from __future__ import annotations

import ctypes
import io
import json
import os

# These are available at runtime inside Ren'Py
import renpy  # type: ignore

_manifest = {}  # type: dict  # original_name -> str (avif) or dict (avis)
_rt_lib = None  # type: ctypes.CDLL | None
_avis_cache = {}  # type: dict[str, bytes]  # avis_name -> raw bytes from archive


def install():
    # type: () -> None
    """Install renpak hooks into Ren'Py. Called from renpak_init.rpy at init -999."""
    global _manifest

    # Load manifest from archive
    try:
        f = renpy.loader.load("renpak_manifest.json")
        data = f.read()
        f.close()
        if isinstance(data, bytes):
            data = data.decode("utf-8")
        raw = json.loads(data)
        # Normalize keys to lowercase — Ren'Py may request with different casing
        _manifest = {k.lower(): v for k, v in raw.items()}
        renpy.display.log.write("renpak: loaded manifest with %d entries" % len(_manifest))
    except Exception as e:
        renpy.display.log.write("renpak: no manifest found, disabled (%s)" % e)
        return

    if not _manifest:
        renpy.display.log.write("renpak: manifest is empty, nothing to do")
        return

    # Check for AVIS entries and try to load runtime decoder
    has_avis = any(isinstance(v, dict) for v in _manifest.values())
    if has_avis:
        _init_rt_lib()

    # Hook 1: file_open_callback — intercept file requests for mapped images
    renpy.config.file_open_callback = _file_open_callback

    # Hook 2: loadable_callback — report original names as loadable
    renpy.config.loadable_callback = _loadable_callback

    # Hook 3: monkey-patch pgrender.load_image to fix filename hint for AVIF
    _patch_load_image()

    renpy.display.log.write("renpak: hooks installed")


def _init_rt_lib():
    # type: () -> None
    """Load librenpak_rt.so for AVIS decoding."""
    global _rt_lib
    # TODO: support platform-specific runtime library names (.dll/.dylib/.so) for AVIS.
    lib_name = "librenpak_rt.so"
    # Look next to this file (game/ directory)
    lib_path = os.path.join(os.path.dirname(__file__), lib_name)
    try:
        lib = ctypes.CDLL(lib_path)
        lib.renpak_decode_frame_png.argtypes = [
            ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_uint32,
            ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),
            ctypes.POINTER(ctypes.c_size_t),
        ]
        lib.renpak_decode_frame_png.restype = ctypes.c_int
        lib.renpak_free.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
        lib.renpak_free.restype = None
        _rt_lib = lib
        renpy.display.log.write("renpak: loaded %s" % lib_path)
    except Exception as e:
        renpy.display.log.write("renpak: failed to load %s (%s), AVIS disabled" % (lib_path, e))


def _load_avis_bytes(avis_name):
    # type: (str) -> bytes
    """Load AVIS container bytes from archive, with caching."""
    if avis_name in _avis_cache:
        return _avis_cache[avis_name]
    f = renpy.loader.load_from_archive(avis_name)
    data = f.read()
    f.close()
    if isinstance(data, memoryview):
        data = bytes(data)
    _avis_cache[avis_name] = data
    return data


def _decode_frame_png(avis_data, frame_idx):
    # type: (bytes, int) -> bytes
    """Decode a single frame from AVIS data, returning PNG bytes."""
    if _rt_lib is None:
        raise RuntimeError("librenpak_rt not loaded")

    buf = ctypes.create_string_buffer(avis_data)
    out_png = ctypes.POINTER(ctypes.c_uint8)()
    out_len = ctypes.c_size_t(0)

    ret = _rt_lib.renpak_decode_frame_png(
        ctypes.cast(buf, ctypes.POINTER(ctypes.c_uint8)),
        len(avis_data),
        frame_idx,
        ctypes.byref(out_png),
        ctypes.byref(out_len),
    )
    if ret != 0:
        raise RuntimeError("renpak_decode_frame_png failed: %d" % ret)

    png_bytes = bytes(
        ctypes.cast(out_png, ctypes.POINTER(ctypes.c_uint8 * out_len.value)).contents
    )
    _rt_lib.renpak_free(out_png, out_len)
    return png_bytes


def _file_open_callback(name):
    # type: (str) -> object
    """Redirect requests for original image names to their compressed versions."""
    entry = _manifest.get(name.lower())
    if entry is None:
        return None

    if isinstance(entry, str):
        # Scatter AVIF path — load from archive and tag as AVIF
        try:
            f = renpy.loader.load_from_archive(entry)
            if f is not None:
                f._renpak_avif = True
            return f
        except Exception as e:
            renpy.display.log.write("renpak: load_from_archive(%s) failed: %s" % (entry, e))
            return None
    elif isinstance(entry, dict) and _rt_lib is not None:
        # AVIS sequence path
        try:
            avis_name = entry["avis"]
            frame_idx = entry["frame"]
            avis_data = _load_avis_bytes(avis_name)
            png_bytes = _decode_frame_png(avis_data, frame_idx)
            return io.BytesIO(png_bytes)
        except Exception as e:
            renpy.display.log.write("renpak: avis decode(%s) failed: %s" % (entry, e))
            return None

    return None


def _loadable_callback(name):
    # type: (str) -> bool
    """Tell Ren'Py that original image names are still loadable."""
    return name.lower() in _manifest


def _patch_load_image():
    # type: () -> None
    """Monkey-patch pgrender.load_image to pass correct .avif extension hint."""
    try:
        orig = renpy.display.pgrender.load_image
    except AttributeError:
        renpy.display.log.write("renpak: pgrender.load_image not found, skipping patch")
        return

    def _patched_load_image(f, filename, size=None):
        # Only change the hint for files we tagged in _file_open_callback
        if getattr(f, '_renpak_avif', False):
            base, _, _ = filename.rpartition('.')
            if base:
                filename = base + '.avif'

        try:
            return orig(f, filename, size=size)
        except Exception as e:
            renpy.display.log.write("renpak: load_image(%s) failed: %s" % (filename, e))
            raise

    renpy.display.pgrender.load_image = _patched_load_image
