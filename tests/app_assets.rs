use std::fs;
use std::path::Path;

#[test]
fn desktop_entry_uses_the_project_application_icon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let desktop = fs::read_to_string(root.join("resources/casa.alera.LorinExplorer.desktop")).unwrap();

    assert!(desktop
        .lines()
        .any(|line| line == "Icon=casa.alera.LorinExplorer"));
    assert!(root
        .join("resources/icons/hicolor/scalable/apps/casa.alera.LorinExplorer.svg")
        .is_file());
}
