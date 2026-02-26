#!/usr/bin/env python3
"""Analyze renpak compression: compare original vs compressed RPA images."""

import zlib
import pickle
import io
import json
import random
import sys
import os

import numpy as np
from PIL import Image
from skimage.metrics import structural_similarity as ssim

ORIG_RPA = "/home/spencer/Games/Eternum-0.9.5-pc/game/.renpak_backup/archive_0.09.05.rpa"
COMP_RPA = "/home/spencer/Games/Eternum-0.9.5-pc/game/archive_0.09.05.rpa"
SAMPLE_SIZE = 100


def read_rpa_index(path):
    with open(path, "rb") as f:
        header = f.readline().decode("utf-8", errors="replace").strip().split()
        assert header[0] == "RPA-3.0", f"Not RPA-3.0: {path}"
        index_offset = int(header[1], 16)
        key = int(header[2], 16)
        f.seek(index_offset)
        raw = pickle.loads(zlib.decompress(f.read()))

    entries = {}
    for name, tuples in raw.items():
        if not tuples:
            continue
        t = tuples[0]
        offset = t[0] ^ key
        length = t[1] ^ key
        prefix = t[2] if len(t) >= 3 and isinstance(t[2], bytes) else b""
        entries[name] = (offset, length, prefix)
    return entries


def extract(fh, offset, length, prefix):
    fh.seek(offset)
    data = fh.read(length)
    if prefix:
        data = prefix + data[len(prefix):]
    return data


def decode(data):
    img = Image.open(io.BytesIO(data))
    return img.convert("RGB")


def main():
    print("Reading indexes...")
    orig_idx = read_rpa_index(ORIG_RPA)
    comp_idx = read_rpa_index(COMP_RPA)
    print(f"  Original: {len(orig_idx)} files")
    print(f"  Compressed: {len(comp_idx)} files")

    # manifest
    mdata = None
    with open(COMP_RPA, "rb") as f:
        o, l, p = comp_idx["renpak_manifest.json"]
        mdata = extract(f, o, l, p)
    manifest = json.loads(mdata)
    print(f"  Manifest: {len(manifest)} mapped entries")

    # classify
    img_exts = {".png", ".webp", ".jpg", ".jpeg"}
    images = [n for n in manifest if os.path.splitext(n)[1].lower() in img_exts]
    print(f"  Image files: {len(images)}")

    # count by type
    by_ext = {}
    for n in images:
        ext = os.path.splitext(n)[1].lower()
        by_ext[ext] = by_ext.get(ext, 0) + 1
    print(f"  By type: {by_ext}")

    # total image data sizes
    orig_img_bytes = sum(orig_idx[n][1] for n in images if n in orig_idx)
    comp_img_bytes = sum(comp_idx[manifest[n]][1] for n in images if manifest[n] in comp_idx)
    print(f"  Original image data: {orig_img_bytes / 1e9:.2f} GB")
    print(f"  Compressed image data: {comp_img_bytes / 1e9:.2f} GB")
    print(f"  Image compression ratio: {comp_img_bytes / orig_img_bytes:.3f}")

    # sample for SSIM
    sample = random.sample(images, min(SAMPLE_SIZE, len(images)))
    print(f"\nComputing SSIM on {len(sample)} samples...")

    ssim_scores = []
    errors = 0
    orig_fh = open(ORIG_RPA, "rb")
    comp_fh = open(COMP_RPA, "rb")

    for i, orig_name in enumerate(sample):
        avif_name = manifest[orig_name]
        try:
            orig_data = extract(orig_fh, *orig_idx[orig_name])
            comp_data = extract(comp_fh, *comp_idx[avif_name])

            orig_img = decode(orig_data)
            comp_img = decode(comp_data)

            # crop AVIF (padded to 8x) back to original size
            ow, oh = orig_img.size
            if comp_img.size != orig_img.size:
                comp_img = comp_img.crop((0, 0, ow, oh))

            orig_arr = np.array(orig_img)
            comp_arr = np.array(comp_img)

            score = ssim(orig_arr, comp_arr, channel_axis=2, data_range=255)
            ssim_scores.append(score)

            if (i + 1) % 20 == 0:
                print(f"  [{i+1}/{len(sample)}] mean SSIM so far: {np.mean(ssim_scores):.4f}")
        except Exception as e:
            errors += 1
            if errors <= 5:
                print(f"  Error on {orig_name}: {e}")

    orig_fh.close()
    comp_fh.close()

    print(f"\n{'='*50}")
    print(f"SSIM Results ({len(ssim_scores)} samples, {errors} errors):")
    if ssim_scores:
        arr = np.array(ssim_scores)
        print(f"  Mean:   {arr.mean():.4f}")
        print(f"  Std:    {arr.std():.4f}")
        print(f"  Min:    {arr.min():.4f}")
        print(f"  Max:    {arr.max():.4f}")
        print(f"  Median: {np.median(arr):.4f}")

    # summary JSON
    summary = {
        "orig_rpa_bytes": os.path.getsize(ORIG_RPA),
        "comp_rpa_bytes": os.path.getsize(COMP_RPA),
        "total_files_orig": len(orig_idx),
        "total_files_comp": len(comp_idx),
        "image_count": len(images),
        "image_types": by_ext,
        "orig_image_bytes": orig_img_bytes,
        "comp_image_bytes": comp_img_bytes,
        "ssim_mean": float(arr.mean()) if ssim_scores else None,
        "ssim_std": float(arr.std()) if ssim_scores else None,
        "ssim_min": float(arr.min()) if ssim_scores else None,
        "ssim_max": float(arr.max()) if ssim_scores else None,
        "ssim_median": float(np.median(arr)) if ssim_scores else None,
    }
    print(f"\n{json.dumps(summary, indent=2)}")


if __name__ == "__main__":
    random.seed(42)
    main()
