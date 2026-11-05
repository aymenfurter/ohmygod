use serde::Serialize;

#[derive(Serialize)]
pub struct AboutInfo {
    name: &'static str,
    version: &'static str,
    revision: &'static str,
    platform: &'static str,
    architecture: &'static str,
    license: &'static str,
}

#[tauri::command]
pub fn about_info() -> AboutInfo {
    AboutInfo {
        name: "OH MY GOD!",
        version: env!("CARGO_PKG_VERSION"),
        revision: env!("OHMYGOD_REVISION"),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        license: env!("CARGO_PKG_LICENSE"),
    }
}

#[tauri::command]
pub fn open_releases() -> Result<(), String> {
    ohmygod_core::platform::open_path(std::path::Path::new(
        "https://github.com/aymenfurter/ohmygod/releases",
    )).map_err(|e| format!("Cannot open the releases page: {e}"))
}
