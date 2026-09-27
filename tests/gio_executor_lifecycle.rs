use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};

use gio::prelude::ObjectExt;
use zorin_explorer::{
    application::lifecycle::install_executor_shutdown,
    infrastructure::gio_executor::{CancellationSignal, GioExecutor},
};

fn serial_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn shared_executor_is_process_wide_and_joins_when_no_adapter_owns_it() {
    let _serial = serial_test_guard();
    let first = GioExecutor::shared();
    let first_weak = Arc::downgrade(&first);
    drop(first);

    let second = GioExecutor::shared();
    assert!(first_weak
        .upgrade()
        .is_some_and(|first| Arc::ptr_eq(&first, &second)));

    drop(second);
    assert!(GioExecutor::shutdown_shared_if_idle_for_tests());
    assert!(first_weak.upgrade().is_none());
}

#[test]
fn application_default_is_shared_by_adapters_created_on_other_threads() {
    let _serial = serial_test_guard();
    let executor = GioExecutor::for_application();
    GioExecutor::install_as_application_default(&executor);

    let adapter_executor = std::thread::spawn(GioExecutor::shared)
        .join()
        .expect("adapter construction thread must not panic");
    assert!(Arc::ptr_eq(&adapter_executor, &executor));

    drop(adapter_executor);
    executor.shutdown_for_tests();
}

#[test]
fn application_shutdown_drains_its_owned_executor() {
    let _serial = serial_test_guard();
    let application = gio::Application::builder()
        .application_id("io.github.explorer.ExecutorLifecycleFixture")
        .build();
    let executor = GioExecutor::for_application();
    let (started, started_receiver) = std::sync::mpsc::sync_channel(1);
    let completed = Arc::new(AtomicBool::new(false));

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
                        completed.store(true, Ordering::SeqCst);
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

    assert!(completed.load(Ordering::SeqCst));
    assert_eq!(executor.active_request_count_for_tests(), 0);
    assert!(executor.is_shutdown_for_tests());
}
