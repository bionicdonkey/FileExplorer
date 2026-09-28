use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use gio::prelude::{ApplicationExt, ApplicationExtManual, CancellableExt};
use gtk::prelude::GtkWindowExt;
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

#[gtk::test]
fn application_shutdown_drains_its_owned_executor() {
    let _serial = serial_test_guard();
    let application = gtk::Application::builder()
        .application_id("io.github.explorer.ExecutorLifecycleFixture")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
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

    let shutdown = install_executor_shutdown(&application, executor.clone());
    let adapter_executor = GioExecutor::shared();
    assert!(Arc::ptr_eq(&adapter_executor, &executor));
    drop(adapter_executor);
    application.connect_activate(move |_| shutdown.quit());
    application.run_with_args::<&str>(&[]);

    let deadline = Instant::now() + Duration::from_secs(3);
    while !completed.load(Ordering::SeqCst) || !executor.is_shutdown_for_tests() {
        assert!(
            Instant::now() < deadline,
            "executor shutdown must drain before application teardown completes"
        );
        std::thread::yield_now();
    }

    assert!(completed.load(Ordering::SeqCst));
    assert_eq!(executor.active_request_count_for_tests(), 0);
    assert!(executor.is_shutdown_for_tests());
}

#[gtk::test]
fn application_shutdown_keeps_owner_context_running_until_mount_callback_releases_lease() {
    let _serial = serial_test_guard();
    let context = glib::MainContext::default();
    let application = gtk::Application::builder()
        .application_id("io.github.explorer.ExecutorOwnerMountShutdownFixture")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let executor = GioExecutor::for_application();
    let (handoff_send, handoff_receive) = mpsc::sync_channel(1);
    executor
        .submit(CancellationSignal::new(), move |request| {
            handoff_send.send(request.into_owner_handoff()).unwrap();
        })
        .unwrap();

    let owner_cancellable = gio::Cancellable::new();
    let owner_request = handoff_receive
        .recv_timeout(Duration::from_secs(1))
        .expect("owner mount lifecycle must retain the executor request")
        .begin_owner_mount(&owner_cancellable);
    let callback_released = Arc::new(AtomicBool::new(false));
    let callback_request = Rc::new(RefCell::new(Some(owner_request)));
    let callback_request_on_owner = callback_request.clone();
    let callback_released_on_owner = callback_released.clone();
    let callback_cancellable = owner_cancellable.clone();
    glib::idle_add_local(move || {
        if callback_cancellable.is_cancelled() {
            callback_request_on_owner
                .borrow_mut()
                .take()
                .expect("owner mount callback must release its lease once")
                .complete();
            callback_released_on_owner.store(true, Ordering::SeqCst);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    let shutdown = install_executor_shutdown(&application, executor.clone());
    application.connect_activate(move |_| shutdown.quit());
    application.run_with_args::<&str>(&[]);

    let deadline = Instant::now() + Duration::from_secs(3);
    while !callback_released.load(Ordering::SeqCst) || !executor.is_shutdown_for_tests() {
        assert!(
            Instant::now() < deadline,
            "shutdown must complete after owner callback release"
        );
        context.iteration(false);
        std::thread::yield_now();
    }

    assert_eq!(executor.active_request_count_for_tests(), 0);
}

#[gtk::test]
fn programmatic_quit_drains_owner_mount_before_application_quits() {
    let _serial = serial_test_guard();
    let application = gtk::Application::builder()
        .application_id("io.github.explorer.ExecutorPreQuitOwnerMountFixture")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let executor = GioExecutor::for_application();
    let (handoff_send, handoff_receive) = mpsc::sync_channel(1);
    executor
        .submit(CancellationSignal::new(), move |request| {
            handoff_send.send(request.into_owner_handoff()).unwrap();
        })
        .unwrap();

    let owner_cancellable = gio::Cancellable::new();
    let owner_request = handoff_receive
        .recv_timeout(Duration::from_secs(1))
        .expect("owner mount lifecycle must retain the executor request")
        .begin_owner_mount(&owner_cancellable);
    let callback_released = Arc::new(AtomicBool::new(false));
    let callback_request = Rc::new(RefCell::new(Some(owner_request)));
    let callback_request_on_owner = callback_request.clone();
    let callback_released_on_owner = callback_released.clone();
    let callback_cancellable = owner_cancellable.clone();
    glib::idle_add_local(move || {
        if callback_cancellable.is_cancelled() {
            callback_request_on_owner
                .borrow_mut()
                .take()
                .expect("owner mount completion must release its lease once")
                .complete();
            callback_released_on_owner.store(true, Ordering::SeqCst);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    let shutdown = install_executor_shutdown(&application, executor.clone());
    let shutdown_after_callback = Arc::new(AtomicBool::new(false));
    application.connect_shutdown({
        let callback_released = callback_released.clone();
        let shutdown_after_callback = shutdown_after_callback.clone();
        move |_| {
            shutdown_after_callback
                .store(callback_released.load(Ordering::SeqCst), Ordering::SeqCst)
        }
    });
    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::new(application);
        window.present();
        shutdown.quit();
    });

    application.run_with_args::<&str>(&[]);

    assert!(callback_released.load(Ordering::SeqCst));
    assert!(shutdown_after_callback.load(Ordering::SeqCst));
    assert_eq!(executor.active_request_count_for_tests(), 0);
    assert!(executor.is_shutdown_for_tests());
}

#[gtk::test]
fn closing_one_window_waits_for_the_final_window_before_draining_owner_mount() {
    let _serial = serial_test_guard();
    let application = gtk::Application::builder()
        .application_id("io.github.explorer.ExecutorMultiWindowPreQuitFixture")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let executor = GioExecutor::for_application();
    let (handoff_send, handoff_receive) = mpsc::sync_channel(1);
    executor
        .submit(CancellationSignal::new(), move |request| {
            handoff_send.send(request.into_owner_handoff()).unwrap();
        })
        .unwrap();

    let owner_cancellable = gio::Cancellable::new();
    let owner_request = handoff_receive
        .recv_timeout(Duration::from_secs(1))
        .expect("owner mount lifecycle must retain the executor request")
        .begin_owner_mount(&owner_cancellable);
    let callback_released = Arc::new(AtomicBool::new(false));
    let callback_request = Rc::new(RefCell::new(Some(owner_request)));
    let callback_request_on_owner = callback_request.clone();
    let callback_released_on_owner = callback_released.clone();
    let callback_cancellable = owner_cancellable.clone();
    glib::idle_add_local(move || {
        if callback_cancellable.is_cancelled() {
            callback_request_on_owner
                .borrow_mut()
                .take()
                .expect("owner mount completion must release its lease once")
                .complete();
            callback_released_on_owner.store(true, Ordering::SeqCst);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    let _shutdown = install_executor_shutdown(&application, executor.clone());
    let first_close_kept_executor_alive = Arc::new(AtomicBool::new(false));
    let shutdown_after_callback = Arc::new(AtomicBool::new(false));
    application.connect_shutdown({
        let callback_released = callback_released.clone();
        let shutdown_after_callback = shutdown_after_callback.clone();
        move |_| {
            shutdown_after_callback
                .store(callback_released.load(Ordering::SeqCst), Ordering::SeqCst)
        }
    });
    let first_close_kept_executor_alive_on_activate = first_close_kept_executor_alive.clone();
    application.connect_activate(move |application| {
        let first = gtk::ApplicationWindow::new(application);
        let second = gtk::ApplicationWindow::new(application);
        first.connect_close_request({
            let owner_cancellable = owner_cancellable.clone();
            let first_close_kept_executor_alive =
                first_close_kept_executor_alive_on_activate.clone();
            move |_| {
                first_close_kept_executor_alive
                    .store(!owner_cancellable.is_cancelled(), Ordering::SeqCst);
                glib::Propagation::Proceed
            }
        });
        first.present();
        second.present();
        first.close();
        glib::idle_add_local_once(move || second.close());
    });

    application.run_with_args::<&str>(&[]);

    let callback_released_before_shutdown = shutdown_after_callback.load(Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !executor.is_shutdown_for_tests() {
        assert!(
            Instant::now() < deadline,
            "test cleanup must drain the executor"
        );
        glib::MainContext::default().iteration(false);
        std::thread::yield_now();
    }

    assert!(first_close_kept_executor_alive.load(Ordering::SeqCst));
    assert!(callback_released_before_shutdown);
    assert_eq!(executor.active_request_count_for_tests(), 0);
}
