use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::infrastructure::gio_executor::GioExecutor;
use gio::prelude::{ActionMapExt, ApplicationExt, ApplicationExtManual};
use glib::object::IsA;
use glib::prelude::ObjectExt;
use gtk::prelude::{GtkApplicationExt, GtkWindowExt};

/// Connects application termination to the executor instance owned by that
/// application. The executor is not the process-wide fallback, so active
/// adapters in another owner can never be cancelled by this hook.
pub fn install_executor_shutdown(
    application: &impl IsA<gtk::Application>,
    executor: Arc<GioExecutor>,
) -> ApplicationShutdown {
    GioExecutor::install_as_application_default(&executor);
    let shutdown = ApplicationShutdown {
        executor: executor.clone(),
        application: application.as_ref().downgrade(),
        drain_requested: Arc::new(AtomicBool::new(false)),
        close_permitted: Arc::new(AtomicBool::new(false)),
    };

    for window in application.windows() {
        shutdown.defer_window_close(&window);
    }
    application.connect_window_added({
        let shutdown = shutdown.clone();
        move |_, window| {
            // ExplorerWindow installs its active-operation confirmation while it
            // is constructed. Attach afterwards so a Keep Waiting response
            // vetoes this lifecycle handler before it can begin shutdown.
            let shutdown = shutdown.clone();
            let window = window.clone();
            glib::idle_add_local_once(move || shutdown.defer_window_close(&window));
        }
    });
    shutdown
}

/// Coordinates application-owned executor shutdown with the active GTK close
/// request, so owner-context mount callbacks can release their final leases.
#[derive(Clone)]
pub struct ApplicationShutdown {
    executor: Arc<GioExecutor>,
    application: glib::WeakRef<gtk::Application>,
    drain_requested: Arc<AtomicBool>,
    close_permitted: Arc<AtomicBool>,
}

impl ApplicationShutdown {
    /// Requests application termination through the pre-quit drain path.
    ///
    /// Call this instead of `ApplicationExt::quit` so the GTK owner context
    /// remains active until owner-mount completions release their leases.
    pub fn quit(&self) {
        self.begin_drain(DrainCompletion::QuitApplication);
    }

    /// Installs the only production programmatic quit entry point. The raw
    /// GTK quit call remains private to the post-drain completion below.
    pub fn install_quit_action(&self) {
        let Some(application) = self.application.upgrade() else {
            return;
        };
        let action = gio::SimpleAction::new("quit", None);
        let shutdown = self.clone();
        action.connect_activate(move |_, _| shutdown.quit());
        application.add_action(&action);
    }

    /// Runs the application while this coordinator remains owned by main.
    pub fn run(&self) {
        if let Some(application) = self.application.upgrade() {
            application.run();
        }
    }

    fn defer_window_close(&self, window: &gtk::Window) {
        let shutdown = self.clone();
        window.connect_close_request(move |window| {
            if shutdown.close_permitted.load(Ordering::Acquire) {
                return glib::Propagation::Proceed;
            }
            let Some(application) = shutdown.application.upgrade() else {
                return glib::Propagation::Stop;
            };
            if application.windows().len() > 1 {
                return glib::Propagation::Proceed;
            }

            shutdown.begin_drain(DrainCompletion::CloseWindow(window.downgrade()));
            glib::Propagation::Stop
        });
    }

    fn begin_drain(&self, completion: DrainCompletion) {
        if self.drain_requested.swap(true, Ordering::AcqRel) {
            return;
        }

        self.executor.shutdown_async();
        let executor = self.executor.clone();
        let close_permitted = self.close_permitted.clone();
        let application = self.application.clone();
        glib::timeout_add_local(Duration::from_millis(5), move || {
            if !executor.shutdown_complete() {
                return glib::ControlFlow::Continue;
            }

            close_permitted.store(true, Ordering::Release);
            match completion {
                DrainCompletion::CloseWindow(ref window) => {
                    if let Some(window) = window.upgrade() {
                        window.close();
                    }
                }
                DrainCompletion::QuitApplication => {
                    if let Some(application) = application.upgrade() {
                        application.quit();
                    }
                }
            }
            glib::ControlFlow::Break
        });
    }
}

enum DrainCompletion {
    CloseWindow(glib::WeakRef<gtk::Window>),
    QuitApplication,
}
