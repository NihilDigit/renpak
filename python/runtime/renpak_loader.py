"""renpak runtime loader — hooks into Ren'Py to serve AVIF-compressed images.

This module runs inside Ren'Py's embedded Python 3.9.
No third-party dependencies. No 3.10+ syntax.
"""

from __future__ import annotations

import json

# These are available at runtime inside Ren'Py
import renpy  # type: ignore

_manifest = {}  # type: dict[str, str]  # original_name -> avif_name


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
        _manifest = json.loads(data)
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
    """Redirect requests for original image names to their AVIF versions."""
    if name not in _manifest:
        return None
    avif_name = _manifest[name]
    try:
        return renpy.loader.load_from_archive(avif_name)
    except Exception:
        return None


def _loadable_callback(name):
    # type: (str) -> bool
    """Tell Ren'Py that original image names are still loadable."""
    return name in _manifest


def _patch_load_image():
    # type: () -> None
    """Monkey-patch pgrender.load_image to pass correct .avif extension hint."""
    try:
        orig = renpy.display.pgrender.load_image
    except AttributeError:
        renpy.display.log.write("renpak: pgrender.load_image not found, skipping patch")
        return

    def _patched_load_image(f, filename, size=None):
        # Detect AVIF by checking for 'ftyp' box at offset 4
        try:
            pos = f.tell()
            header = f.read(12)
            f.seek(pos)
        except Exception:
            return orig(f, filename, size=size)

        if len(header) >= 8 and header[4:8] == b'ftyp':
            base, _, _ = filename.rpartition('.')
            if base:
                filename = base + '.avif'

        return orig(f, filename, size=size)

    renpy.display.pgrender.load_image = _patched_load_image
