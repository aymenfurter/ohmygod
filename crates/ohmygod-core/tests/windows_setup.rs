#![cfg(windows)]

use ohmygod_core::{config::Style, model::Features, setup};

#[test]
fn setup_previews_applies_and_removes_windows_launchers_without_symlinks() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("USERPROFILE", home.path());
    std::env::set_var("APPDATA", home.path().join("AppData/Roaming"));
    std::env::set_var("OHMYGOD_HOME", home.path().join(".ohmygod"));
    let cli = home.path().join("App With Spaces/ohmygod.exe");
    std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
    std::fs::write(&cli, "test executable").unwrap();
    let app = cli.with_file_name("ohmygod-app.exe");
    let mut options = setup::PlanOptions {
        style: Style::Casual, uses: &[], visits: false, reputation: false,
        features: Features::default(), port: 47821, uninstall: false,
        login: Some(&app), harnesses: &[], cli: Some(&cli),
    };
    let plan = setup::plan(&options);
    assert!(plan.changes.iter().all(|c| c.status != "error"));
    let approved: Vec<_> = plan.changes.iter().filter(|c| c.id == "cli" || c.id == "login")
        .map(|c| (c.id.clone(), c.before_sha.clone())).collect();
    assert_eq!(approved.len(), 2);
    setup::apply(&approved, &options).unwrap();
    let launcher = setup::cli_link().path;
    assert!(!std::fs::symlink_metadata(&launcher).unwrap().file_type().is_symlink());
    let text = std::fs::read_to_string(&launcher).unwrap();
    assert!(text.contains(&format!("\"{}\" %*", cli.display())));
    assert!(setup::launch_agent_path().exists());
    assert!(setup::plan(&options).changes.iter().filter(|c| c.id == "cli" || c.id == "login").all(|c| c.status == "unchanged"));
    options.uninstall = true;
    let plan = setup::plan(&options);
    let approved: Vec<_> = plan.changes.iter().filter(|c| c.id == "cli" || c.id == "login")
        .map(|c| (c.id.clone(), c.before_sha.clone())).collect();
    setup::apply(&approved, &options).unwrap();
    assert!(!launcher.exists());
    assert!(!setup::launch_agent_path().exists());
    // A foreign command must never be replaced.
    std::fs::write(&launcher, "@echo off\r\necho other program\r\n").unwrap();
    options.uninstall = false;
    assert_eq!(setup::plan(&options).changes.iter().find(|c| c.id == "cli").unwrap().status, "error");
}
