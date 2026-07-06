"""renpak runtime loader — hooks into Ren'Py to serve AVIF-compressed images.

This module runs inside Ren'Py's embedded Python (2.7 for Ren'Py 7.x, 3.9 for 8.x).
No third-party dependencies. No 3.10+ syntax. No Python 3-only constructs.
"""

import json
import os
import hashlib

# These are available at runtime inside Ren'Py
import renpy  # type: ignore

_manifest = {}  # type: dict  # original_name -> manifest entry
_unsupported_logged = set()
_android_runtime = None
_android_runtime_checked = False
_bundle_cache = {}
_MAX_BUNDLE_CACHE_BYTES = 256 * 1024 * 1024
_MAX_BUNDLE_CACHE_FILES = 4096

try:
    _string_types = (basestring,)
except NameError:
    _string_types = (str,)


def install():
    # type: () -> None
    """Install renpak hooks into Ren'Py. Called from renpak_init.rpy at init -999."""
    global _manifest, _unsupported_logged

    # Load manifest from archive
    try:
        f = renpy.loader.load("renpak_manifest.json")
        data = f.read()
        f.close()
        if isinstance(data, bytes):
            data = data.decode("utf-8")
        raw = json.loads(data)
        _manifest = _parse_manifest(raw)
        _unsupported_logged = set()
        renpy.display.log.write("renpak: loaded manifest with %d entries" % len(_manifest))
    except Exception as e:
        renpy.display.log.write("renpak: no manifest found, disabled (%s)" % e)
        return

    if not _manifest:
        renpy.display.log.write("renpak: manifest is empty, nothing to do")
        return

    _register_manifest_images()

    # Hook 1: file_open_callback — intercept file requests for mapped images
    renpy.config.file_open_callback = _file_open_callback

    # Hook 2: loadable_callback — report original names as loadable
    renpy.config.loadable_callback = _loadable_callback

    # Hook 3: monkey-patch pgrender.load_image to fix filename hint for AVIF
    _patch_load_image()

    renpy.display.log.write("renpak: hooks installed")


def _parse_manifest(raw):
    # type: (object) -> dict
    """Return normalized original-name -> entry mapping for legacy or v2 manifests."""
    if not isinstance(raw, dict):
        return {}

    if raw.get("version") == 2 and isinstance(raw.get("assets"), dict):
        source = raw.get("assets")
    else:
        source = raw

    # Normalize keys to lowercase — Ren'Py may request with different casing.
    manifest = {}
    for key, entry in source.items():
        if isinstance(key, _string_types):
            manifest[key.lower()] = entry
    return manifest


def _entry_target(entry):
    # type: (object) -> object
    if isinstance(entry, _string_types):
        return entry
    if isinstance(entry, dict):
        target = entry.get("target")
        if isinstance(target, _string_types) and target:
            return target
    return None


def _entry_mode(entry):
    # type: (object) -> object
    if isinstance(entry, _string_types):
        return "avif"
    if isinstance(entry, dict):
        mode = entry.get("mode")
        if isinstance(mode, _string_types):
            return mode
    return None


def _target_is_avif(target):
    # type: (object) -> bool
    return isinstance(target, _string_types) and target.lower().endswith(".avif")


def _auto_image_name(path):
    # type: (str) -> object
    """Return the image name Ren'Py's default images/ scan would assign."""
    normalized = path.replace("\\", "/")
    prefix = "images/"
    if not normalized.lower().startswith(prefix):
        return None

    basename = os.path.basename(normalized)
    base, ext = os.path.splitext(basename)
    if not ext:
        return None

    base = base.lower()
    base, _, _oversample = base.partition("@")
    base = base.strip()
    if not base:
        return None
    return base


