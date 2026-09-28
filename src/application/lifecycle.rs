use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::infrastructure::gio_executor::GioExecutor;
use gio::prelude::ApplicationExt;
use glib::object::IsA;
use gtk::prelude::GtkWindowExt;

/// Connects application termination to the executor instance owned by that
/// application. The executor is not the process-wide fallback, so active
/// adapters in another owner can never be cancelled by this hook.
pub fn install_executor_shutdown(
    application: &impl IsA<gio::Application>,
    executor: Arc<GioExecutor>,
) -> ApplicationShutdown {
    GioExecutor::install_as_application_default(&executor);
    let shutdown = ApplicationShutdown {
        executor: executor.clone(),
        close_requested: Arc::new(AtomicBool::new(false)),
        close_permitted: Arc::new(AtomicBool::new(false)),
    };

    // This is only a noninteractive fallback. The normal window-close path
    // below begins draining while the GTK owner context still pumps.
    application.connect_shutdown(move |_| executor.shutdown_async());
    shutdown
}

/// Coordinates application-owned executor shutdown with the active GTK close
/// request, so owner-context mount callbacks can release their final leases.
#[derive(Clone)]
pub struct ApplicationShutdown {
    executor: Arc<GioExecutor>,
    close_requested: Arc<AtomicBool>,
    close_permitted: Arc<AtomicBool>,
}

impl ApplicationShutdown {
    /// Defers one top-level window close until native executor and owner-mount
    /// requests drain. The window remains alive, and its GTK context keeps
    /// processing the mount completion that releases the final request lease.
    pub fn defer_window_close(&self, window: &impl IsA<gtk::Window>) {
        let executor = self.executor.clone();
        let close_requested = self.close_requested.clone();
        let close_permitted = self.close_permitted.clone();
        window.connect_close_request(move |window| {
            if close_permitted.load(Ordering::Acquire) {
                return glib::Propagation::Proceed;
            }
            if close_requested.swap(true, Ordering::AcqRel) {
                return glib::Propagation::Stop;
            }

            executor.shutdown_async();
            let close_window = window.clone();
            let executor = executor.clone();
            let close_permitted = close_permitted.clone();
            glib::timeout_add_local(Duration::from_millis(5), move || {
                if executor.shutdown_complete() {
                    close_permitted.store(true, Ordering::Release);
                    close_window.close();
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
            glib::Propagation::Stop
        });
    }
}
