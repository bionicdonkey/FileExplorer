use adw::prelude::*;
use adw::Application;
use lorin_explorer::{
    application::lifecycle::install_executor_shutdown, infrastructure::gio_executor::GioExecutor,
};

fn main() {
    // Re-entry point for the polkit-backed elevated retry: `pkexec` launches
    // this same binary with `--elevated-op`, which performs exactly one
    // validated local operation and exits without opening a window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = lorin_explorer::core::elevated_operations::run_from_args(&args) {
        std::process::exit(code);
    }
    // A stale binary from an older source tree behaves differently enough to
    // mislead bug reports, so builds identify themselves on request.
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("lorin-explorer {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Settings from before the rename (see the function) move over before anything reads them.
    if let Err(error) = lorin_explorer::core::application_preferences::migrate_legacy_config_dir() {
        eprintln!("lorin-explorer: could not move the old settings folder: {error}");
    }

    let app = Application::builder()
        .application_id("casa.alera.LorinExplorer")
        .build();
    let executor = GioExecutor::for_application();
    let shutdown = install_executor_shutdown(&app, executor.clone());
    shutdown.install_quit_action();

    app.connect_activate(move |app| {
        if let Some(display) = gtk::gdk::Display::default() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(include_str!("../resources/style.css"));
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        // A development build shows the icon from this source tree rather than whichever
        // version is installed system-wide. (The shell's taskbar icon still comes from the
        // installed desktop file; only icons the app itself draws, like the About page, change.)
        #[cfg(debug_assertions)]
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/icons"));
        }
        let window = lorin_explorer::ui::window::ExplorerWindow::new(app);
        window.present();
    });

    shutdown.run();
}
