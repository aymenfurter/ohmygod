//! Harness detection and consent-based integration with GitHub Copilot.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};

use crate::config::{data_home, home_dir, mcp_url, Style, SERVER_NAME};
use crate::instructions::{instruction_block_with, remove_block, upsert_block};
use crate::store::write_atomic;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Harness {
    pub id: String,
    pub name: String,
    pub detected: bool,
    pub supported: bool,
    /// Key of a brand mark the UI may show. Only set where the brand owner permits
    /// showing an integration (GitHub logo policy: "Show integration").
    pub logo: Option<String>,
    pub evidence: Vec<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffLine {
    pub tag: &'static str,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub id: String,
    pub label: String,
    pub path: String,
    pub display_path: String,
    pub purpose: String,
    /// create | modify | unchanged | remove | error
    pub status: String,
    pub warning: Option<String>,
    pub before_sha: Option<String>,
    pub after: String,
    pub diff: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub changes: Vec<FileChange>,
    pub installed: bool,
}

pub fn copilot_home() -> PathBuf {
    match std::env::var_os("COPILOT_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => home_dir().join(".copilot"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstructionsState {
    pub installed: bool,
    pub version: Option<u32>,
    pub style: Option<String>,
    pub uses: Option<Vec<String>>,
    pub current: u32,
    pub outdated: bool,
}

/// Agent tools OH MY GOD! sets up. All of them use the `ohmygod` CLI from their shell; they differ
/// only in where their user-level instructions live. The file names match what Ruler
/// (github.com/intellectronica/ruler) writes per project: CLAUDE.md for Claude Code, AGENTS.md
/// for Codex, OpenCode, and pi.
pub const HARNESSES: [(&str, &str); 5] = [
    ("copilot", "GitHub Copilot"),
    ("claude-code", "Claude Code"),
    ("codex", "Codex"),
    ("opencode", "OpenCode"),
    ("pi", "pi"),
];

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn claude_home() -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home_dir().join(".claude"))
}

/// User-level instructions file of a harness.
pub fn harness_instructions_path(id: &str) -> Option<PathBuf> {
    let home = home_dir();
    Some(match id {
        "copilot" => instructions_path(),
        "claude-code" => claude_home().join("CLAUDE.md"),
        "codex" => env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex")).join("AGENTS.md"),
        "opencode" => env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")).join("opencode/AGENTS.md"),
        "pi" => home.join(".pi/agent/AGENTS.md"),
        _ => return None,
    })
}

/// Version of the OH MY GOD! blocks in the harness instruction files, checked on startup.
/// The oldest installed block decides whether an update is offered.
pub fn instructions_state() -> InstructionsState {
    let current = crate::instructions::INSTRUCTIONS_VERSION;
    let mut found: Option<(u32, Option<String>, Option<Vec<String>>)> = None;
    for (id, _) in HARNESSES {
        let text = harness_instructions_path(id).and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        if let Some(b) = crate::instructions::installed_block_full(&text) {
            if found.as_ref().map_or(true, |f| b.0 < f.0) {
                found = Some(b);
            }
        }
    }
    match found {
        Some((v, style, uses)) => InstructionsState { installed: true, version: Some(v), style, uses, current, outdated: v < current },
        None => InstructionsState { installed: false, version: None, style: None, uses: None, current, outdated: false },
    }
}

pub fn instructions_path() -> PathBuf {
    copilot_home().join("copilot-instructions.md")
}

