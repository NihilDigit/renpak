#!/usr/bin/env python3
"""End-to-end Android VP9 smoke test for the renpak MVP path."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
import zipfile
from pathlib import Path


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def run(cmd: list[str], env: dict[str, str] | None = None) -> None:
    print("+", " ".join(cmd))
    subprocess.run(cmd, check=True, env=env)


def capture(cmd: list[str], env: dict[str, str] | None = None) -> str:
    print("+", " ".join(cmd))
    return subprocess.check_output(cmd, text=True, env=env)


def remove_dir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)


def build_assets(args: argparse.Namespace, archive: Path, assets_dir: Path, work_dir: Path) -> None:
    cmd = [
        str(args.renpak),
        "build-vp9",
        str(args.source_rpa),
        str(archive),
        "--limit",
        str(args.limit),
        "--bundle-size",
        str(args.bundle_size),
        "--strip-optimized",
        "--work-dir",
        str(work_dir),
        "--android-assets-dir",
        str(assets_dir),
    ]
    if args.allow_transparent:
        cmd.append("--allow-transparent")
    run(cmd)
    run([
        sys.executable,
        str(repo_root() / "scripts" / "verify_vp9_archive.py"),
        str(archive),
        "--android-assets-dir",
        str(assets_dir),
        "--expect-stripped",
    ])


def prepare_project(args: argparse.Namespace, assets_dir: Path) -> None:
    cmd = [
        sys.executable,
        str(repo_root() / "scripts" / "prepare_android_game.py"),
        str(args.project),
        "--android-assets-dir",
        str(assets_dir),
        "--prune-android-assets",
    ]
    run(cmd)


def build_apk(args: argparse.Namespace, apk: Path, build_dir: Path) -> None:
    env = os.environ.copy()
    env["ANDROID_HOME"] = str(args.android_home)
    if args.java_home is not None:
        env["JAVA_HOME"] = str(args.java_home)

    cmd = [
        sys.executable,
        str(repo_root() / "scripts" / "build_android_package.py"),
        str(args.project),
        "--renpy-sdk",
        str(args.renpy_sdk),
        "--build-dir",
        str(build_dir),
        "--android-home",
        str(args.android_home),
        "--output-apk",
        str(apk),
    ]
    if args.java_home is not None:
        cmd.extend(["--java-home", str(args.java_home)])
    if args.android_keystore is not None:
        cmd.extend(["--android-keystore", str(args.android_keystore)])
    if args.bundle_keystore is not None:
        cmd.extend(["--bundle-keystore", str(args.bundle_keystore)])
    if args.no_install:
        cmd.append("--no-install")
    if args.no_launch:
        cmd.append("--no-launch")
    run(cmd, env=env)


def manifest_asset_count(manifest_path: Path) -> int:
    with manifest_path.open("r", encoding="utf-8") as f:
        manifest = json.load(f)
    assets = manifest.get("assets")
    if not isinstance(assets, dict):
        raise SystemExit(f"{manifest_path}: missing v2 assets object")
    return len(assets)


def verify_apk_assets(apk: Path, expected_bundle_count: int) -> None:
    with zipfile.ZipFile(apk) as zf:
        names = set(zf.namelist())
    manifest_names = [name for name in names if name.endswith("x-renpak_manifest.json")]
    bundle_names = sorted(
        name for name in names
        if "x-renpak/x-bundles/" in name and name.endswith(".webm")
    )
    if len(manifest_names) != 1:
        raise SystemExit(f"expected one renpak manifest in APK, found {len(manifest_names)}")
    if len(bundle_names) != expected_bundle_count:
        raise SystemExit(
            f"expected {expected_bundle_count} VP9 bundles in APK, found {len(bundle_names)}"
        )
    print(f"verified APK assets: manifest=1 bundles={len(bundle_names)}")


def verify_device_decode(args: argparse.Namespace, log_path: Path, screenshot_path: Path) -> None:
    adb = args.android_home / "platform-tools" / "adb"
    run([str(adb), "shell", "pm", "clear", args.package])
    run([str(adb), "logcat", "-c"])
    run([
        str(adb),
        "shell",
        "am",
        "start",
        "-W",
        "-a",
        "android.intent.action.MAIN",
        f"{args.package}/{args.activity}",
    ])
    time.sleep(args.device_wait_seconds)
    log = capture([str(adb), "logcat", "-d", "-v", "time"])
    log_path.write_text(log, encoding="utf-8")
    filtered = "\n".join(
        line for line in log.splitlines()
        if "RenpakRuntime" in line or "c2.qti.vp9" in line or "video/x-vnd.on2.vp9" in line
    )
    decoded_count = filtered.count("decoded frame=")
    hardware_count = filtered.count("c2.qti.vp9.decoder")
    if decoded_count == 0:
        raise SystemExit(f"no RenpakRuntime decoded frame found in {log_path}")
    if hardware_count == 0:
        raise SystemExit(f"no hardware VP9 decoder log found in {log_path}")
    screenshot_path.parent.mkdir(parents=True, exist_ok=True)
    with screenshot_path.open("wb") as f:
        subprocess.run([str(adb), "exec-out", "screencap", "-p"], check=True, stdout=f)
    print(
        "verified device decode: "
        f"decoded_frames={decoded_count} hardware_vp9_lines={hardware_count} "
        f"log={log_path} screenshot={screenshot_path}"
    )


def default_keystore(project: Path, name: str) -> Path | None:
    path = project / name
    return path if path.is_file() else None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-rpa", type=Path, required=True)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--renpy-sdk", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--renpak", type=Path, default=repo_root() / "target/release/renpak")
    parser.add_argument("--android-home", type=Path, default=Path(os.environ.get("ANDROID_HOME", "/opt/android-sdk")))
    parser.add_argument("--java-home", type=Path, default=Path(os.environ["JAVA_HOME"]) if os.environ.get("JAVA_HOME") else None)
    parser.add_argument("--limit", type=int, default=16)
    parser.add_argument("--bundle-size", type=int, default=8)
    parser.add_argument("--allow-transparent", action="store_true")
    parser.add_argument("--android-keystore", type=Path)
    parser.add_argument("--bundle-keystore", type=Path)
    parser.add_argument("--no-install", action="store_true")
    parser.add_argument("--no-launch", action="store_true")
    parser.add_argument("--skip-device-check", action="store_true")
    parser.add_argument("--package", default="org.renpak.prototype")
    parser.add_argument("--activity", default="org.renpy.android.PythonSDLActivity")
    parser.add_argument("--device-wait-seconds", type=float, default=4.0)
    args = parser.parse_args()

    args.source_rpa = args.source_rpa.resolve()
    args.project = args.project.resolve()
    args.renpy_sdk = args.renpy_sdk.resolve()
    args.output_dir = args.output_dir.resolve()
    args.renpak = args.renpak.resolve()
    args.android_home = args.android_home.resolve()
    args.java_home = args.java_home.resolve() if args.java_home else None
    args.android_keystore = (
        args.android_keystore.resolve()
        if args.android_keystore
        else default_keystore(args.project, "android.keystore")
    )
    args.bundle_keystore = (
        args.bundle_keystore.resolve()
        if args.bundle_keystore
        else default_keystore(args.project, "bundle.keystore")
    )
    return args


def main() -> None:
    args = parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)

    archive = args.output_dir / f"archive_vp9_limit{args.limit}.rpa"
    assets_dir = args.output_dir / "android-assets"
    work_dir = args.output_dir / "work"
    build_dir = args.output_dir / "android-build"
    apk = args.output_dir / f"renpak-vp9-limit{args.limit}.apk"
    log_path = args.output_dir / "device-logcat.txt"
    screenshot_path = args.output_dir / "device-screenshot.png"

    remove_dir(assets_dir)
    remove_dir(work_dir)
    build_assets(args, archive, assets_dir, work_dir)
    expected_bundles = len(list((assets_dir / "renpak" / "bundles").glob("*.webm")))
    expected_assets = manifest_asset_count(assets_dir / "renpak_manifest.json")
    print(f"built assets: manifest_assets={expected_assets} bundles={expected_bundles}")

    prepare_project(args, assets_dir)
    build_apk(args, apk, build_dir)
    verify_apk_assets(apk, expected_bundles)

    if not args.skip_device_check and not args.no_install:
        verify_device_decode(args, log_path, screenshot_path)

    print(f"smoke ok: archive={archive} apk={apk}")


if __name__ == "__main__":
    main()
