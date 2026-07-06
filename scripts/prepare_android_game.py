#!/usr/bin/env python3
"""Prepare a Ren'Py project directory for renpak Android packaging."""

from __future__ import annotations

import argparse
import filecmp
import importlib.util
import shutil
import subprocess
import time
from pathlib import Path
from typing import Optional


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def timestamp() -> str:
    return time.strftime("%Y%m%d-%H%M%S")


def project_and_game_dir(path: Path) -> tuple[Path, Path]:
    path = path.resolve()
    if path.name == "game":
        return path.parent, path

    game_dir = path / "game"
    if game_dir.is_dir():
        return path, game_dir

    raise SystemExit(f"could not find game/ under {path}")


def backup_path_for(path: Path, project_dir: Path, backup_root: Path) -> Path:
    try:
        rel = path.relative_to(project_dir)
    except ValueError:
        rel = Path(path.name)
    return backup_root / rel


def backup_if_needed(path: Path, project_dir: Path, backup_root: Path) -> None:
    if not path.exists():
        return
    backup = backup_path_for(path, project_dir, backup_root)
    backup.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, backup)
    print(f"backup {path} -> {backup}")


def copy_file(source: Path, dest: Path, project_dir: Path, backup_root: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() and filecmp.cmp(source, dest, shallow=False):
        print(f"unchanged {dest}")
        return
    backup_if_needed(dest, project_dir, backup_root)
    shutil.copy2(source, dest)
    print(f"copy {source} -> {dest}")


def copy_tree(source: Path, dest: Path, project_dir: Path, backup_root: Path) -> None:
    for path in sorted(source.rglob("*")):
        if path.is_dir():
            continue
        rel = path.relative_to(source)
        copy_file(path, dest / rel, project_dir, backup_root)


def prune_tree_to_source(source: Path, dest: Path, project_dir: Path, backup_root: Path) -> None:
    if not dest.exists():
        return

    source_files = {
        path.relative_to(source)
        for path in source.rglob("*")
        if path.is_file()
    }
    for path in sorted(dest.rglob("*"), reverse=True):
        if path.is_dir():
            continue
        rel = path.relative_to(dest)
        if rel in source_files:
            continue
        backup = backup_path_for(path, project_dir, backup_root)
        backup.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(path), str(backup))
        print(f"prune {path} -> {backup}")

    for path in sorted(dest.rglob("*"), reverse=True):
        if path.is_dir():
            try:
                path.rmdir()
            except OSError:
                pass


def install_runtime(game_dir: Path, project_dir: Path, backup_root: Path) -> None:
    runtime_dir = repo_root() / "python" / "runtime"
    copy_file(
        runtime_dir / "renpak_loader.py",
        game_dir / "renpak_loader.py",
        project_dir,
        backup_root,
    )
    copy_file(
        runtime_dir / "renpak_init.rpy",
        game_dir / "renpak_init.rpy",
        project_dir,
        backup_root,
    )


def install_android_assets(
    assets_dir: Path,
    game_dir: Path,
    project_dir: Path,
    backup_root: Path,
    prune: bool,
) -> None:
    manifest = assets_dir / "renpak_manifest.json"
    bundles = assets_dir / "renpak"

    if not manifest.is_file():
        raise SystemExit(f"missing {manifest}")
    if not bundles.is_dir():
        raise SystemExit(f"missing {bundles}")

    copy_file(manifest, game_dir / "renpak_manifest.json", project_dir, backup_root)
    if prune:
        prune_tree_to_source(bundles, game_dir / "renpak", project_dir, backup_root)
    copy_tree(bundles, game_dir / "renpak", project_dir, backup_root)


def install_archive(
    archive: Path,
    game_dir: Path,
    archive_name: Optional[str],
    project_dir: Path,
    backup_root: Path,
) -> None:
    if not archive.is_file():
        raise SystemExit(f"missing {archive}")

    name = archive_name or archive.name
    if "/" in name or "\\" in name or not name.endswith(".rpa"):
        raise SystemExit("--archive-name must be a simple .rpa filename")

    copy_file(archive, game_dir / name, project_dir, backup_root)


