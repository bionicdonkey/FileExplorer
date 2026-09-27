use std::sync::Arc;

use crate::infrastructure::gio_executor::GioExecutor;
use gio::prelude::ApplicationExt;
use glib::object::IsA;

/// Connects application termination to the executor instance owned by that
/// application. The executor is not the process-wide fallback, so active
/// adapters in another owner can never be cancelled by this hook.
pub fn install_executor_shutdown(
    application: &impl IsA<gio::Application>,
    executor: Arc<GioExecutor>,
) {
    GioExecutor::install_as_application_default(&executor);
    application.connect_shutdown(move |_| executor.shutdown());
}
