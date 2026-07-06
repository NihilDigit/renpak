#!/usr/bin/env python3
"""Run a mobile VP9 video profile experiment against videos stored in an RPA."""

from __future__ import annotations

import argparse
import csv
import json
import pickle
import shutil
import subprocess
import time
import zlib
from pathlib import Path
from typing import Any


VIDEO_EXTS = (".webm", ".mp4", ".mkv", ".avi", ".mov")


def read_rpa_index(path: Path) -> tuple[int, list[dict[str, Any]]]:
    with path.open("rb") as handle:
        header = handle.readline().decode("ascii", errors="replace")
        parts = header.split()
        if len(parts) < 3 or parts[0] != "RPA-3.0":
            raise SystemExit(f"{path}: unsupported RPA header: {header.strip()}")

        index_offset = int(parts[1], 16)
        key = 0
        for part in parts[2:]:
            key ^= int(part, 16)

        handle.seek(index_offset)
        raw_index = pickle.loads(zlib.decompress(handle.read()), encoding="bytes")

    entries = []
    for raw_name, raw_entry in raw_index.items():
        name = raw_name.decode("utf-8") if isinstance(raw_name, bytes) else str(raw_name)
        offset, length = raw_entry[0][0] ^ key, raw_entry[0][1] ^ key
        prefix = raw_entry[0][2] if len(raw_entry[0]) > 2 else b""
        if isinstance(prefix, list):
            prefix = bytes(prefix)
        entries.append({
            "name": name,
            "offset": offset,
            "length": length,
            "prefix": prefix,
            "size": length + len(prefix),
        })
    return key, entries


def extract_entry(rpa: Path, entry: dict[str, Any], output: Path) -> None:
    with rpa.open("rb") as handle:
        handle.seek(entry["offset"])
        data = entry["prefix"] + handle.read(entry["length"])
    output.write_bytes(data)


def run_json(cmd: list[str]) -> dict[str, Any]:
    output = subprocess.check_output(cmd, text=True)
    return json.loads(output)


def probe_video(path: Path) -> dict[str, Any]:
    data = run_json([
        "ffprobe",
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,r_frame_rate,avg_frame_rate,duration,bit_rate",
        "-of",
        "json",
        str(path),
    ])
    streams = data.get("streams") or []
    return streams[0] if streams else {}


def encode_mobile_vp9(input_path: Path, output_path: Path, max_height: int, crf: int) -> float:
    scale = f"scale=-2:min({max_height}\\,ih):flags=lanczos"
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(input_path),
        "-vf",
        scale,
        "-c:v",
        "libvpx-vp9",
        "-deadline",
        "realtime",
        "-cpu-used",
        "8",
        "-row-mt",
        "1",
        "-crf",
        str(crf),
        "-b:v",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "copy",
        str(output_path),
    ]
    start = time.monotonic()
    subprocess.run(cmd, check=True)
    return time.monotonic() - start


def pick_videos(entries: list[dict[str, Any]], limit: int) -> list[dict[str, Any]]:
    videos = [
        entry
        for entry in entries
        if entry["name"].lower().endswith(VIDEO_EXTS)
    ]
    videos.sort(key=lambda entry: (-entry["size"], entry["name"]))
    return videos[:limit]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument("--max-height", type=int, default=720)
    parser.add_argument("--crf", type=int, default=38)
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    work_dir = args.output_dir / "work"
    encoded_dir = args.output_dir / "encoded"
    work_dir.mkdir(parents=True, exist_ok=True)
    encoded_dir.mkdir(parents=True, exist_ok=True)

    _key, entries = read_rpa_index(args.archive)
    videos = pick_videos(entries, args.limit)
    rows = []
    for index, entry in enumerate(videos, start=1):
        suffix = Path(entry["name"]).suffix or ".webm"
        input_path = work_dir / f"input_{index:03}{suffix}"
        output_path = encoded_dir / f"video_{index:03}.webm"
        print(f"[{index}/{len(videos)}] {entry['name']} ({entry['size']} bytes)")

        extract_entry(args.archive, entry, input_path)
        source_probe = probe_video(input_path)
        elapsed = encode_mobile_vp9(input_path, output_path, args.max_height, args.crf)
        output_probe = probe_video(output_path)
        original_size = entry["size"]
        encoded_size = output_path.stat().st_size
        saved = 1.0 - (encoded_size / original_size)
        rows.append({
            "name": entry["name"],
            "original_size": original_size,
            "encoded_size": encoded_size,
            "saved_percent": round(saved * 100, 2),
            "encode_seconds": round(elapsed, 2),
            "source_width": source_probe.get("width"),
            "source_height": source_probe.get("height"),
            "source_fps": source_probe.get("avg_frame_rate") or source_probe.get("r_frame_rate"),
            "source_duration": source_probe.get("duration"),
            "output_width": output_probe.get("width"),
            "output_height": output_probe.get("height"),
            "output_fps": output_probe.get("avg_frame_rate") or output_probe.get("r_frame_rate"),
            "output_duration": output_probe.get("duration"),
        })
        input_path.unlink(missing_ok=True)
        print(f"    -> {encoded_size} bytes, saved {saved * 100:.1f}%, {elapsed:.1f}s")

    csv_path = args.output_dir / "results.csv"
    json_path = args.output_dir / "results.json"
    if rows:
        with csv_path.open("w", newline="", encoding="utf-8") as handle:
            writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
            writer.writeheader()
            writer.writerows(rows)
    json_path.write_text(json.dumps(rows, indent=2), encoding="utf-8")

    original_total = sum(row["original_size"] for row in rows)
    encoded_total = sum(row["encoded_size"] for row in rows)
    print(f"results: {csv_path}")
    print(f"json: {json_path}")
    if original_total:
        print(
            f"total: {original_total} -> {encoded_total} bytes, "
            f"saved {(1.0 - encoded_total / original_total) * 100:.2f}%"
        )


if __name__ == "__main__":
    main()