pub fn mcp_config_path() -> PathBuf {
    copilot_home().join("mcp-config.json")
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Show a path with `~` for the home directory.
pub fn tilde(p: &Path) -> String {
    let home = home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Runtimes that SDK hosts (for example the Copilot app) download into caches are not the
/// user's own installation. OH MY GOD! never selects them automatically.
fn is_bundled_runtime(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.contains("github-copilot-sdk") || s.contains("copilot-sdk-runtime") || s.contains(".app/Contents/")
}

const HARNESS_BINS: [&str; 5] = ["copilot", "claude", "codex", "opencode", "pi"];

type ShellCache = Option<(std::collections::HashMap<String, PathBuf>, std::time::Instant)>;
static SHELL_CACHE: std::sync::Mutex<ShellCache> = std::sync::Mutex::new(None);

/// Resolve commands the way the user's terminal does: through the login shell, in one call.
/// GUI apps on macOS do not inherit the shell PATH. Cached for a minute.
fn login_shell_paths() -> std::collections::HashMap<String, PathBuf> {
    let mut cache = SHELL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((map, at)) = cache.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(60) {
            return map.clone();
        }
    }
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let script = HARNESS_BINS
        .iter()
        .map(|b| format!("printf '%s=%s\\n' {b} \"$(command -v {b} 2>/dev/null)\""))
        .collect::<Vec<_>>()
        .join("; ");
    let mut map = std::collections::HashMap::new();
    if let Ok(out) = std::process::Command::new(shell)
        .args(["-lc", &script])
        .env_remove("COPILOT_CLI_PATH")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((k, v)) = line.split_once('=') {
                let p = PathBuf::from(v.trim());
                if p.is_absolute() && p.is_file() && !is_bundled_runtime(&p) {
                    map.insert(k.to_string(), p);
                }
            }
        }
    }
    *cache = Some((map.clone(), std::time::Instant::now()));
    map
}

fn login_shell_which(bin: &str) -> Option<PathBuf> {
    login_shell_paths().get(bin).cloned()
}

fn which(bin: &str) -> Option<PathBuf> {
    if let Some(p) = login_shell_which(bin) {
        return Some(p);
    }
    let home = home_dir();
    let mut dirs: Vec<PathBuf> = vec![
        home.join(".local/bin"),
        home.join(".homebrew/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".npm-global/bin"),
        home.join(".bun/bin"),
        home.join(".opencode/bin"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(bin)).find(|p| p.is_file() && !is_bundled_runtime(p))
}

static CLI_CACHE: std::sync::Mutex<Option<(Option<String>, Option<PathBuf>, std::time::Instant)>> = std::sync::Mutex::new(None);

/// The user's own Copilot CLI: configured path, then the login shell, then common install
/// locations. Results are cached for a minute because the login shell is slow to start.
pub fn find_copilot_cli(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured.filter(|s| !s.is_empty()) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let key = configured.map(String::from);
    let mut cache = CLI_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, v, at)) = cache.as_ref() {
        if *k == key && at.elapsed() < std::time::Duration::from_secs(60) {
            return v.clone();
        }
    }
    let found = which("copilot").or_else(|| {
        std::env::var_os("COPILOT_CLI_PATH").map(PathBuf::from).filter(|p| p.is_file() && !is_bundled_runtime(p))
    });
    *cache = Some((key, found.clone(), std::time::Instant::now()));
    found
}

/// `GitHub Copilot CLI 1.0.105.` → `1.0.105`
pub fn copilot_version(cli: &Path) -> Option<String> {
    static CACHE: std::sync::Mutex<Option<(PathBuf, Option<String>)>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, v)) = cache.as_ref() {
        if p == cli {
            return v.clone();
        }
    }
    let v = copilot_version_uncached(cli);
    *cache = Some((cli.to_path_buf(), v.clone()));
    v
}

fn copilot_version_uncached(cli: &Path) -> Option<String> {
    let out = std::process::Command::new(cli)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|w| w.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .map(|w| w.trim_end_matches('.').to_string())
}

fn exists(p: &Path) -> bool {
    p.exists()
}

fn evidence_for(paths: &[(PathBuf, &str)], bins: &[&str]) -> Vec<String> {
    let mut ev = vec![];
    for b in bins {
        if let Some(p) = which(b) {
            ev.push(tilde(&p));
        }
    }
    for (p, label) in paths {
        if exists(p) {
            let _ = label;
            ev.push(tilde(p));
        }
    }
    ev
}