def verify_archive(
    archive: Path,
    expect_stripped: bool,
    android_assets_dir: Optional[Path],
) -> None:
    verifier = repo_root() / "scripts" / "verify_vp9_archive.py"
    spec = importlib.util.spec_from_file_location("verify_vp9_archive", verifier)
    if spec is None or spec.loader is None:
        raise SystemExit(f"unable to load {verifier}")

    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    names = module.read_rpa_index(archive)
    manifest = module.read_manifest_from_rpa(archive)
    assets = module.validate_manifest(manifest)

    missing_targets = []
    kept_originals = []
    for original, entry in assets.items():
        if not isinstance(entry, dict):
            raise SystemExit(f"{original}: manifest entry is not an object")
        target = entry.get("target")
        if not isinstance(target, str) or not target:
            raise SystemExit(f"{original}: missing target")
        if target not in names:
            missing_targets.append(target)
        if expect_stripped and original in names:
            kept_originals.append(original)

    if missing_targets:
        sample = ", ".join(sorted(set(missing_targets))[:10])
        raise SystemExit(f"archive verification failed, missing targets: {sample}")
    if kept_originals:
        sample = ", ".join(sorted(kept_originals)[:10])
        raise SystemExit(f"archive verification failed, originals still present: {sample}")

    print(
        f"verified archive {archive}: entries={len(names)} "
        f"assets={len(assets)} expect_stripped={expect_stripped}"
    )
    if android_assets_dir is not None:
        module.verify_android_assets(android_assets_dir, manifest)


def compile_project(renpy_sdk: Path, project_dir: Path) -> None:
    renpy_sh = renpy_sdk / "renpy.sh"
    if not renpy_sh.is_file():
        raise SystemExit(f"missing {renpy_sh}")

    print(f"compile {project_dir}")
    subprocess.run(
        [str(renpy_sh), str(project_dir), "compile", "--keep-orphan-rpyc"],
        check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("project_or_game_dir", type=Path)
    parser.add_argument(
        "--android-assets-dir",
        type=Path,
        help="Directory produced by renpak build-vp9 --android-assets-dir.",
    )
    parser.add_argument(
        "--prune-android-assets",
        action="store_true",
        help="Move stale game/renpak files into the prepare backup directory before copying assets.",
    )
    parser.add_argument(
        "--archive",
        type=Path,
        help="RPA archive to copy into game/. Use --archive-name to replace an existing archive name.",
    )
    parser.add_argument(
        "--archive-name",
        help="Destination archive filename under game/, for example archive_0.09.05.rpa.",
    )
    parser.add_argument(
        "--verify-archive",
        action="store_true",
        help="Verify the source archive manifest and bundle targets before copying. Also verifies --android-assets-dir when provided.",
    )
    parser.add_argument(
        "--expect-stripped",
        action="store_true",
        help="With --verify-archive, require manifest originals to be absent from the archive.",
    )
    parser.add_argument(
        "--compile-with-sdk",
        type=Path,
        help="Ren'Py SDK root. Runs renpy.sh <project> compile --keep-orphan-rpyc.",
    )
    args = parser.parse_args()

    project_dir, game_dir = project_and_game_dir(args.project_or_game_dir)
    stamp = timestamp()
    backup_root = project_dir / ".renpak_prepare_backups" / stamp

    install_runtime(game_dir, project_dir, backup_root)
    if args.android_assets_dir is not None:
        install_android_assets(
            args.android_assets_dir.resolve(),
            game_dir,
            project_dir,
            backup_root,
            args.prune_android_assets,
        )
    if args.archive is not None:
        archive = args.archive.resolve()
        if args.verify_archive:
            verify_archive(
                archive,
                args.expect_stripped,
                args.android_assets_dir.resolve() if args.android_assets_dir else None,
            )
        install_archive(archive, game_dir, args.archive_name, project_dir, backup_root)
    elif args.expect_stripped:
        raise SystemExit("--expect-stripped requires --archive and --verify-archive")
    if args.compile_with_sdk is not None:
        compile_project(args.compile_with_sdk.resolve(), project_dir)

    print(f"prepared {game_dir}")


if __name__ == "__main__":
    main()
