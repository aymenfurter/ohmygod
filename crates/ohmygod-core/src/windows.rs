//! User PATH changes are included in the setup diff and require the same approval.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

pub const PATH_KEY: &str = r"HKEY_CURRENT_USER\Environment\Path";

pub fn user_path() -> Result<Option<String>> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Environment")?;
    match key.get_value("Path") {
        Ok(path) => Ok(Some(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("read user PATH"),
    }
}

pub fn path_after(before: &str, dir: &Path, remove: bool) -> String {
    let directory = dir.to_string_lossy();
    let mut entries: Vec<&str> = before.split(';').filter(|p| !p.trim().is_empty()).collect();
    let matches = |p: &str| p.trim().trim_end_matches('\\').eq_ignore_ascii_case(directory.trim_end_matches('\\'));
    if remove {
        entries.retain(|p| !matches(p));
    } else if !entries.iter().any(|p| matches(p)) {
        entries.push(&directory);
    }
    entries.join(";")
}

pub fn set_user_path(value: &str) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    // Keep expandable entries such as %USERPROFILE% intact.
    let mut bytes: Vec<u8> = value.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    key.set_raw_value("Path", &winreg::RegValue { vtype: winreg::enums::REG_EXPAND_SZ, bytes: std::mem::take(&mut bytes) })?;
    let message: Vec<u16> = "Environment\0".encode_utf16().collect();
    unsafe {
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, message.as_ptr() as isize, SMTO_ABORTIFHUNG, 1000, std::ptr::null_mut());
    }
    Ok(())
}

pub fn cli_directory() -> PathBuf {
    crate::config::home_dir().join(".ohmygod/bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_change_preserves_other_entries_and_is_idempotent() {
        let dir = Path::new(r"C:\Users\Agent\.ohmygod\bin");
        let before = r"%USERPROFILE%\bin;C:\Tools";
        let after = path_after(before, dir, false);
        assert_eq!(path_after(&after, dir, false), after);
        assert_eq!(path_after(&after, dir, true), before);
        assert_eq!(path_after(r"C:\TOOLS;c:\users\agent\.ohmygod\bin\", dir, false), r"C:\TOOLS;c:\users\agent\.ohmygod\bin\");
    }
}
