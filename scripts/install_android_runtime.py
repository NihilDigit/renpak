#!/usr/bin/env python3
"""Install renpak's Android runtime bridge into a Ren'Py RAPT tree."""

from __future__ import annotations

import argparse
import shutil
from pathlib import Path


BRIDGE_DECODE = """\
    public static String renpakDecodeFrameToCache(String webmPath, int frameIndex, String outName) {
        return RenpakRuntime.decodeFrameToCache(webmPath, frameIndex, outName);
    }

"""

BRIDGE_STATS = """\
    public static String renpakStats() {
        return RenpakRuntime.stats();
    }

"""

BRIDGE_CLEAR = """\
    public static boolean renpakClearCache() {
        return RenpakRuntime.clearCache();
    }

"""


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def install_java_root(java_root: Path, source: Path) -> None:
    activity = java_root / "PythonSDLActivity.java"
    runtime = java_root / "RenpakRuntime.java"

    if not activity.exists():
        raise SystemExit(f"PythonSDLActivity.java not found under {java_root}")

    shutil.copy2(source, runtime)

    text = activity.read_text()
    marker = "    // Activity Requests ///////////////////////////////////////////////////////\n"
    if marker not in text:
        raise SystemExit("could not find insertion point in PythonSDLActivity.java")

    bridge = ""
    if "renpakDecodeFrameToCache" not in text:
        bridge += BRIDGE_DECODE
    if "renpakStats" not in text:
        bridge += BRIDGE_STATS
    if "renpakClearCache" not in text:
        bridge += BRIDGE_CLEAR

    if bridge:
        text = text.replace(marker, bridge + marker, 1)
        activity.write_text(text)

    print(f"installed RenpakRuntime.java -> {runtime}")
    print(f"patched bridge methods -> {activity}")


def install_runtime(rapt_root: Path) -> None:
    source = repo_root() / "android" / "rapt" / "renpyandroid" / "src" / "main" / "java" / "org" / "renpy" / "android" / "RenpakRuntime.java"
    relative = Path("renpyandroid/src/main/java/org/renpy/android")

    install_java_root(rapt_root / "prototype" / relative, source)

    project_root = rapt_root / "project" / relative
    if project_root.exists():
        install_java_root(project_root, source)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("rapt_root", type=Path, help="Path to the Ren'Py SDK rapt directory.")
    args = parser.parse_args()
    install_runtime(args.rapt_root.resolve())


if __name__ == "__main__":
    main()
