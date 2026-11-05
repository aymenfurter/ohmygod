#!/usr/bin/env python3
"""Build a native installer. Run on a host that matches the target."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

from importlib.util import module_from_spec, spec_from_file_location

ROOT = Path(__file__).resolve().parents[1]
spec = spec_from_file_location("version", ROOT / "scripts/check-version.py")
versions = module_from_spec(spec)
spec.loader.exec_module(versions)


def run(*args, cwd=ROOT):
    subprocess.run(args, cwd=cwd, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=[
        "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc",
    ])
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--prepare-cli-only", action="store_true")
    args = parser.parse_args()
    version = versions.version()
    target = args.target
    windows = "windows" in target
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT, text=True,
    ))
    build = Path(metadata["target_directory"]) / target / "release"
    if windows:
        run("cargo", "build", "--locked", "--release", "--target", target, "-p", "ohmygod-cli")
        sidecars = ROOT / "src-tauri/binaries"
        sidecars.mkdir(exist_ok=True)
        shutil.copy2(build / "ohmygod.exe", sidecars / f"ohmygod-{target}.exe")
    if args.prepare_cli_only:
        return
    run("node", str(ROOT / "node_modules/@tauri-apps/cli/tauri.js"),
        "build", "--ci", "--target", target,
        "--bundles", "nsis" if windows else "dmg", cwd=ROOT / "src-tauri")
    pattern = f"nsis/*_{version}_*.exe" if windows else f"dmg/*_{version}_*.dmg"
    installers = list((build / "bundle").glob(pattern))
    if len(installers) != 1:
        raise RuntimeError(f"Expected one installer in {build / 'bundle'}, got {installers}")
    args.output.mkdir(parents=True, exist_ok=True)
    suffix = "windows-x64.exe" if windows else "macos-arm64.dmg" if target.startswith("aarch64") else "macos-x64.dmg"
    destination = args.output / f"OH-MY-GOD_{version}_{suffix}"
    shutil.copy2(installers[0], destination)
    metadata = {
        "version": version,
        "target": target,
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "file": destination.name,
        "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
    }
    (args.output / f"{destination.name}.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(destination)


if __name__ == "__main__":
    main()