def _register_manifest_images():
    # type: () -> None
    image_func = getattr(renpy, "image", None)
    if image_func is None and hasattr(renpy, "exports"):
        image_func = getattr(renpy.exports, "image", None)

    has_image_func = getattr(renpy, "has_image", None)
    if has_image_func is None and hasattr(renpy, "exports"):
        has_image_func = getattr(renpy.exports, "has_image", None)

    if image_func is None:
        _log_once("image-register-api", "renpak: image registration API unavailable")
        return

    registered = 0
    for original, entry in _manifest.items():
        mode = _entry_mode(entry)
        if mode not in ("avif", "file", "passthrough", "vp9_bundle_frame"):
            continue

        image_name = _auto_image_name(original)
        if image_name is None:
            continue

        try:
            if has_image_func is not None and has_image_func(image_name, exact=True):
                continue
            image_func(image_name, original)
            registered += 1
        except Exception as e:
            _log_once("image-register-" + original, "renpak: failed to register image %s: %s" % (image_name, e))

    if registered:
        renpy.display.log.write("renpak: registered %d manifest images" % registered)


def _log_unsupported_once(mode):
    # type: (object) -> None
    key = mode
    if key is None:
        key = "<malformed>"
    if key in _unsupported_logged:
        return
    _unsupported_logged.add(key)
    renpy.display.log.write("renpak: unsupported manifest entry mode %s, using fallback" % key)


def _log_once(key, message):
    # type: (str, str) -> None
    if key in _unsupported_logged:
        return
    _unsupported_logged.add(key)
    renpy.display.log.write(message)


def _load_archive_entry(target, mark_avif):
    # type: (str, bool) -> object
    try:
        f = renpy.loader.load_from_archive(target)
        if f is not None and mark_avif:
            f._renpak_avif = True
        return f
    except Exception as e:
        renpy.display.log.write("renpak: load_from_archive(%s) failed: %s" % (target, e))
        return None


def _android_runtime_class():
    # type: () -> object
    global _android_runtime, _android_runtime_checked

    if _android_runtime_checked:
        return _android_runtime

    _android_runtime_checked = True
    try:
        from jnius import autoclass  # type: ignore
        _android_runtime = autoclass("org.renpy.android.PythonSDLActivity")
        renpy.display.log.write("renpak: Android runtime bridge available")
        try:
            _log_once("android-runtime-stats", "renpak: %s" % _android_runtime.renpakStats())
        except Exception as e:
            _log_once("android-runtime-stats-error", "renpak: Android runtime stats unavailable (%s)" % e)
    except Exception as e:
        _android_runtime = None
        _log_once("android-runtime-missing", "renpak: Android runtime bridge unavailable (%s)" % e)

    return _android_runtime


def _cache_root():
    # type: () -> object
    base = os.environ.get("ANDROID_PRIVATE") or os.environ.get("ANDROID_PUBLIC")
    if not base:
        return None
    root = os.path.join(base, "renpak_cache")
    try:
        if not os.path.isdir(root):
            os.makedirs(root)
        return root
    except Exception as e:
        _log_once("cache-root", "renpak: unable to create cache root (%s)" % e)
        return None


def _safe_cache_name(value, suffix):
    # type: (str, str) -> str
    h = hashlib.sha1()
    if isinstance(value, bytes):
        h.update(value)
    else:
        h.update(value.encode("utf-8"))
    return h.hexdigest() + suffix


def _bundle_cache_files(root):
    # type: (str) -> list
    files = []
    for base, _dirs, names in os.walk(root):
        for name in names:
            path = os.path.join(base, name)
            try:
                st = os.stat(path)
            except Exception:
                continue
            if not os.path.isfile(path):
                continue
            files.append((st.st_mtime, path, st.st_size))
    return files


def _prune_bundle_cache(root):
    # type: (str) -> None
    try:
        files = _bundle_cache_files(root)
        total = 0
        for _mtime, _path, size in files:
            total += size

        if total <= _MAX_BUNDLE_CACHE_BYTES and len(files) <= _MAX_BUNDLE_CACHE_FILES:
            return

        files.sort()
        for _mtime, path, size in files:
            if total <= _MAX_BUNDLE_CACHE_BYTES and len(files) <= _MAX_BUNDLE_CACHE_FILES:
                break
            try:
                os.remove(path)
                total -= size
            except Exception:
                pass
            try:
                files.remove((_mtime, path, size))
            except Exception:
                pass
    except Exception as e:
        _log_once("bundle-cache-prune", "renpak: bundle cache prune failed (%s)" % e)


