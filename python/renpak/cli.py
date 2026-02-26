"""CLI entry point for renpak."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(
        prog="renpak",
        description="Ren'Py asset compression toolkit — JPG/PNG → AVIF transcoding",
    )
    subparsers = parser.add_subparsers(dest="command")

    # build
    p_build = subparsers.add_parser("build", help="Build compressed RPA archive")
    p_build.add_argument("game_dir", type=Path, help="Game directory containing .rpa files")
    p_build.add_argument("-o", "--output", type=Path, default=None, help="Output directory")
    p_build.add_argument("--limit", type=int, default=0, help="Max images to encode (0 = all)")
    p_build.add_argument("--quality", type=int, default=50, help="AVIF quality 1-63 (default: 50)")

    # analyze
    p_analyze = subparsers.add_parser("analyze", help="Analyze RPA contents without encoding")
    p_analyze.add_argument("game_dir", type=Path, help="Game directory containing .rpa files")

    # info
    p_info = subparsers.add_parser("info", help="Show RPA index information")
    p_info.add_argument("rpa_file", type=Path, help="Path to .rpa file")

    args = parser.parse_args()

    if args.command is None:
        parser.print_help()
        sys.exit(1)

    from renpak.build import build, analyze, info

    if args.command == "build":
        output_dir = args.output or Path(str(args.game_dir) + "_compressed")
        build(args.game_dir, output_dir, limit=args.limit, quality=args.quality)
    elif args.command == "analyze":
        analyze(args.game_dir)
    elif args.command == "info":
        info(args.rpa_file)
