#!/usr/bin/env python3
"""Run an Opus audio profile experiment against audio stored in an RPA."""

from __future__ import annotations

import argparse
import csv
import json
import pickle
import subprocess
import time
import zlib
from pathlib import Path
from typing import Any


AUDIO_EXTS = (".ogg", ".mp3", ".wav", ".flac", ".m4a", ".aac", ".opus")


def read_rpa_index(path: Path) -> list[dict[str, Any]]:
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
    return entries


def extract_entry(rpa: Path, entry: dict[str, Any], output: Path) -> None:
    with rpa.open("rb") as handle:
        handle.seek(entry["offset"])
        data = entry["prefix"] + handle.read(entry["length"])
    output.write_bytes(data)


def run_json(cmd: list[str]) -> dict[str, Any]:
    output = subprocess.check_output(cmd, text=True)
    return json.loads(output)


def probe_audio(path: Path) -> dict[str, Any]:
    data = run_json([
        "ffprobe",
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=codec_name,channels,sample_rate,duration,bit_rate",
        "-of",
        "json",
        str(path),
    ])
    streams = data.get("streams") or []
    return streams[0] if streams else {}


def encode_opus(input_path: Path, output_path: Path, bitrate: str) -> float:
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(input_path),
        "-vn",
        "-c:a",
        "libopus",
        "-b:a",
        bitrate,
        "-vbr",
        "on",
        str(output_path),
    ]
    start = time.monotonic()
    subprocess.run(cmd, check=True)
    return time.monotonic() - start


def pick_audio(entries: list[dict[str, Any]], limit: int | None) -> list[dict[str, Any]]:
    audio = [entry for entry in entries if entry["name"].lower().endswith(AUDIO_EXTS)]
    audio.sort(key=lambda entry: (-entry["size"], entry["name"]))
    return audio if limit is None else audio[:limit]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--bitrate", default="128k")
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    work_dir = args.output_dir / "work"
    encoded_dir = args.output_dir / "encoded"
    work_dir.mkdir(parents=True, exist_ok=True)
    encoded_dir.mkdir(parents=True, exist_ok=True)

    entries = read_rpa_index(args.archive)
    audio_entries = pick_audio(entries, args.limit)
    rows = []
    for index, entry in enumerate(audio_entries, start=1):
        suffix = Path(entry["name"]).suffix or ".bin"
        input_path = work_dir / f"input_{index:04}{suffix}"
        output_path = encoded_dir / f"audio_{index:04}.ogg"
        print(f"[{index}/{len(audio_entries)}] {entry['name']} ({entry['size']} bytes)")

        extract_entry(args.archive, entry, input_path)
        source_probe = probe_audio(input_path)
        elapsed = encode_opus(input_path, output_path, args.bitrate)
        output_probe = probe_audio(output_path)
        original_size = entry["size"]
        encoded_size = output_path.stat().st_size
        saved = 1.0 - (encoded_size / original_size)
        rows.append({
            "name": entry["name"],
            "original_size": original_size,
            "encoded_size": encoded_size,
            "saved_percent": round(saved * 100, 2),
            "encode_seconds": round(elapsed, 2),
            "source_codec": source_probe.get("codec_name"),
            "source_channels": source_probe.get("channels"),
            "source_sample_rate": source_probe.get("sample_rate"),
            "source_duration": source_probe.get("duration"),
            "source_bit_rate": source_probe.get("bit_rate"),
            "output_codec": output_probe.get("codec_name"),
            "output_channels": output_probe.get("channels"),
            "output_sample_rate": output_probe.get("sample_rate"),
            "output_duration": output_probe.get("duration"),
            "output_bit_rate": output_probe.get("bit_rate"),
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
