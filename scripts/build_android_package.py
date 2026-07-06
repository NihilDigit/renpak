#!/usr/bin/env python3
"""Build a Ren'Py Android package through RAPT with renpak runtime support."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


SDK_PYTHON_ENV = "RENPAK_ANDROID_BUILD_SDK_PYTHON"


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def sdk_python(renpy_sdk: Path) -> Path:
    platform = os.environ.get("RENPY_PLATFORM", "linux-x86_64")
    python = renpy_sdk / "lib" / f"py3-{platform}" / "python"
    if not python.is_file():
        raise SystemExit(f"missing Ren'Py SDK python: {python}")
    return python


def sdk_python_lib(renpy_sdk: Path) -> Path:
    lib = renpy_sdk / "lib" / "python3.12"
    if not lib.is_dir():
        raise SystemExit(f"missing Ren'Py SDK python lib: {lib}")
    return lib


def reexec_with_sdk_python(args: argparse.Namespace) -> None:
    if os.environ.get(SDK_PYTHON_ENV) == "1":
        return

    env = os.environ.copy()
    env[SDK_PYTHON_ENV] = "1"

    sdk_lib = str(sdk_python_lib(args.renpy_sdk))
    env["PYTHONPATH"] = (
        sdk_lib
        if not env.get("PYTHONPATH")
        else sdk_lib + os.pathsep + env["PYTHONPATH"]
    )

    os.execvpe(
        str(sdk_python(args.renpy_sdk)),
        [str(sdk_python(args.renpy_sdk)), __file__, *sys.argv[1:]],
        env,
    )


def install_runtime(rapt_root: Path) -> None:
    sys.path.insert(0, str(repo_root() / "scripts"))
    from install_android_runtime import install_runtime as install

    install(rapt_root)


def prepare_build_dir(source: Path, build_dir: Path) -> None:
    if build_dir.exists():
        shutil.rmtree(build_dir)
    shutil.copytree(
        source,
        build_dir,
        symlinks=True,
        ignore=shutil.ignore_patterns("*.bak.*", ".renpak_prepare_backups"),
    )


class Interface:
    def info(self, message):
        print(message)

    def success(self, message):
        print(message)

    def final_success(self, message):
        print(message)

    def fail(self, message):
        raise RuntimeError(message)

    def call(self, args, **kwargs):
        print("+", " ".join(args))
        stdin = None
        if kwargs.get("yes", False):
            stdin = "y\n" * 20
        subprocess.run(args, check=True, text=True, input=stdin)

    def background(self, fn):
        fn()

    def open_directory(self, *args, **kwargs):
        pass

    def terms(self, *args, **kwargs):
        pass

    def download(self, *args, **kwargs):
        raise RuntimeError("download not expected during scripted Android build")


def patch_signing_keys(
    rapt_root: Path,
    android_keystore: Path | None,
    bundle_keystore: Path | None,
    key_alias: str,
    store_password: str,
    alias_password: str,
) -> None:
    if android_keystore is None and bundle_keystore is None:
        return

    sys.path.insert(0, str(rapt_root / "buildlib"))
    from rapt.properties import bundle_properties, local_properties, set_property
    import rapt.build

    def update_project_keys(_base):
        for properties, key_path in (
            (local_properties, android_keystore),
            (bundle_properties, bundle_keystore),
        ):
            if key_path is None:
                continue
            set_property(properties, "key.alias", key_alias, replace=True)
            set_property(properties, "key.store.password", store_password, replace=True)
            set_property(properties, "key.alias.password", alias_password, replace=True)
            set_property(properties, "key.store", str(key_path), replace=True)

    rapt.build.update_project_keys = update_project_keys


def run_build(args: argparse.Namespace) -> None:
    rapt_root = args.rapt_root or args.renpy_sdk / "rapt"
    if not rapt_root.is_dir():
        raise SystemExit(f"missing RAPT root: {rapt_root}")

    if args.install_runtime:
        install_runtime(rapt_root)

    build_dir = args.build_dir or args.project.with_name(args.project.name + "-android-build")
    prepare_build_dir(args.project, build_dir)

    os.environ["ANDROID_HOME"] = str(args.android_home)
    if args.java_home is not None:
        os.environ["JAVA_HOME"] = str(args.java_home)
        os.environ["PATH"] = (
            str(args.java_home / "bin")
            + os.pathsep
            + str(args.android_home / "platform-tools")
            + os.pathsep
            + os.environ.get("PATH", "")
        )

    sys.path.insert(0, str(args.renpy_sdk))
    sys.path.insert(0, str(rapt_root / "buildlib"))

    import rapt.build

    patch_signing_keys(
        rapt_root,
        args.android_keystore,
        args.bundle_keystore,
        args.key_alias,
        args.store_password,
        args.alias_password,
    )

    rapt.build.build(
        Interface(),
        str(build_dir),
        str(build_dir),
        install=args.install,
        bundle=args.bundle,
        launch=args.launch,
    )

    apk = rapt_root / "project" / "app" / "build" / "outputs" / "apk" / "release" / "app-release.apk"
    if apk.is_file():
        print(f"apk: {apk}")
        if args.output_apk is not None:
            args.output_apk.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(apk, args.output_apk)
            print(f"copy apk -> {args.output_apk}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("project", type=Path, help="Prepared Ren'Py project directory.")
    parser.add_argument("--renpy-sdk", type=Path, required=True)
    parser.add_argument("--rapt-root", type=Path)
    parser.add_argument("--build-dir", type=Path)
    parser.add_argument("--android-home", type=Path, default=Path(os.environ.get("ANDROID_HOME", "/opt/android-sdk")))
    parser.add_argument("--java-home", type=Path, default=Path(os.environ["JAVA_HOME"]) if os.environ.get("JAVA_HOME") else None)
    parser.add_argument("--install-runtime", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--install", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--launch", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--bundle", action="store_true")
    parser.add_argument("--output-apk", type=Path)
    parser.add_argument("--android-keystore", type=Path)
    parser.add_argument("--bundle-keystore", type=Path)
    parser.add_argument("--key-alias", default="android")
    parser.add_argument("--store-password", default="android")
    parser.add_argument("--alias-password", default="android")
    args = parser.parse_args()
    args.project = args.project.resolve()
    args.renpy_sdk = args.renpy_sdk.resolve()
    args.rapt_root = args.rapt_root.resolve() if args.rapt_root else None
    args.build_dir = args.build_dir.resolve() if args.build_dir else None
    args.android_home = args.android_home.resolve()
    args.java_home = args.java_home.resolve() if args.java_home else None
    args.android_keystore = args.android_keystore.resolve() if args.android_keystore else None
    args.bundle_keystore = args.bundle_keystore.resolve() if args.bundle_keystore else None
    args.output_apk = args.output_apk.resolve() if args.output_apk else None
    return args


def main() -> None:
    args = parse_args()
    reexec_with_sdk_python(args)
    run_build(args)


if __name__ == "__main__":
    main()
