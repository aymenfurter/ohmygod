#!/usr/bin/env python3
"""Verify the disk image, bundle version, architecture, and bundled CLI."""

import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import time

root = Path(__file__).resolve().parents[1]
version = subprocess.check_output([sys.executable, root / "scripts/check-version.py"], text=True).strip()
architecture = "arm64" if sys.argv[1] == "aarch64-apple-darwin" else "x64"
image = root / f"dist/OH-MY-GOD_{version}_macos-{architecture}.dmg"
temp = Path(tempfile.mkdtemp(prefix="ohmygod-dmg-"))
mount = temp / "mount"
mount.mkdir()
link = temp / "ohmygod"
subprocess.run(["hdiutil", "verify", str(image)], check=True)
subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint", str(mount), str(image)], check=True)
try:
    app = mount / "OH MY GOD!.app"
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info["CFBundleShortVersionString"] != version:
        raise RuntimeError("The bundle version does not match Cargo.toml.")
    executable = app / "Contents/MacOS/ohmygod-app"
    kind = subprocess.check_output(["file", str(executable)], text=True)
    if ("arm64" if architecture == "arm64" else "x86_64") not in kind:
        raise RuntimeError(f"Wrong executable architecture: {kind}")
    link.symlink_to(executable)
    actual = subprocess.check_output([str(link), "--version"], text=True).strip()
    if actual != f"ohmygod {version}":
        raise RuntimeError(f"The bundled CLI version does not match: {actual}")
    print(f"Disk image, {architecture} app, and bundled CLI match version {version}.")
finally:
    link.unlink(missing_ok=True)
    for attempt in range(10):
        detached = subprocess.run(["hdiutil", "detach", str(mount)], capture_output=True, text=True)
        if detached.returncode == 0:
            break
        if attempt == 9:
            raise RuntimeError(f"Cannot detach test disk image at {mount}: {detached.stderr}")
        time.sleep(1)
    if mount.exists():
        mount.rmdir()
    os.rmdir(temp)
