pub mod core;
pub mod application {
    pub mod commands;
    pub mod navigation;
    pub mod operations;
    pub mod search;
}
pub mod infrastructure {
    pub mod gio_directory;
    pub mod gio_operations;
    pub mod mock_provider;
    pub mod tracker_search;
}
pub mod settings;
pub mod ui;
