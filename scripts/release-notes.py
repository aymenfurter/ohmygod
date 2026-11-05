#!/usr/bin/env python3
"""Reject incomplete or mixed builds, then create checksums and download links."""

import hashlib
import json
from pathlib import Path
import sys

version, commit = sys.argv[1:]
dist = Path("dist")
targets = {
    "aarch64-apple-darwin": ("macOS (Apple silicon)", "macos-arm64.dmg"),
    "x86_64-apple-darwin": ("macOS (Intel)", "macos-x64.dmg"),
    "x86_64-pc-windows-msvc": ("Windows (x64)", "windows-x64.exe"),
}
records = [json.loads(p.read_text()) for p in dist.glob("*.json")]
if len(records) != len(targets) or {r["target"] for r in records} != targets.keys():
    raise SystemExit("The release must contain one installer for each platform.")
checksums = []
links = {}
for record in records:
    target = record["target"]
    name = f"OH-MY-GOD_{version}_{targets[target][1]}"
    if (record["version"], record["commit"], record["file"]) != (version, commit, name):
        raise SystemExit(f"Build metadata does not match this release: {target}")
    checksum = hashlib.sha256((dist / name).read_bytes()).hexdigest()
    if checksum != record["sha256"]:
        raise SystemExit(f"Installer checksum mismatch: {name}")
    checksums.append(f"{checksum}  {name}\n")
    links[target] = f"| {targets[target][0]} | [Download](https://github.com/aymenfurter/ohmygod/releases/download/v{version}/{name}) |"
(dist / "SHA256SUMS").write_text("".join(sorted(checksums)), encoding="ascii", newline="\n")
(dist / "release-notes.md").write_text(
    f"# OH MY GOD! {version}\n\n"
    "Sign in to GitHub with an account that has access to this private repository.\n\n"
    "| Platform | Installer |\n| --- | --- |\n"
    + "\n".join(links[t] for t in targets)
    + "\n\nInstallers are not code-signed or notarized. macOS Gatekeeper and Windows "
    "SmartScreen can block them. Use your organization's normal approval process. "
    "The Windows installer includes the console CLI and requires Windows 10 or later (x64). "
    "The Mac downloads require macOS 10.13 or later. SHA256SUMS contains installer checksums.\n\n"
    f"Source revision: `{commit}`\n"
)
