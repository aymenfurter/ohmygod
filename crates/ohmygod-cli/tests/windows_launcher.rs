#![cfg(windows)]

use ohmygod_core::{config::Style, model::Features, setup};

#[test]
fn installed_launcher_has_console_output_and_handles_spaces_and_exclamation_marks() {
    let home = tempfile::Builder::new().prefix("OH MY GOD! CLI ").tempdir().unwrap();
    std::env::set_var("USERPROFILE", home.path());
    std::env::set_var("APPDATA", home.path().join("AppData/Roaming"));
    std::env::set_var("OHMYGOD_HOME", home.path().join(".ohmygod"));
    let options = setup::PlanOptions {
        style: Style::Casual, uses: &[], visits: false, reputation: false,
        features: Features::default(), port: 47821, uninstall: false,
        login: None, harnesses: &[], cli: Some(std::path::Path::new(env!("CARGO_BIN_EXE_ohmygod"))),
    };
    let plan = setup::plan(&options);
    let command = plan.changes.iter().find(|c| c.id == "cli").unwrap();
    setup::apply(&[(command.id.clone(), command.before_sha.clone())], &options).unwrap();
    let output = std::process::Command::new(setup::cli_link().path).arg("--version").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), format!("ohmygod {}", env!("CARGO_PKG_VERSION")));
}
