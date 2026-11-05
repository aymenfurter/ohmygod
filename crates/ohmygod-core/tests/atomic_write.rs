#[test]
fn atomic_write_replaces_existing_files_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    ohmygod_core::store::write_atomic(&path, b"before").unwrap();
    ohmygod_core::store::write_atomic(&path, b"after").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"after");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