def _extract_bundle_to_cache(target):
    # type: (str) -> object
    if target in _bundle_cache and os.path.exists(_bundle_cache[target]):
        try:
            os.utime(_bundle_cache[target], None)
        except Exception:
            pass
        return _bundle_cache[target]

    root = _cache_root()
    if root is None:
        return None

    out_path = os.path.join(root, _safe_cache_name(target, ".webm"))
    if os.path.exists(out_path):
        _bundle_cache[target] = out_path
        try:
            os.utime(out_path, None)
        except Exception:
            pass
        _log_once("bundle-cache-hit-" + target, "renpak: bundle cache hit %s" % target)
        return out_path

    try:
        try:
            f = renpy.loader.load(target)
        except Exception:
            f = renpy.loader.load_from_archive(target)
        if f is None:
            _log_once("bundle-missing-" + target, "renpak: bundle target not found: %s" % target)
            return None
        data = f.read()
        f.close()
        out = open(out_path, "wb")
        try:
            out.write(data)
        finally:
            out.close()
        _bundle_cache[target] = out_path
        _prune_bundle_cache(root)
        _log_once("bundle-cached-" + target, "renpak: cached bundle %s -> %s" % (target, out_path))
        return out_path
    except Exception as e:
        _log_once("bundle-cache-" + target, "renpak: failed to cache bundle %s: %s" % (target, e))
        return None


def _load_vp9_bundle_frame(name, entry):
    # type: (str, dict) -> object
    runtime = _android_runtime_class()
    if runtime is None:
        _log_once("vp9-no-runtime", "renpak: VP9 frame requested but Android runtime is unavailable")
        return None

    target = _entry_target(entry)
    frame = entry.get("frame")
    if not isinstance(target, _string_types) or not isinstance(frame, int):
        _log_unsupported_once("vp9_bundle_frame")
        return None

    bundle_path = _extract_bundle_to_cache(target)
    if bundle_path is None:
        return None

    out_name = _safe_cache_name(name + ":" + target + ":" + str(frame), ".png")
    try:
        _log_once("vp9-decode-start-" + name, "renpak: decoding %s frame %s from %s" % (name, frame, target))
        decoded_path = runtime.renpakDecodeFrameToCache(bundle_path, int(frame), out_name)
        if decoded_path:
            _log_once("vp9-decode-ok-" + name, "renpak: decoded %s -> %s" % (name, decoded_path))
            try:
                _log_once("android-runtime-stats-after-decode", "renpak: %s" % runtime.renpakStats())
            except Exception:
                pass
            return open(decoded_path, "rb")
        _log_once("vp9-decode-empty-" + name, "renpak: Android VP9 frame decode returned no path")
    except Exception as e:
        _log_once("vp9-decode-" + target, "renpak: Android VP9 frame decode failed: %s" % e)

    return None


def _file_open_callback(name):
    # type: (str) -> object
    """Redirect requests for original image names to their compressed versions."""
    entry = _manifest.get(name.lower())
    if entry is None:
        return None

    _log_once("manifest-hit-" + name, "renpak: manifest hit %s" % name)

    mode = _entry_mode(entry)
    target = _entry_target(entry)

    if mode == "vp9_bundle_frame" and isinstance(entry, dict):
        return _load_vp9_bundle_frame(name, entry)

    if target is not None and (mode == "avif" or _target_is_avif(target)):
        return _load_archive_entry(target, True)

    if target is not None and (mode == "file" or mode == "passthrough"):
        return _load_archive_entry(target, False)

    _log_unsupported_once(mode)

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