/// Read-only detection of agent harnesses on this machine.
pub fn detect(configured_cli: Option<&str>) -> Vec<Harness> {
    let home = home_dir();
    let mut out = vec![];

    // GitHub Copilot: also runs the moderator and supervisor.
    let cli = find_copilot_cli(configured_cli);
    let version = cli.as_deref().and_then(copilot_version);
    let chome = copilot_home();
    let mut ev = vec![];
    if let Some(p) = &cli {
        ev.push(match &version {
            Some(v) => format!("CLI {v} · {}", tilde(p)),
            None => tilde(p),
        });
    }
    if chome.is_dir() {
        ev.push(format!("config {}", tilde(&chome)));
    }
    let mut checks = vec![];
    checks.push(Check {
        label: "Copilot CLI".into(),
        ok: cli.is_some(),
        detail: match (&cli, &version) {
            (Some(p), Some(v)) => format!("Version {v} at {}. The moderator and supervisor use it.", tilde(p)),
            (Some(p), None) => format!("{}. The moderator and supervisor use it.", tilde(p)),
            _ => "Not found in your login shell. Install GitHub Copilot CLI, or set its path below.".into(),
        },
    });
    checks.push(Check {
        label: "Configuration directory".into(),
        ok: chome.is_dir(),
        detail: format!(
            "{}{}",
            tilde(&chome),
            if std::env::var_os("COPILOT_HOME").is_some() { " (from COPILOT_HOME)" } else { "" }
        ),
    });
    let ipath = instructions_path();
    let icontent = fs::read_to_string(&ipath).unwrap_or_default();
    let istate = instructions_state();
    checks.push(Check {
        label: "Custom instructions".into(),
        ok: istate.installed && !istate.outdated,
        detail: if !ipath.exists() {
            format!("{} does not exist yet. Setup creates it.", tilde(&ipath))
        } else if istate.outdated {
            format!("OH MY GOD! block version {} in {}. Version {} is available.", istate.version.unwrap_or(1), tilde(&ipath), istate.current)
        } else if icontent.contains("<!-- ohmygod:begin") {
            format!("OH MY GOD! block version {} present in {}.", istate.current, tilde(&ipath))
        } else {
            format!("{} exists. OH MY GOD! block not present.", tilde(&ipath))
        },
    });
    let link = cli_link();
    checks.push(Check {
        label: "ohmygod command".into(),
        ok: link.installed,
        detail: if link.installed {
            format!("Linked at {}.", tilde(&link.path))
        } else {
            format!("Setup links it at {} so that agents can run it.", tilde(&link.path))
        },
    });
    out.push(Harness {
        id: "copilot".into(),
        name: "GitHub Copilot".into(),
        detected: !ev.is_empty(),
        supported: true,
        logo: Some("copilot".into()),
        evidence: ev,
        checks,
    });

    // Logos only where the owner's terms allow showing them; see THIRD_PARTY_NOTICES.md.
    let others: [(&str, &str, Option<&str>, Vec<(PathBuf, &str)>, Vec<&str>); 4] = [
        ("claude-code", "Claude Code", None, vec![(home.join(".claude"), "config directory")], vec!["claude"]),
        ("codex", "Codex", None, vec![(home.join(".codex"), "config directory")], vec!["codex"]),
        (
            "opencode",
            "OpenCode",
            Some("opencode"),
            vec![(home.join(".config/opencode"), "config directory"), (home.join(".opencode"), "data directory")],
            vec!["opencode"],
        ),
        ("pi", "pi", Some("pi"), vec![(home.join(".pi/agent"), "config directory")], vec!["pi"]),
    ];
    for (id, name, logo, paths, bins) in others {
        let ev = evidence_for(&paths, &bins);
        out.push(Harness {
            id: id.into(),
            name: name.into(),
            detected: !ev.is_empty(),
            supported: true,
            logo: logo.map(String::from),
            evidence: ev,
            checks: vec![],
        });
    }
    out
}

fn diff(before: &str, after: &str) -> (Vec<DiffLine>, usize, usize) {
    let d = TextDiff::from_lines(before, after);
    let mut lines = vec![];
    let (mut added, mut removed) = (0, 0);
    for group in d.grouped_ops(3) {
        if !lines.is_empty() {
            lines.push(DiffLine { tag: "gap", old: None, new: None, text: "…".into() });
        }
        for op in group {
            for change in d.iter_changes(&op) {
                let tag = match change.tag() {
                    ChangeTag::Delete => {
                        removed += 1;
                        "del"
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        "add"
                    }
                    ChangeTag::Equal => "ctx",
                };
                lines.push(DiffLine {
                    tag,
                    old: change.old_index().map(|i| i + 1),
                    new: change.new_index().map(|i| i + 1),
                    text: change.value().trim_end_matches('\n').to_string(),
                });
            }
        }
    }
    (lines, added, removed)
}

