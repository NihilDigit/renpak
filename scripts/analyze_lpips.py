#!/usr/bin/env python3
"""Compute LPIPS between original and compressed RPA images."""

import zlib
import pickle
import io
import json
import random
import os

import numpy as np
from PIL import Image
import torch
import lpips

ORIG_RPA = "/home/spencer/Games/Eternum-0.9.5-pc/game/.renpak_backup/archive_0.09.05.rpa"
COMP_RPA = "/home/spencer/Games/Eternum-0.9.5-pc/game/archive_0.09.05.rpa"
SAMPLE_SIZE = 50  # smaller sample, LPIPS is slower


def read_rpa_index(path):
    with open(path, "rb") as f:
        header = f.readline().decode("utf-8", errors="replace").strip().split()
        assert header[0] == "RPA-3.0"
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


def to_tensor(img):
    """Convert PIL Image to LPIPS-compatible tensor: [1, 3, H, W] in [-1, 1]."""
    arr = np.array(img.convert("RGB")).astype(np.float32) / 255.0
    t = torch.from_numpy(arr).permute(2, 0, 1).unsqueeze(0)
    return t * 2.0 - 1.0  # [0,1] -> [-1,1]


def main():
    print("Loading LPIPS model (alex)...")
    loss_fn = lpips.LPIPS(net="alex", verbose=False)

    print("Reading indexes...")
    orig_idx = read_rpa_index(ORIG_RPA)
    comp_idx = read_rpa_index(COMP_RPA)

    with open(COMP_RPA, "rb") as f:
        o, l, p = comp_idx["renpak_manifest.json"]
        manifest = json.loads(extract(f, o, l, p))

    img_exts = {".png", ".webp", ".jpg", ".jpeg"}
    images = [n for n in manifest if os.path.splitext(n)[1].lower() in img_exts]
    sample = random.sample(images, min(SAMPLE_SIZE, len(images)))
    print(f"Computing LPIPS on {len(sample)} samples...")

    scores = []
    orig_fh = open(ORIG_RPA, "rb")
    comp_fh = open(COMP_RPA, "rb")

    with torch.no_grad():
        for i, orig_name in enumerate(sample):
            avif_name = manifest[orig_name]
            try:
                orig_data = extract(orig_fh, *orig_idx[orig_name])
                comp_data = extract(comp_fh, *comp_idx[avif_name])

                orig_img = Image.open(io.BytesIO(orig_data)).convert("RGB")
                comp_img = Image.open(io.BytesIO(comp_data)).convert("RGB")

                ow, oh = orig_img.size
                if comp_img.size != orig_img.size:
                    comp_img = comp_img.crop((0, 0, ow, oh))

                t0 = to_tensor(orig_img)
                t1 = to_tensor(comp_img)
                d = loss_fn(t0, t1).item()
                scores.append(d)

                if (i + 1) % 10 == 0:
                    print(f"  [{i+1}/{len(sample)}] mean LPIPS: {np.mean(scores):.4f}")
            except Exception as e:
                print(f"  Error: {e}")

    orig_fh.close()
    comp_fh.close()

    arr = np.array(scores)
    print(f"\nLPIPS Results ({len(scores)} samples):")
    print(f"  Mean:   {arr.mean():.4f}")
    print(f"  Std:    {arr.std():.4f}")
    print(f"  Min:    {arr.min():.4f}")
    print(f"  Max:    {arr.max():.4f}")
    print(f"  Median: {np.median(arr):.4f}")


if __name__ == "__main__":
    random.seed(42)
    main()
