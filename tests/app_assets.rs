use std::fs;
use std::path::Path;

#[test]
fn desktop_entry_uses_the_project_application_icon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let desktop = fs::read_to_string(root.join("resources/com.zorin.Explorer.desktop")).unwrap();

    assert!(desktop
        .lines()
        .any(|line| line == "Icon=com.zorin.Explorer"));
    assert!(root
        .join("resources/icons/hicolor/scalable/apps/com.zorin.Explorer.svg")
        .is_file());
}

#[test]
fn flatpak_manifest_allows_a_gtk_window_to_connect_to_the_display() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        fs::read_to_string(root.join("packaging/flatpak/com.zorin.Explorer.yml")).unwrap();

    assert!(manifest.contains("- --share=ipc"));
    assert!(manifest.contains("- --socket=wayland"));
    assert!(manifest.contains("- --socket=fallback-x11"));
}