fn change(id: &str, label: &str, purpose: &str, path: &Path, before: Option<String>, after: Result<String>, removing: bool) -> FileChange {
    let before_sha = before.as_ref().map(|b| sha(b.as_bytes()));
    match after {
        Err(e) => FileChange {
            id: id.into(),
            label: label.into(),
            path: path.display().to_string(),
            display_path: tilde(path),
            purpose: purpose.into(),
            status: "error".into(),
            warning: Some(format!("{e:#}")),
            before_sha,
            after: String::new(),
            diff: vec![],
            added: 0,
            removed: 0,
        },
        Ok(after) => {
            let b = before.clone().unwrap_or_default();
            let (lines, added, removed) = diff(&b, &after);
            let status = if before.as_deref() == Some(after.as_str()) {
                "unchanged"
            } else if removing {
                "remove"
            } else if before.is_none() {
                "create"
            } else {
                "modify"
            };
            FileChange {
                id: id.into(),
                label: label.into(),
                path: path.display().to_string(),
                display_path: tilde(path),
                purpose: purpose.into(),
                status: status.into(),
                warning: None,
                before_sha,
                after,
                diff: lines,
                added,
                removed,
            }
        }
    }
}

/// Remove the `ohmygod` MCP entry that earlier versions registered. An entry that points
/// elsewhere belongs to something else and stays.
fn mcp_cleanup(before: &str, port: u16) -> Result<(String, Option<String>)> {
    let mut root: Value = serde_json::from_str(before).context("The file is not valid JSON. Fix it by hand; OH MY GOD! will not overwrite it")?;
    let servers = root.get_mut("mcpServers").and_then(Value::as_object_mut).ok_or_else(|| anyhow!("`mcpServers` is not a JSON object"))?;
    if let Some(existing) = servers.get(SERVER_NAME) {
        if existing.get("url").and_then(Value::as_str) != Some(mcp_url(port).as_str()) {
            return Ok((before.to_string(), Some(format!("The `{SERVER_NAME}` entry does not point to this OH MY GOD! app. OH MY GOD! keeps it."))));
        }
    }
    servers.remove(SERVER_NAME);
    Ok((serde_json::to_string_pretty(&root)? + "\n", None))
}

pub const LAUNCH_AGENT_LABEL: &str = "dev.ohmygod.desktop";

pub fn launch_agent_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{LAUNCH_AGENT_LABEL}.plist"))
}

