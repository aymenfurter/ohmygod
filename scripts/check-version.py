#!/usr/bin/env python3
"""Cargo.toml is the version source for the app, CLI, installers, and tags."""

import argparse
import json
import os
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def version():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    value = manifest["workspace"]["package"]["version"]
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value):
        raise ValueError("Use a stable semantic version: MAJOR.MINOR.PATCH")
    for member in manifest["workspace"]["members"]:
        package = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
        if package["version"] != {"workspace": True}:
            raise ValueError(f"{member} must inherit the workspace version")
    for config in (ROOT / "src-tauri").glob("tauri*.conf.json"):
        if "version" in json.loads(config.read_text()):
            raise ValueError(f"{config.name} must inherit the Cargo version")
    return value


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag")
    args = parser.parse_args()
    value = version()
    if args.tag and args.tag != f"v{value}":
        parser.error(f"Tag {args.tag} does not match Cargo version v{value}")
    print(value)
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as stream:
            stream.write(f"version={value}\n")
