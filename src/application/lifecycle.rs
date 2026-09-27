use std::sync::Arc;

#[cfg(test)]
use crate::infrastructure::gio_executor::CancellationSignal;
use crate::infrastructure::gio_executor::GioExecutor;
use gio::prelude::ApplicationExt;
use glib::object::IsA;
#[cfg(test)]
use glib::prelude::ObjectExt;

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

#[test]
fn application_shutdown_drains_its_owned_executor() {
    let application = gio::Application::builder()
        .application_id("io.github.explorer.ExecutorLifecycleFixture")
        .build();
    let executor = GioExecutor::for_application();
    let (started, started_receiver) = std::sync::mpsc::sync_channel(1);
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    executor
        .submit(CancellationSignal::new(), {
            let completed = completed.clone();
            move |request| {
                let mut request = Some(request);
                let source = glib::timeout_source_new(
                    std::time::Duration::from_millis(10),
                    None,
                    glib::Priority::DEFAULT,
                    move || {
                        request.take().unwrap().complete();
                        completed.store(true, std::sync::atomic::Ordering::SeqCst);
                        glib::ControlFlow::Break
                    },
                );
                source.attach(Some(&glib::MainContext::ref_thread_default()));
                started.send(()).unwrap();
            }
        })
        .unwrap();
    started_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();

    install_executor_shutdown(&application, executor.clone());
    let adapter_executor = GioExecutor::shared();
    assert!(Arc::ptr_eq(&adapter_executor, &executor));
    drop(adapter_executor);
    application.emit_by_name::<()>("shutdown", &[]);

    assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(executor.active_request_count_for_tests(), 0);
    assert!(executor.is_shutdown_for_tests());
}
