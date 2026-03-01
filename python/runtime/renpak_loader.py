"""renpak runtime loader — hooks into Ren'Py to serve AVIF-compressed images.

This module runs inside Ren'Py's embedded Python 3.9.
No third-party dependencies. No 3.10+ syntax.
"""

from __future__ import annotations

import json
import os

# These are available at runtime inside Ren'Py
import renpy  # type: ignore

_manifest = {}  # type: dict  # original_name -> str (avif path)


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

    # Hook 1: file_open_callback — intercept file requests for mapped images
    renpy.config.file_open_callback = _file_open_callback

    # Hook 2: loadable_callback — report original names as loadable
    renpy.config.loadable_callback = _loadable_callback

    # Hook 3: monkey-patch pgrender.load_image to fix filename hint for AVIF
    _patch_load_image()

    renpy.display.log.write("renpak: hooks installed")


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
