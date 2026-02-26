import json
import os
import shutil
import threading
import time
from concurrent.futures import ProcessPoolExecutor, as_completed
from io import BytesIO
from pathlib import Path
from collections import defaultdict

from PIL import Image

from renpak.rpa import RpaReader, RpaWriter, RpaEntry
from renpak.encode import (
    is_image, should_encode, encode_avif, get_avif_name,
    group_by_prefix, encode_avis, SEQUENCE_THRESHOLD,
)

# Memory budget for concurrent AVIS encoding (bytes).
# Each AVIS group loads all frames as RGBA in worker memory.
# The encoder also allocates ~2x frame size internally.
# Conservative: use half of available RAM, leaving room for OS + other processes.
_RGBA_BPP = 4  # bytes per pixel
_DEFAULT_RES = (1920, 1080)  # assumed resolution for budget estimation
# Actual per-frame memory: RGBA data + Pillow Image + encoder buffers
# Empirically ~24MB/frame for 1920x1080 (3x raw RGBA)
_FRAME_MEM_MULTIPLIER = 3
_WORKER_BASE_MEM = 200 * 1024 * 1024  # ~200MB per worker process baseline


def _get_avis_mem_budget(workers: int) -> int:
    """Calculate memory budget for AVIS encoding based on available RAM."""
    try:
        with open('/proc/meminfo') as f:
            for line in f:
                if line.startswith('MemAvailable:'):
                    available = int(line.split()[1]) * 1024  # kB -> bytes
                    # Use 50% of available, minus worker base overhead
                    budget = int(available * 0.5) - workers * _WORKER_BASE_MEM
                    return max(budget, 1024 * 1024 * 1024)  # at least 1GB
    except Exception:
        pass
    return 4 * 1024 * 1024 * 1024  # fallback 4GB


# --- Worker functions (top-level for pickling) ---

def _worker_encode_avif(rpa_path: str, name: str, offset: int, length: int,
                        prefix: bytes, quality: int) -> tuple:
    """Encode a single image to AVIF. Runs in worker process."""
    entry = RpaEntry(name=name, offset=offset, length=length, prefix=prefix)
    with RpaReader(Path(rpa_path)) as reader:
        data = reader.read_file(entry)
    try:
        avif_data = encode_avif(data, quality=quality)
        avif_name = get_avif_name(name)
        return ("avif", name, avif_name, avif_data, len(data), None)
    except Exception as e:
        return ("avif_fail", name, name, data, len(data), str(e))


def _worker_encode_avis(rpa_path: str, prefix: str,
                        entries_info: list[tuple[str, int, int, bytes]],
                        quality: int, speed: int) -> tuple:
    """Encode a group of images to AVIS. Runs in worker process."""
    with RpaReader(Path(rpa_path)) as reader:
        frames_rgba = []
        group_original = 0
        group_w, group_h = None, None

        for name, offset, length, pfx in entries_info:
            entry = RpaEntry(name=name, offset=offset, length=length, prefix=pfx)
            data = reader.read_file(entry)
            group_original += len(data)

            try:
                img = Image.open(BytesIO(data))
                if img.mode != 'RGBA':
                    img = img.convert('RGBA')
                w, h = img.size

                if group_w is None:
                    group_w, group_h = w, h
                elif (w, h) != (group_w, group_h):
                    return ("avis_fail", prefix, entries_info, group_original,
                            f"resolution mismatch ({w}x{h} vs {group_w}x{group_h})")

                frames_rgba.append((img.tobytes(), w, h))
            except Exception as e:
                return ("avis_fail", prefix, entries_info, group_original,
                        f"decode error: {e}")

    try:
        avis_data = encode_avis(frames_rgba, quality=quality, speed=speed)
    except Exception as e:
        return ("avis_fail", prefix, entries_info, group_original,
                f"encode error: {e}")

    safe_prefix = prefix.replace('/', '_').replace(' ', '_').strip('_')
    avis_name = f"sequences/{safe_prefix}.avis"
    names = [info[0] for info in entries_info]
    return ("avis", prefix, avis_name, avis_data, names, group_original)


# --- Main build logic ---

