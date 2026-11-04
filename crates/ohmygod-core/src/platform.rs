//! Platform services shared by setup and the desktop app.

use std::path::Path;
use std::process::Command;

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let command = Command::new(program);
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    };
    command
}

pub fn open_path(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = command("open");
    #[cfg(windows)]
    let mut cmd = command("explorer.exe");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = command("xdg-open");
    #[cfg(target_os = "macos")]
    if path.is_file() {
        cmd.arg("-R");
    }
    cmd.arg(path).spawn()?;
    Ok(())
}
