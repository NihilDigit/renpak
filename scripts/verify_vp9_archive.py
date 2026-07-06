#!/usr/bin/env python3
"""Verify a renpak VP9 archive manifest against its RPA index."""

from __future__ import annotations

import argparse
import json
import pickle
import zlib
from pathlib import Path
from typing import Any


MANIFEST_PATH = "renpak_manifest.json"


def read_rpa_index(path: Path) -> set[str]:
    with path.open("rb") as handle:
        header = handle.readline().decode("ascii", errors="replace")
        parts = header.split()
        if len(parts) < 3 or parts[0] != "RPA-3.0":
            raise SystemExit(f"{path}: unsupported RPA header: {header.strip()}")

        index_offset = int(parts[1], 16)
        handle.seek(index_offset)
        raw_index = pickle.loads(zlib.decompress(handle.read()), encoding="bytes")

    names = set()
    for key in raw_index.keys():
        if isinstance(key, bytes):
            names.add(key.decode("utf-8"))
        else:
            names.add(str(key))
    return names


def read_manifest_from_rpa(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        header = handle.readline().decode("ascii", errors="replace")
        parts = header.split()
        index_offset = int(parts[1], 16)
        key = 0
        for part in parts[2:]:
            key ^= int(part, 16)

        handle.seek(index_offset)
        raw_index = pickle.loads(zlib.decompress(handle.read()), encoding="bytes")

        entry = raw_index.get(MANIFEST_PATH)
        if entry is None:
            entry = raw_index.get(MANIFEST_PATH.encode("utf-8"))
        if entry is None:
            raise SystemExit(f"{path}: missing {MANIFEST_PATH}")

        offset, length = entry[0][0] ^ key, entry[0][1] ^ key
        prefix = entry[0][2] if len(entry[0]) > 2 else b""
        if isinstance(prefix, list):
            prefix = bytes(prefix)
        handle.seek(offset)
        data = prefix + handle.read(length)

    return json.loads(data.decode("utf-8"))


def validate_manifest(raw: dict[str, Any]) -> dict[str, Any]:
    if raw.get("version") != 2:
        raise SystemExit("manifest is not version 2")

    assets = raw.get("assets")
    if not isinstance(assets, dict):
        raise SystemExit("manifest.assets is missing or not an object")

    return assets


def expected_bundle_targets(assets: dict[str, Any]) -> set[str]:
    targets = set()
    for original, entry in assets.items():
        if not isinstance(entry, dict):
            raise SystemExit(f"{original}: manifest entry is not an object")
        target = entry.get("target")
        if not isinstance(target, str) or not target:
            raise SystemExit(f"{original}: missing target")
        if entry.get("mode") == "vp9_bundle_frame":
            targets.add(target)
    return targets


def verify_android_assets(assets_dir: Path, archive_manifest: dict[str, Any]) -> None:
    manifest_path = assets_dir / MANIFEST_PATH
    bundle_root = assets_dir / "renpak" / "bundles"

    if not manifest_path.is_file():
        raise SystemExit(f"missing Android assets manifest: {manifest_path}")
    if not bundle_root.is_dir():
        raise SystemExit(f"missing Android bundle directory: {bundle_root}")

    android_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if android_manifest != archive_manifest:
        raise SystemExit("Android assets manifest does not match archive manifest")

    assets = validate_manifest(android_manifest)
    expected = expected_bundle_targets(assets)
    actual = {
        path.relative_to(assets_dir).as_posix()
        for path in bundle_root.rglob("*.webm")
        if path.is_file()
    }

    missing = expected - actual
    extra = actual - expected
    if missing:
        sample = ", ".join(sorted(missing)[:10])
        raise SystemExit(f"missing Android VP9 bundles: {sample}")
    if extra:
        sample = ", ".join(sorted(extra)[:10])
        raise SystemExit(f"extra Android VP9 bundles: {sample}")

    bundle_bytes = sum((assets_dir / target).stat().st_size for target in actual)
    print(
        f"ok android_assets={assets_dir} bundles={len(actual)} "
        f"bytes={bundle_bytes}"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument(
        "--android-assets-dir",
        type=Path,
        help="Also verify loose Android manifest and VP9 bundles produced by build-vp9.",
    )
    parser.add_argument(
        "--expect-stripped",
        action="store_true",
        help="Fail if any manifest original asset is still present in the archive.",
    )
    args = parser.parse_args()

    names = read_rpa_index(args.archive)
    manifest = read_manifest_from_rpa(args.archive)
    assets = validate_manifest(manifest)

    missing_targets = []
    kept_originals = []
    modes: dict[str, int] = {}
    targets: set[str] = set()

    for original, entry in assets.items():
        if not isinstance(entry, dict):
            raise SystemExit(f"{original}: manifest entry is not an object")

        mode = entry.get("mode")
        if not isinstance(mode, str):
            raise SystemExit(f"{original}: missing mode")
        modes[mode] = modes.get(mode, 0) + 1

        target = entry.get("target")
        if not isinstance(target, str) or not target:
            raise SystemExit(f"{original}: missing target")
        targets.add(target)

        if target not in names:
            missing_targets.append(target)
        if args.expect_stripped and original in names:
            kept_originals.append(original)

    if missing_targets:
        sample = ", ".join(sorted(set(missing_targets))[:10])
        raise SystemExit(f"missing manifest targets: {sample}")
    if kept_originals:
        sample = ", ".join(sorted(kept_originals)[:10])
        raise SystemExit(f"manifest originals still present: {sample}")

    mode_summary = ", ".join(f"{key}={value}" for key, value in sorted(modes.items()))
    print(
        f"ok archive={args.archive} entries={len(names)} "
        f"assets={len(assets)} targets={len(targets)} {mode_summary}"
    )
    if args.android_assets_dir is not None:
        verify_android_assets(args.android_assets_dir, manifest)


if __name__ == "__main__":
    main()
