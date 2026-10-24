use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use gio::prelude::{ActionGroupExt, ApplicationExt, ApplicationExtManual, CancellableExt};
use gtk::prelude::GtkWindowExt;
use lorin_explorer::{
    application::lifecycle::install_executor_shutdown,
    core::{
        contracts::{DirectoryRequest, PageRequest, RequestId},
        ports::DirectoryRepository,
        Location,
    },
    infrastructure::gio_directory::GioDirectoryRepository,
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
    shutdown.install_quit_action();
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
        application.activate_action("quit", None);
    });

    application.run_with_args::<&str>(&[]);

    assert!(callback_released.load(Ordering::SeqCst));
    assert!(shutdown_after_callback.load(Ordering::SeqCst));
    assert_eq!(executor.active_request_count_for_tests(), 0);
    assert!(executor.is_shutdown_for_tests());
}

#[gtk::test]
fn close_veto_runs_before_lifecycle_drain_and_keep_waiting_keeps_executor_operational() {
    let _serial = serial_test_guard();
    let application = gtk::Application::builder()
        .application_id("io.github.explorer.ExecutorCloseVetoFixture")
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
    let owner_request = Rc::new(RefCell::new(Some(owner_request)));
    let shutdown = install_executor_shutdown(&application, executor.clone());
    let kept_waiting = Arc::new(AtomicBool::new(false));
    let kept_waiting_on_activate = kept_waiting.clone();
    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::new(application);
        window.connect_close_request({
            let kept_waiting = kept_waiting_on_activate.clone();
            move |_| {
                kept_waiting.store(true, Ordering::SeqCst);
                glib::Propagation::Stop
            }
        });
        window.present();
        glib::idle_add_local_once(move || window.close());
        let shutdown = shutdown.clone();
        let owner_request = owner_request.clone();
        let owner_cancellable = owner_cancellable.clone();
        let kept_waiting_after_veto = kept_waiting_on_activate.clone();
        glib::timeout_add_local_once(Duration::from_millis(30), move || {
            assert!(kept_waiting_after_veto.load(Ordering::SeqCst));
            assert!(
                !owner_cancellable.is_cancelled(),
                "keeping the close confirmation open must not cancel active work"
            );
            owner_request
                .borrow_mut()
                .take()
                .expect("test owner request must still be live")
                .complete();
            shutdown.quit();
        });
    });

    application.run_with_args::<&str>(&[]);

    assert!(kept_waiting.load(Ordering::SeqCst));
    assert!(executor.is_shutdown_for_tests());
}

#[test]
fn shutdown_cancels_large_paged_listing_when_owner_context_never_iterates() {
    let _serial = serial_test_guard();
    let directory = tempfile::tempdir().unwrap();
    for index in 0..(128 * 3 + 1) {
        std::fs::write(directory.path().join(format!("entry-{index:04}")), b"x").unwrap();
    }

    let context = glib::MainContext::new();
    let _owner = context.acquire().unwrap();
    let executor = GioExecutor::isolated_for_tests();
    let repository = GioDirectoryRepository::with_executor(context.clone(), executor.clone());
    repository.list_pages(
        DirectoryRequest {
            id: RequestId(41),
            location: Location::from_local_path(directory.path()),
            page: PageRequest {
                offset: 0,
                limit: usize::MAX,
            },
            cancellable: gio::Cancellable::new(),
        },
        Box::new(|_| panic!("an uniterated owner context must not receive pages")),
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while executor.active_request_count_for_tests() == 0 {
        assert!(
            Instant::now() < deadline,
            "listing request must reach executor"
        );
        std::thread::yield_now();
    }

    let (finished, finished_receiver) = mpsc::sync_channel(1);
    let executor_for_shutdown = executor.clone();
    std::thread::spawn(move || {
        executor_for_shutdown.shutdown_for_tests();
        finished.send(()).unwrap();
    });
    finished_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("shutdown must cancel a producer blocked behind unconsumed pages");
    assert_eq!(executor.active_request_count_for_tests(), 0);
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
