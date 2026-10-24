use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;
use lorin_explorer::core::{DirectoryModel, Location, NavigationHistory};

#[test]
fn home_location_uses_home_directory_and_has_display_name() {
    let location = Location::home();

    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set for this test"));
    assert_eq!(location.local_path().as_deref(), Some(home.as_path()));
    assert!(!location.display_name().is_empty());
}

#[test]
fn directory_model_filters_hidden_entries_by_default() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("visible.txt"), b"visible").unwrap();
    fs::write(directory.path().join(".hidden.txt"), b"hidden").unwrap();

    let entries = DirectoryModel::load(directory.path()).unwrap();

    assert_eq!(
        entries.iter().map(|item| item.name()).collect::<Vec<_>>(),
        vec!["visible.txt"]
    );
}

#[test]
fn directory_model_sorts_folders_first_then_by_name() {
    let directory = tempdir().unwrap();
    fs::create_dir(directory.path().join("zeta")).unwrap();
    fs::write(directory.path().join("Alpha.txt"), b"a").unwrap();
    fs::create_dir(directory.path().join("beta")).unwrap();
    fs::write(directory.path().join("aardvark.txt"), b"a").unwrap();

    let entries = DirectoryModel::load(directory.path()).unwrap();
    let names = entries.iter().map(|item| item.name()).collect::<Vec<_>>();

    assert_eq!(names, vec!["beta", "zeta", "aardvark.txt", "Alpha.txt"]);
}

#[test]
fn navigation_history_supports_back_and_forward_and_discards_forward_branch() {
    let mut history = NavigationHistory::new();
    let home = PathBuf::from("/home/test");
    let documents = home.join("Documents");
    let projects = home.join("Projects");
    let downloads = home.join("Downloads");

    history.push(Location::from_local_path(&home));
    history.push(Location::from_local_path(&documents));
    history.push(Location::from_local_path(&projects));
    assert_eq!(
        history.current(),
        Some(&Location::from_local_path(&projects))
    );
    assert_eq!(history.back(), Some(&Location::from_local_path(&documents)));
    assert_eq!(history.back(), Some(&Location::from_local_path(&home)));
    assert_eq!(
        history.forward(),
        Some(&Location::from_local_path(&documents))
    );

    history.push(Location::from_local_path(&downloads));
    assert_eq!(
        history.current(),
        Some(&Location::from_local_path(&downloads))
    );
    assert_eq!(history.forward(), None);
    assert_eq!(history.back(), Some(&Location::from_local_path(&documents)));
}
