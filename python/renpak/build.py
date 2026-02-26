import json
import shutil
import time
from io import BytesIO
from pathlib import Path
from collections import defaultdict

from PIL import Image

from renpak.rpa import RpaReader, RpaWriter
from renpak.encode import (
    is_image, should_encode, encode_avif, get_avif_name,
    group_by_prefix, encode_avis, SEQUENCE_THRESHOLD,
)


def build(game_dir: Path, output_dir: Path, limit: int = 0, quality: int = 50):
    """Build compressed RPA archive with AVIF-encoded images.

    Args:
        game_dir: Path to game directory containing .rpa files
        output_dir: Output directory for compressed files
        limit: Max images to encode (0 = all)
        quality: AVIF quality 1-63
    """
    game_dir = Path(game_dir)
    output_dir = Path(output_dir)

    # Find RPA files
    rpa_files = sorted(game_dir.glob("*.rpa"))
    if not rpa_files:
        # Also check game/ subdirectory
        rpa_files = sorted((game_dir / "game").glob("*.rpa"))
    if not rpa_files:
        print(f"No .rpa files found in {game_dir}")
        return

    for rpa_path in rpa_files:
        _build_rpa(rpa_path, output_dir, limit, quality)

    # Copy runtime files
    _copy_runtime(output_dir)


def _build_rpa(rpa_path: Path, output_dir: Path, limit: int, quality: int):
    """Process a single RPA file."""
    print(f"\n=== Processing {rpa_path.name} ===")
    start_time = time.time()

    # Determine output path - put RPA in output_dir/game/ with same name
    out_game_dir = output_dir / "game"
    out_game_dir.mkdir(parents=True, exist_ok=True)
    out_rpa = out_game_dir / rpa_path.name

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        print(f"  Entries: {len(index)}")

        # Classify
        images = {n: e for n, e in index.items() if should_encode(n)}
        others = {n: e for n, e in index.items() if not should_encode(n)}
        print(f"  Images: {len(images)}, Other: {len(others)}")

        # Apply limit
        if limit > 0:
            image_names = sorted(images.keys())[:limit]
            for name in sorted(images.keys())[limit:]:
                others[name] = images[name]
            images = {n: images[n] for n in image_names}
            print(f"  Encoding {len(images)} images (limit={limit})")

        # Group images by prefix for AVIS sequences
        seq_groups, ungrouped_names = group_by_prefix(list(images.keys()))
        seq_count = sum(len(v) for v in seq_groups.values())
        print(f"  Sequences: {len(seq_groups)} groups ({seq_count} images), Scatter: {len(ungrouped_names)}")

        manifest = {}  # original_name -> avif_name or {"avis": path, "frame": idx}
        original_size = 0
        compressed_size = 0
        encoded_count = 0

        # Check if renpak-core is available for AVIS encoding
        avis_available = True
        try:
            from renpak.encode import _load_core_lib
            _load_core_lib()
        except (FileNotFoundError, OSError) as e:
            avis_available = False
            print(f"  WARNING: AVIS encoding unavailable ({e}), falling back to AVIF-only")
            # Move all sequence images back to ungrouped
            for names in seq_groups.values():
                ungrouped_names.extend(names)
            seq_groups = {}

        with RpaWriter(out_rpa) as writer:
            # --- AVIS sequence encoding ---
            if avis_available:
                for prefix, names in seq_groups.items():
                    group_original = 0
                    frames_rgba = []
                    group_w, group_h = None, None
                    fallback = False

                    for name in names:
                        data = reader.read_file(images[name])
                        group_original += len(data)
                        original_size += len(data)

                        try:
                            img = Image.open(BytesIO(data))
                            if img.mode != 'RGBA':
                                img = img.convert('RGBA')
                            w, h = img.size

                            if group_w is None:
                                group_w, group_h = w, h
                            elif (w, h) != (group_w, group_h):
                                print(f"  AVIS fallback: {prefix}* — resolution mismatch ({w}x{h} vs {group_w}x{group_h})")
                                fallback = True
                                break

                            frames_rgba.append((img.tobytes(), w, h))
                        except Exception as e:
                            print(f"  AVIS fallback: {prefix}* — decode error: {e}")
                            fallback = True
                            break

                    if fallback:
                        # Fall back to per-image AVIF for this group
                        ungrouped_names.extend(names)
                        # Undo the original_size accounting (will be re-added in AVIF loop)
                        original_size -= group_original
                        continue

                    # Encode AVIS
                    try:
                        avis_data = encode_avis(frames_rgba, quality=quality, speed=6)
                    except Exception as e:
                        print(f"  AVIS fallback: {prefix}* — encode error: {e}")
                        ungrouped_names.extend(names)
                        original_size -= group_original
                        continue

                    # Generate AVIS archive path from prefix
                    safe_prefix = prefix.replace('/', '_').replace(' ', '_').strip('_')
                    avis_name = f"sequences/{safe_prefix}.avis"
                    writer.add_file(avis_name, avis_data)
                    compressed_size += len(avis_data)

                    for frame_idx, name in enumerate(names):
                        manifest[name] = {"avis": avis_name, "frame": frame_idx}
                        encoded_count += 1

                    ratio = group_original / len(avis_data) if avis_data else 0
                    print(f"  [AVIS] {prefix}* ({len(names)} frames) -> {ratio:.1f}x ({group_original} -> {len(avis_data)})")

            # --- Scatter AVIF encoding ---
            for name in ungrouped_names:
                if name not in images:
                    continue
                entry = images[name]
                data = reader.read_file(entry)
                original_size += len(data)

                try:
                    avif_data = encode_avif(data, quality=quality)
                    avif_name = get_avif_name(name)
                    writer.add_file(avif_name, avif_data)
                    manifest[name] = avif_name
                    compressed_size += len(avif_data)
                    encoded_count += 1

                    ratio = len(data) / len(avif_data) if avif_data else 0
                    print(f"  [{encoded_count}/{len(images)}] {name} -> {ratio:.1f}x ({len(data)} -> {len(avif_data)})")
                except Exception as e:
                    print(f"  [{encoded_count}/{len(images)}] SKIP {name}: {e}")
                    writer.add_file(name, data)
                    compressed_size += len(data)

            # Process non-images (pass through)
            for i, (name, entry) in enumerate(others.items()):
                data = reader.read_file(entry)
                writer.add_file(name, data)
                if (i + 1) % 1000 == 0:
                    print(f"  Copying non-images: {i+1}/{len(others)}")

            # Write manifest
            manifest_json = json.dumps(manifest, ensure_ascii=False, indent=None).encode('utf-8')
            writer.add_file("renpak_manifest.json", manifest_json)
            print(f"  Manifest: {len(manifest)} entries ({len(manifest_json)} bytes)")

    elapsed = time.time() - start_time
    if original_size > 0:
        overall_ratio = original_size / compressed_size if compressed_size > 0 else 0
        print(f"\n  Images: {original_size / 1024 / 1024:.1f} MB -> {compressed_size / 1024 / 1024:.1f} MB ({overall_ratio:.1f}x)")
    print(f"  Output: {out_rpa}")
    print(f"  Time: {elapsed:.1f}s")


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

    # Copy librenpak_rt.so if available
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
        print("  WARNING: librenpak_rt.so not found, AVIS decoding will be unavailable at runtime")


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

            # Group by extension
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