/// The login item of the app's earlier name. Setup removes it.
pub fn legacy_launch_agent_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents/dev.pinrelay.desktop.plist")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// LaunchAgent that opens the app at login. `app` is the .app bundle, or the binary in development.
fn launch_agent_plist(app: &Path) -> String {
    let args = if app.extension().is_some_and(|e| e == "app") {
        format!("    <string>/usr/bin/open</string>\n    <string>-a</string>\n    <string>{}</string>", xml_escape(&app.display().to_string()))
    } else {
        format!("    <string>{}</string>", xml_escape(&app.display().to_string()))
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

// ---------------------------------------------------------------- ohmygod CLI on the PATH

const PROFILE_BEGIN: &str = "# ohmygod:begin (managed by the OH MY GOD! app)";
const PROFILE_END: &str = "# ohmygod:end";
const LEGACY_PROFILE: (&str, &str) = ("# pinrelay:begin", "# pinrelay:end");

pub struct CliLink {
    /// Where the `ohmygod` link goes.
    pub path: PathBuf,
    /// The directory is on the login shell PATH. If not, setup adds it in the shell profile.
    pub in_path: bool,
    pub installed: bool,
    /// Another `ohmygod` that the login shell finds first, if any.
    pub shadowed_by: Option<PathBuf>,
}

/// PATH of the user's login shell and what `ohmygod` resolves to there.
fn login_shell_path() -> (Vec<PathBuf>, Option<PathBuf>) {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let out = std::process::Command::new(shell)
        .args(["-lc", "printf '%s\\n' \"$PATH\"; command -v ohmygod 2>/dev/null"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut lines = text.lines();
    let path = lines.next().unwrap_or_default().split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect();
    let found = lines.next().map(PathBuf::from).filter(|p| p.is_absolute());
    (path, found)
}

fn writable_dir(p: &Path) -> bool {
    p.is_dir() && std::process::Command::new("/bin/test").arg("-w").arg(p).status().is_ok_and(|s| s.success())
}

/// `pinrelay` links from the app's earlier name that point to an app binary. Links to anything
/// else are not ours and stay.
fn legacy_links() -> Vec<PathBuf> {
    let home = home_dir();
    [home.join(".local/bin"), home.join("bin"), home.join(".homebrew/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]
        .iter()
        .map(|d| d.join("pinrelay"))
        .filter(|l| {
            fs::read_link(l).is_ok_and(|t| {
                let t = t.display().to_string();
                t.contains("pinrelay-app") || t.contains("ohmygod-app") || t.contains(".app/Contents/MacOS/")
            })
        })
        .collect()
}

/// Pick a user-writable directory on the login shell PATH for the `ohmygod` link.
pub fn cli_link() -> CliLink {
    let home = home_dir();
    let (path, found) = login_shell_path();
    let candidates = [home.join(".local/bin"), home.join("bin"), home.join(".homebrew/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    let (dir, in_path) = candidates
        .iter()
        .find(|d| path.contains(d) && writable_dir(d))
        .map(|d| (d.clone(), true))
        .unwrap_or((home.join(".local/bin"), false));
    let link = dir.join("ohmygod");
    let installed = fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink());
    let shadowed_by = found.filter(|f| in_path && f != &link);
    CliLink { path: link, in_path, installed, shadowed_by }
}

fn shell_profile() -> PathBuf {
    let home = home_dir();
    match std::env::var("SHELL").unwrap_or_default().rsplit('/').next() {
        Some("bash") => home.join(".bash_profile"),
        Some("zsh") | None | Some("") => home.join(".zprofile"),
        _ => home.join(".profile"),
    }
}

fn strip_block(text: &str, begin: &str, end_marker: &str) -> String {
    match (text.find(begin), text.find(end_marker)) {
        (Some(a), Some(b)) if b > a => {
            let end = b + end_marker.len();
            let end = if text[end..].starts_with('\n') { end + 1 } else { end };
            format!("{}{}", &text[..a], &text[end..])
        }
        _ => text.to_string(),
    }
}

fn profile_after(before: &str, dir: Option<&Path>) -> String {
    let mut out = strip_block(&strip_block(before, PROFILE_BEGIN, PROFILE_END), LEGACY_PROFILE.0, LEGACY_PROFILE.1);
    if let Some(dir) = dir {
        let home = home_dir();
        let shown = match dir.strip_prefix(&home) {
            Ok(rest) => format!("$HOME/{}", rest.display()),
            Err(_) => dir.display().to_string(),
        };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("{PROFILE_BEGIN}\nexport PATH=\"{shown}:$PATH\"\n{PROFILE_END}\n"));
    }
    out
}

/// A link "file" in the plan: its content is the link target, one line.
fn link_target(p: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(p) {
        Err(_) => Ok(None),
        Ok(m) if m.file_type().is_symlink() => Ok(Some(format!("{}\n", fs::read_link(p)?.display()))),
        Ok(_) => bail!("{} is a different program named ohmygod. Remove or rename it, then review again.", tilde(p)),
    }
}

// ---------------------------------------------------------------- Claude Code permission

const CLAUDE_RULE: &str = "Bash(ohmygod:*)";
const LEGACY_CLAUDE_RULE: &str = "Bash(pinrelay:*)";

/// Let Claude Code run `ohmygod` without asking each time. Other settings stay as they are.
fn claude_settings_after(before: Option<&str>, remove: bool) -> Result<String> {
    let mut root: Value = match before {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(s).map_err(|e| anyhow!("Not valid JSON ({e}). Setup will not change it."))?,
        _ => json!({}),
    };
    let obj = root.as_object_mut().ok_or_else(|| anyhow!("The settings file is not a JSON object."))?;
    let perms = obj.entry("permissions").or_insert_with(|| json!({}));
    let perms = perms.as_object_mut().ok_or_else(|| anyhow!("`permissions` is not an object."))?;
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    let list = allow.as_array_mut().ok_or_else(|| anyhow!("`permissions.allow` is not a list."))?;
    list.retain(|v| v != CLAUDE_RULE && v != LEGACY_CLAUDE_RULE);
    if !remove {
        list.push(json!(CLAUDE_RULE));
    }
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

// ---------------------------------------------------------------- plan and apply

pub struct PlanOptions<'a> {
    pub style: Style,
    pub uses: &'a [String],
    /// Agents may visit the board on their own.
    pub visits: bool,
    /// Agents compete on a visible impact ranking.
    pub reputation: bool,
    pub features: crate::model::Features,
    pub port: u16,
    pub uninstall: bool,
    /// Some(app) to start OH MY GOD! at login.
    pub login: Option<&'a Path>,
    /// Harness IDs from [`HARNESSES`] to set up.
    pub harnesses: &'a [String],
    /// Program the `ohmygod` link points to (the app binary).
    pub cli: Option<&'a Path>,
}

/// Compute the proposed changes. Nothing is written.
pub fn plan(o: &PlanOptions) -> Plan {
    let mut changes = vec![];

    // The `ohmygod` command, and the PATH entry when its directory is not on the PATH yet.
    let link = cli_link();
    let want_cli = if o.uninstall { None } else { o.cli };
    if want_cli.is_some() || link.installed {
        let after = want_cli.map(|p| format!("{}\n", p.display())).unwrap_or_default();
        let (before, after) = match link_target(&link.path) {
            Ok(b) => (b, Ok(after)),
            Err(e) => (None, Err(e)),
        };
        let mut c = change(
            "cli",
            "ohmygod command",
            "Links the `ohmygod` command into your PATH, so that every agent tool can use the board from its shell. The link points to the OH MY GOD! app.",
            &link.path,
            before,
            after,
            want_cli.is_none(),
        );
        if c.warning.is_none() {
            c.warning = link.shadowed_by.as_ref().map(|p| format!("Your shell finds another `ohmygod` first, at {}. Remove it so that agents use this one.", tilde(p)));
        }
        changes.push(c);
    }
    let profile = shell_profile();
    let pbefore = fs::read_to_string(&profile).ok();
    let need_path = want_cli.is_some() && !link.in_path;
    let has_block = pbefore.as_deref().is_some_and(|s| s.contains(PROFILE_BEGIN) || s.contains(LEGACY_PROFILE.0));
    if need_path || has_block {
        let dir = link.path.parent().filter(|_| need_path);
        let after = profile_after(pbefore.as_deref().unwrap_or(""), dir);
        changes.push(change(
            "path",
            "Shell PATH",
            "Adds the directory of the `ohmygod` command to your PATH. Open a new terminal afterwards.",
            &profile,
            pbefore,
            Ok(after),
            dir.is_none(),
        ));
    }

    // Instructions for each harness: added where selected, removed where deselected.
    let block = instruction_block_with(o.style, o.uses, o.visits, o.reputation, o.features);
    for (id, name) in HARNESSES {
        let Some(path) = harness_instructions_path(id) else { continue };
        let before = fs::read_to_string(&path).ok();
        let selected = !o.uninstall && o.harnesses.iter().any(|h| h == id);
        let has = before.as_deref().is_some_and(|t| crate::instructions::installed_block(t).is_some());
        if !selected && !has {
            continue;
        }
        let base = before.as_deref().unwrap_or("");
        let after = if selected { upsert_block(base, &block) } else { remove_block(base) };
        changes.push(change(
            &format!("instructions:{id}"),
            &format!("{name} instructions"),
            &format!("User-level instructions for {name}. OH MY GOD! adds one marked block that explains when and how agents use the board. Everything else stays as it is."),
            &path,
            before,
            Ok(after),
            !selected,
        ));
    }

    // Claude Code asks before every shell command unless the command is allowed.
    let cpath = claude_home().join("settings.json");
    let cbefore = fs::read_to_string(&cpath).ok();
    let claude = !o.uninstall && o.harnesses.iter().any(|h| h == "claude-code");
    let has_rule = cbefore.as_deref().is_some_and(|s| s.contains(CLAUDE_RULE) || s.contains(LEGACY_CLAUDE_RULE));
    if claude || has_rule {
        changes.push(change(
            "claude-permission",
            "Claude Code permission",
            "Allows Claude Code to run the `ohmygod` command without asking each time. Other settings stay as they are.",
            &cpath,
            cbefore.clone(),
            claude_settings_after(cbefore.as_deref(), !claude),
            !claude,
        ));
    }

    // Earlier versions registered an MCP server. Agents now use the CLI, so the entry goes.
    let mpath = mcp_config_path();
    let mbefore = fs::read_to_string(&mpath).ok();
    let ours = mbefore
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.pointer(&format!("/mcpServers/{SERVER_NAME}")).cloned())
        .is_some_and(|e| e.get("url").and_then(Value::as_str).is_some_and(|u| u.starts_with("http://127.0.0.1:")));
    if ours {
        let after = mcp_cleanup(mbefore.as_deref().unwrap_or("{}"), o.port).map(|(s, _)| s);
        changes.push(change(
            "mcp",
            "Old MCP server entry",
            "Removes the OH MY GOD! MCP server that earlier versions registered. Agents now use the `ohmygod` command instead. Other servers stay as they are.",
            &mpath,
            mbefore,
            after,
            true,
        ));
    }

    let lpath = launch_agent_path();
    let lbefore = fs::read_to_string(&lpath).ok();
    let want = if o.uninstall { None } else { o.login };
    if want.is_some() || lbefore.is_some() {
        let removing = want.is_none();
        changes.push(change(
            "login",
            "Start at login",
            "Opens OH MY GOD! when you log in, so agents can reach the board right away. Turn it off in Setup to remove this file.",
            &lpath,
            lbefore,
            Ok(want.map(launch_agent_plist).unwrap_or_default()),
            removing,
        ));
    }
    // Leftovers from the app's earlier name: its command link and its login item.
    for link in legacy_links() {
        let before = link_target(&link).ok().flatten();
        changes.push(change(
            &format!("legacy-link:{}", link.display()),
            "Old pinrelay command",
            "Removes the `pinrelay` command from the app's earlier name. Agents now use `ohmygod`.",
            &link,
            before,
            Ok(String::new()),
            true,
        ));
    }
    let old_login = legacy_launch_agent_path();
    if let Ok(before) = fs::read_to_string(&old_login) {
        changes.push(change(
            "legacy-login",
            "Old start-at-login item",
            "Removes the login item of the app's earlier name. The new one is listed above when you turn on start at login.",
            &old_login,
            Some(before),
            Ok(String::new()),
            true,
        ));
    }
    let installed = changes.iter().all(|c| c.status == "unchanged") && !o.uninstall;
    Plan { changes, installed }
}

/// Apply approved changes. Each file is re-read and compared to the hash the user approved.
pub fn apply(approved: &[(String, Option<String>)], o: &PlanOptions) -> Result<Vec<String>> {
    let current = plan(o);
    let mut written = vec![];
    // Validate everything before writing anything.
    for (id, sha_seen) in approved {
        let c = current
            .changes
            .iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| anyhow!("Unknown change {id}"))?;
        if c.status == "error" {
            bail!("{}: {}", c.label, c.warning.clone().unwrap_or_default());
        }
        if &c.before_sha != sha_seen {
            bail!("{} changed after you reviewed it. Review the new diff and approve again.", c.path);
        }
    }
    let backups = data_home().join("backups");
    for (id, _) in approved {
        let c = current.changes.iter().find(|c| &c.id == id).unwrap();
        if c.status == "unchanged" {
            continue;
        }
        let path = PathBuf::from(&c.path);
        if id.starts_with("legacy-link:") {
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            }
            written.push(c.path.clone());
            continue;
        }
        if id == "cli" {
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            }
            if c.status != "remove" {
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(c.after.trim(), &path).with_context(|| format!("link {}", path.display()))?;
            }
            written.push(c.path.clone());
            continue;
        }
        if path.exists() {
            fs::create_dir_all(&backups)?;
            let name = format!(
                "{}-{}",
                Utc::now().format("%Y%m%dT%H%M%S"),
                path.file_name().and_then(|s| s.to_str()).unwrap_or("file")
            );
            fs::copy(&path, backups.join(&name)).with_context(|| format!("back up {}", path.display()))?;
        }
        if (id == "login" || id == "legacy-login") && c.status == "remove" {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            written.push(c.path.clone());
            continue;
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mode = fs::metadata(&path).ok().map(|m| m.permissions());
        write_atomic(&path, c.after.as_bytes()).with_context(|| format!("write {}", path.display()))?;
        if let Some(perm) = mode {
            let _ = fs::set_permissions(&path, perm);
        }
        written.push(c.path.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_cleanup_removes_only_our_entry() {
        let before = r#"{"mcpServers":{"a":{"type":"local","command":"x"},"pinrelay":{"type":"http","url":"http://127.0.0.1:47821/mcp"}},"other":1}"#;
        let (after, w) = mcp_cleanup(before, 47821).unwrap();
        assert!(w.is_none());
        let v: Value = serde_json::from_str(&after).unwrap();
        assert!(v["mcpServers"].get("pinrelay").is_none());
        assert_eq!(v["mcpServers"]["a"]["command"], "x");
        assert_eq!(v["other"], 1);
    }

    #[test]
    fn launch_agent_opens_the_bundle() {
        let p = launch_agent_plist(Path::new("/Applications/Pin & Relay.app"));
        assert!(p.contains("<string>/usr/bin/open</string>"));
        assert!(p.contains("<string>/Applications/Pin &amp; Relay.app</string>"));
        assert!(p.contains("<string>dev.ohmygod.desktop</string>"));
        let dev = launch_agent_plist(Path::new("/tmp/ohmygod-app"));
        assert!(!dev.contains("/usr/bin/open") && dev.contains("<string>/tmp/ohmygod-app</string>"));
    }

    #[test]
    fn claude_rule_is_added_once_and_removed() {
        let before = r#"{"model":"opus","permissions":{"allow":["Bash(ls:*)"]}}"#;
        let after = claude_settings_after(Some(before), false).unwrap();
        let again = claude_settings_after(Some(&after), false).unwrap();
        assert_eq!(after, again);
        let v: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["permissions"]["allow"], json!(["Bash(ls:*)", "Bash(ohmygod:*)"]));
        let removed: Value = serde_json::from_str(&claude_settings_after(Some(&after), true).unwrap()).unwrap();
        assert_eq!(removed["permissions"]["allow"], json!(["Bash(ls:*)"]));
        assert!(claude_settings_after(Some("{ nope"), false).is_err());
    }

    #[test]
    fn profile_block_round_trip() {
        let user = "export EDITOR=vim\n";
        let dir = home_dir().join(".local/bin");
        let with = profile_after(user, Some(&dir));
        assert!(with.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert_eq!(profile_after(&with, Some(&dir)), with);
        assert_eq!(profile_after(&with, None), user);
    }

    #[test]
    fn harness_files_follow_each_tool() {
        assert!(harness_instructions_path("claude-code").unwrap().ends_with("CLAUDE.md"));
        for id in ["codex", "opencode", "pi"] {
            assert!(harness_instructions_path(id).unwrap().ends_with("AGENTS.md"));
        }
        assert!(harness_instructions_path("cursor").is_none());
    }

    #[test]
    fn malformed_json_is_not_overwritten() {
        assert!(mcp_cleanup("{ not json", 1).is_err());
    }

    #[test]
    fn foreign_mcp_entry_is_kept() {
        let before = r#"{"mcpServers":{"pinrelay":{"type":"local","command":"other"}}}"#;
        let (kept, w) = mcp_cleanup(before, 47821).unwrap();
        assert!(w.is_some());
        assert_eq!(kept, before);
    }

    #[test]
    fn earlier_name_leftovers_are_replaced() {
        let old = "# pinrelay:begin (managed by the Pinrelay app)\nexport PATH=\"$HOME/.local/bin:$PATH\"\n# pinrelay:end\nalias x=y\n";
        let out = profile_after(old, None);
        assert_eq!(out, "alias x=y\n");
        let claude = r#"{"permissions":{"allow":["Bash(pinrelay:*)","Bash(ls:*)"]}}"#;
        let v: Value = serde_json::from_str(&claude_settings_after(Some(claude), false).unwrap()).unwrap();
        assert_eq!(v["permissions"]["allow"], json!(["Bash(ls:*)", "Bash(ohmygod:*)"]));
        let block = "a\n<!-- pinrelay:begin version=19 (managed by the Pinrelay app) -->\nold\n<!-- pinrelay:end -->\nb\n";
        let next = upsert_block(block, &crate::instructions::instruction_block(Style::Casual, &[], true));
        assert!(!next.contains("pinrelay:begin") && next.matches("ohmygod:begin").count() == 1 && next.starts_with("a\n") && next.ends_with("b\n"));
    }
}