def build(game_dir: Path, output_dir: Path, limit: int = 0, quality: int = 50,
          workers: int = 0):
    """Build compressed RPA archive with AVIF-encoded images.

    Args:
        game_dir: Path to game directory containing .rpa files
        output_dir: Output directory for compressed files
        limit: Max images to encode (0 = all)
        quality: AVIF quality 1-63
        workers: Number of parallel workers (0 = cpu_count)
    """
    game_dir = Path(game_dir)
    output_dir = Path(output_dir)
    if workers <= 0:
        workers = os.cpu_count() or 4

    rpa_files = sorted(game_dir.glob("*.rpa"))
    if not rpa_files:
        rpa_files = sorted((game_dir / "game").glob("*.rpa"))
    if not rpa_files:
        print(f"No .rpa files found in {game_dir}")
        return

    for rpa_path in rpa_files:
        _build_rpa(rpa_path, output_dir, limit, quality, workers)

    _copy_runtime(output_dir)


def _build_rpa(rpa_path: Path, output_dir: Path, limit: int, quality: int,
               workers: int):
    """Process a single RPA file with parallel encoding."""
    print(f"\n=== Processing {rpa_path.name} ===")
    start_time = time.time()

    out_game_dir = output_dir / "game"
    out_game_dir.mkdir(parents=True, exist_ok=True)
    out_rpa = out_game_dir / rpa_path.name

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        print(f"  Entries: {len(index)}")

        images = {n: e for n, e in index.items() if should_encode(n)}
        others = {n: e for n, e in index.items() if not should_encode(n)}
        print(f"  Images: {len(images)}, Other: {len(others)}")

        if limit > 0:
            image_names = sorted(images.keys())[:limit]
            for name in sorted(images.keys())[limit:]:
                others[name] = images[name]
            images = {n: images[n] for n in image_names}
            print(f"  Encoding {len(images)} images (limit={limit})")

        seq_groups, ungrouped_names = group_by_prefix(list(images.keys()))
        seq_count = sum(len(v) for v in seq_groups.values())
        print(f"  Sequences: {len(seq_groups)} groups ({seq_count} images), "
              f"Scatter: {len(ungrouped_names)}")

        # Check AVIS availability
        avis_available = True
        try:
            from renpak.encode import _load_core_lib
            _load_core_lib()
        except (FileNotFoundError, OSError) as e:
            avis_available = False
            print(f"  WARNING: AVIS unavailable ({e}), AVIF-only")
            for names in seq_groups.values():
                ungrouped_names.extend(names)
            seq_groups = {}

        # --- Submit all encoding tasks ---
        rpa_str = str(rpa_path)
        total_tasks = (len(seq_groups) if avis_available else 0) + len(ungrouped_names)
        print(f"  Encoding with {workers} workers, {total_tasks} tasks...")
        t_encode_start = time.time()

        manifest = {}
        original_size = 0
        compressed_size = 0
        encoded_count = 0
        encoded_results = []  # [(archive_name, data)]
        fallback_to_avif = []  # names that failed AVIS

        def _collect_result(result):
            """Process a single worker result. Returns True if ok."""
            nonlocal original_size, compressed_size, encoded_count
            if result[0] == "avis":
                _, prefix, avis_name, avis_data, names, group_orig = result
                encoded_results.append((avis_name, avis_data))
                original_size += group_orig
                compressed_size += len(avis_data)
                for frame_idx, name in enumerate(names):
                    manifest[name] = {"avis": avis_name, "frame": frame_idx}
                    encoded_count += 1
                ratio = group_orig / len(avis_data) if avis_data else 0
                return True, f"AVIS {prefix}* ({len(names)}f) {ratio:.1f}x"
            elif result[0] == "avis_fail":
                _, prefix, entries_info, group_orig, err = result
                fallback_to_avif.extend(info[0] for info in entries_info)
                return False, f"AVIS FAIL {prefix}*: {err}"
            elif result[0] == "avif":
                _, name, avif_name, avif_data, orig_len, _ = result
                encoded_results.append((avif_name, avif_data))
                manifest[name] = avif_name
                original_size += orig_len
                compressed_size += len(avif_data)
                encoded_count += 1
                return True, None
            elif result[0] == "avif_fail":
                _, name, _, data, orig_len, err = result
                encoded_results.append((name, data))
                original_size += orig_len
                compressed_size += len(data)
                return False, f"SKIP {name}: {err}"
            return True, None

        done_count = 0

        def _progress(msg):
            """Print progress with elapsed time and ETA."""
            elapsed = time.time() - t_encode_start
            if done_count > 0 and done_count < total_tasks:
                eta = elapsed / done_count * (total_tasks - done_count)
                print(f"  [{done_count}/{total_tasks}] {elapsed:.0f}s "
                      f"(ETA {eta:.0f}s) {msg}")
            else:
                print(f"  [{done_count}/{total_tasks}] {elapsed:.0f}s {msg}")

        with ProcessPoolExecutor(max_workers=workers) as pool:
            # Phase 1: AVIS groups with memory-budgeted concurrency
            if avis_available and seq_groups:
                # Sort groups by frame count descending (big groups first)
                sorted_groups = sorted(seq_groups.items(),
                                       key=lambda kv: -len(kv[1]))
                pixel_count = _DEFAULT_RES[0] * _DEFAULT_RES[1]
                frame_mem = pixel_count * _RGBA_BPP * _FRAME_MEM_MULTIPLIER

                avis_mem_budget = _get_avis_mem_budget(workers)
                avis_futures = {}
                mem_in_flight = 0
                group_queue = list(sorted_groups)

                def _submit_avis_groups():
                    """Submit as many AVIS groups as memory budget allows."""
                    nonlocal mem_in_flight
                    while group_queue:
                        prefix, names = group_queue[0]
                        group_mem = len(names) * frame_mem
                        # Always allow at least 1 task even if over budget
                        if avis_futures and mem_in_flight + group_mem > avis_mem_budget:
                            break
                        group_queue.pop(0)
                        entries_info = [
                            (n, images[n].offset, images[n].length, images[n].prefix)
                            for n in names
                        ]
                        fut = pool.submit(_worker_encode_avis, rpa_str, prefix,
                                          entries_info, quality, 6)
                        avis_futures[fut] = (prefix, len(names), group_mem)
                        mem_in_flight += group_mem

                _submit_avis_groups()
                print(f"  AVIS phase: {len(seq_groups)} groups, "
                      f"mem budget {avis_mem_budget / (1024**3):.1f}GB, "
                      f"{len(avis_futures)} initial tasks")

                while avis_futures:
                    # Wait for any one to complete
                    for fut in as_completed(avis_futures):
                        prefix, nframes, group_mem = avis_futures.pop(fut)
                        mem_in_flight -= group_mem
                        done_count += 1
                        ok, msg = _collect_result(fut.result())
                        if msg:
                            _progress(msg)
                        # Submit more now that memory freed
                        _submit_avis_groups()
                        break  # re-enter as_completed with updated dict

            # Phase 2: Scatter AVIF (small memory, full parallelism)
            avif_futures = {}
            for name in ungrouped_names:
                if name not in images:
                    continue
                e = images[name]
                fut = pool.submit(_worker_encode_avif, rpa_str, name,
                                  e.offset, e.length, e.prefix, quality)
                avif_futures[fut] = name

            # Also handle AVIS fallbacks
            if fallback_to_avif:
                print(f"  Re-encoding {len(fallback_to_avif)} AVIS fallbacks as AVIF...")
                for name in fallback_to_avif:
                    if name not in images:
                        continue
                    e = images[name]
                    fut = pool.submit(_worker_encode_avif, rpa_str, name,
                                      e.offset, e.length, e.prefix, quality)
                    avif_futures[fut] = name

            avif_total = len(avif_futures)
            avif_done = 0
            for fut in as_completed(avif_futures):
                done_count += 1
                avif_done += 1
                ok, msg = _collect_result(fut.result())
                if msg:
                    _progress(msg)
                elif avif_done % 50 == 0:
                    _progress(f"AVIF {avif_done}/{avif_total}...")

        t_encode_elapsed = time.time() - t_encode_start
        print(f"  Encoding done: {t_encode_elapsed:.1f}s")

        # --- Write phase (serial) ---
        t_write_start = time.time()
        with RpaWriter(out_rpa) as writer:
            # Write encoded images
            for archive_name, data in encoded_results:
                writer.add_file(archive_name, data)

            # Copy non-images
            for i, (name, entry) in enumerate(others.items()):
                data = reader.read_file(entry)
                writer.add_file(name, data)
                if (i + 1) % 2000 == 0:
                    print(f"  Copying non-images: {i+1}/{len(others)}")

            # Write manifest
            manifest_json = json.dumps(manifest, ensure_ascii=False,
                                       indent=None).encode('utf-8')
            writer.add_file("renpak_manifest.json", manifest_json)
            print(f"  Manifest: {len(manifest)} entries ({len(manifest_json)} bytes)")

        t_write_elapsed = time.time() - t_write_start

    elapsed = time.time() - start_time
    if original_size > 0:
        ratio = original_size / compressed_size if compressed_size > 0 else 0
        print(f"\n  Images: {original_size / 1024 / 1024:.1f} MB -> "
              f"{compressed_size / 1024 / 1024:.1f} MB ({ratio:.1f}x)")
    print(f"  Encode: {t_encode_elapsed:.1f}s, Write: {t_write_elapsed:.1f}s, "
          f"Total: {elapsed:.1f}s")
    print(f"  Output: {out_rpa}")


