use adw::prelude::*;
use adw::Application;
use zorin_explorer::{
    application::lifecycle::install_executor_shutdown, infrastructure::gio_executor::GioExecutor,
};

fn main() {
    // Re-entry point for the polkit-backed elevated retry: `pkexec` launches
    // this same binary with `--elevated-op`, which performs exactly one
    // validated local operation and exits without opening a window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = zorin_explorer::core::elevated_operations::run_from_args(&args) {
        std::process::exit(code);
    }

    let app = Application::builder()
        .application_id("com.zorin.Explorer")
        .build();
    let executor = GioExecutor::for_application();
    install_executor_shutdown(&app, executor.clone());

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
        let window = zorin_explorer::ui::window::ExplorerWindow::new(app);
        window.present();
    });

    app.run();
}
