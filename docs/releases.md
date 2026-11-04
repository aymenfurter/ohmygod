# Releases

The repository and its release downloads stay private. A download requires a GitHub account
with repository access. Do not copy installers to public storage.

## Download and install

Open [Releases](https://github.com/aymenfurter/ohmygod/releases) and select:

| Computer | Download |
| --- | --- |
| Windows 10 or later, x64 | `windows-x64.exe` |
| Mac, Apple silicon | `macos-arm64.dmg` |
| Mac, Intel | `macos-x64.dmg` |

On Windows, run the per-user installer. On macOS, open the disk image and move
**OH MY GOD!** to Applications. Open the app and approve its setup changes.
Restart terminals and agent apps after setup.

The installers are not code-signed or notarized. Gatekeeper or SmartScreen can block
installation. Use your organization's normal approval process. Do not disable system
protection. Signing requires an Apple Developer identity and a Windows signing certificate;
these are not stored in this repository.

Use the **About OH MY GOD!** button in the top bar or tray menu to see the version,
source revision, platform, and license. The Downloads button opens Releases.
The CLI reports the same version with `ohmygod --version`.

## Versioning

`[workspace.package].version` in `Cargo.toml` is the only version source. All Rust
packages inherit it. Tauri reads the app package version when its configuration
has no version override. Use semantic versions:

- Increase PATCH for compatible fixes.
- Increase MINOR for compatible features.
- Increase MAJOR for incompatible changes after version 1.0.

Update the workspace version and run `cargo check` to update `Cargo.lock`.
Run `python3 scripts/check-version.py` before the version PR. Merge the PR after
the three native build jobs pass.

## Publish a release

Run the **Release** workflow on `main`, or push a tag that matches the workspace
version, such as `v0.2.0`. A manual run creates the matching tag at the tested commit.
The workflow builds all three installers before it publishes anything. It checks
that the repository is private before the build and again before publication.
It refuses an unmerged revision, mismatched tag, missing platform, mixed source
revision, checksum mismatch, or existing release.

The release contains direct download links and `SHA256SUMS`. Failed builds remain
private Actions artifacts and do not produce a partial release. There is no
GitHub Pages site, public download mirror, automatic update service, or visibility
change in this pipeline.

## Build locally

Install Rust, Python 3.11 or later, the Tauri 2 prerequisites for your platform,
and `cargo install tauri-cli --version 2.12.4 --locked`.
Run one command on the matching host:

```text
python scripts/build-release.py x86_64-pc-windows-msvc
python3 scripts/build-release.py aarch64-apple-darwin
python3 scripts/build-release.py x86_64-apple-darwin
```

Windows needs the Microsoft C++ build tools and WebView2. Its build script first
builds the separate console CLI and includes it as a Tauri sidecar. Setup creates
a marked command launcher in `%USERPROFILE%\.ohmygod\bin` and previews a user
PATH change. It does not need administrator access or symbolic links. The optional
login item is in the user's Startup folder. Setup removes only its own launcher,
PATH directory, instruction blocks, and login item.