def _copy_runtime(output_dir: Path):
    """Copy runtime plugin files to output game directory."""
    runtime_src = Path(__file__).parent.parent / "runtime"
    game_dir = output_dir / "game"
    game_dir.mkdir(parents=True, exist_ok=True)

    for fname in ("renpak_init.rpy", "renpak_loader.py"):
        src = runtime_src / fname
        if src.exists():
            shutil.copy2(src, game_dir / fname)
            print(f"  Copied {fname} -> {game_dir / fname}")
        else:
            print(f"  WARNING: {src} not found")

    rt_candidates = [
        Path(__file__).parent.parent.parent / "target" / "release" / "librenpak_rt.so",
        Path(__file__).parent / "librenpak_rt.so",
    ]
    for rt_path in rt_candidates:
        if rt_path.exists():
            shutil.copy2(rt_path, game_dir / "librenpak_rt.so")
            print(f"  Copied librenpak_rt.so -> {game_dir / 'librenpak_rt.so'}")
            break
    else:
        print("  WARNING: librenpak_rt.so not found, AVIS decoding unavailable")


def analyze(game_dir: Path):
    """Analyze RPA contents without encoding."""
    game_dir = Path(game_dir)
    rpa_files = sorted(game_dir.glob("*.rpa"))
    if not rpa_files:
        rpa_files = sorted((game_dir / "game").glob("*.rpa"))
    if not rpa_files:
        print(f"No .rpa files found in {game_dir}")
        return

    for rpa_path in rpa_files:
        print(f"\n=== {rpa_path.name} ({rpa_path.stat().st_size / 1024 / 1024:.1f} MB) ===")
        with RpaReader(rpa_path) as reader:
            index = reader.read_index()

            by_ext = defaultdict(lambda: {"count": 0, "names": []})
            for name in sorted(index.keys()):
                ext = Path(name).suffix.lower() or "(no ext)"
                by_ext[ext]["count"] += 1
                if len(by_ext[ext]["names"]) < 3:
                    by_ext[ext]["names"].append(name)

            print(f"  Total entries: {len(index)}")
            print(f"  {'Extension':<12} {'Count':>8}  Examples")
            print(f"  {'-'*12} {'-'*8}  {'-'*40}")
            for ext in sorted(by_ext.keys(), key=lambda e: -by_ext[e]["count"]):
                info = by_ext[ext]
                example = info["names"][0] if info["names"] else ""
                print(f"  {ext:<12} {info['count']:>8}  {example}")


def info(rpa_path: Path):
    """Show RPA index information."""
    rpa_path = Path(rpa_path)
    print(f"=== {rpa_path.name} ===")
    print(f"Size: {rpa_path.stat().st_size / 1024 / 1024:.1f} MB")

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        print(f"Entries: {len(index)}")
        print(f"\n{'Name':<60} {'Offset':>12} {'Length':>12}")
        print(f"{'-'*60} {'-'*12} {'-'*12}")
        for name in sorted(index.keys())[:50]:
            entry = index[name]
            print(f"{name:<60} {entry.offset:>12} {entry.length:>12}")
        if len(index) > 50:
            print(f"  ... and {len(index) - 50} more entries")
