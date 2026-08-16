use adw::prelude::*;
use adw::Application;

fn main() {
    let app = Application::builder()
        .application_id("com.zorin.Explorer")
        .build();

    app.connect_activate(|app| {
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
