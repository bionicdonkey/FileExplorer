use gio::prelude::CancellableExt;
use glib::prelude::ObjectType;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
};

static SHARED: OnceLock<Mutex<Option<Arc<GioExecutor>>>> = OnceLock::new();

/// A process-wide owner for native GIO asynchronous work.
///
/// Every submitted job starts on this executor's dedicated thread, whose
/// private `MainContext` remains thread-default until explicit shutdown. A
/// `GioRequest` must be retained by a native completion callback and consumed
/// there, after the callback has released its GIO values.
pub struct GioExecutor {
    context: glib::MainContext,
    state: Arc<ExecutorState>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct ExecutorState {
    main_loop: glib::MainLoop,
    lifecycle: Mutex<Lifecycle>,
}

struct Lifecycle {
    closing: bool,
    next_request: u64,
    live: HashMap<u64, CancellationSignal>,
}
/// A thread-safe cancellation intent that carries no owning GLib reference.
///
/// The requesting context owns its public `gio::Cancellable`; the executor
/// creates and owns the native request cancellable. The signal stores only an
/// executor-published borrowed pointer while that native cancellable is alive.
/// `GCancellable` explicitly supports cancellation from another thread, and
/// the lock guarantees the executor never drops it while a cancellation call
/// is in progress.
#[derive(Clone, Default)]
pub struct CancellationSignal(Arc<CancellationState>);

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    active_cancellable: Mutex<Option<usize>>,
}

struct CancellationLease {
    state: Arc<CancellationState>,
}

impl CancellationSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.cancel_active();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    fn attach(&self, cancellable: &gio::Cancellable) -> CancellationLease {
        let mut active = self.0.active_cancellable.lock().unwrap();
        *active = Some(cancellable.as_ptr() as usize);
        if self.is_cancelled() {
            cancel_native(*active);
        }
        CancellationLease {
            state: self.0.clone(),
        }
    }
}

impl CancellationState {
    fn cancel_active(&self) {
        let active = self.active_cancellable.lock().unwrap();
        cancel_native(*active);
    }
}

impl Drop for CancellationLease {
    fn drop(&mut self) {
        // Holding this lock excludes a cancellation callback that may be using
        // the borrowed pointer. Clear it before `GioRequest` releases the
        // executor-owned `gio::Cancellable`.
        *self.state.active_cancellable.lock().unwrap() = None;
    }
}

fn cancel_native(cancellable: Option<usize>) {
    if let Some(cancellable) = cancellable {
        // `g_cancellable_cancel` is documented as thread-safe. This uses a
        // borrowed pointer only; no GObject reference crosses contexts.
        unsafe { gio::ffi::g_cancellable_cancel(cancellable as *mut gio::ffi::GCancellable) };
    }
}

///
/// The lifetime token for one native GIO request.
///
/// Move this token into the native completion callback. Calling `complete`, or
/// dropping the token, makes shutdown aware that the callback released its
/// request-owned GIO values.
pub struct GioRequest {
    // Drop this lease before `cancellable`, so the bridge cannot observe a
    // freed native object.
    _cancellation_lease: CancellationLease,
    cancellable: gio::Cancellable,
    cancellation: CancellationSignal,
    completion: Option<RequestCompletion>,
}
struct RequestCompletion {
    id: u64,
    state: Arc<ExecutorState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    ShuttingDown,
}

impl GioRequest {
    pub fn cancellable(&self) -> &gio::Cancellable {
        &self.cancellable
    }

    /// Applies a cross-thread cancellation intent from the executor thread.
    pub fn synchronize_cancellation(&self) -> bool {
        if self.cancellation.is_cancelled() {
            self.cancellable.cancel();
        }
        self.cancellable.is_cancelled()
    }

    pub fn complete(mut self) {
        self.completion.take();
    }
}

impl Drop for RequestCompletion {
    fn drop(&mut self) {
        self.state.complete(self.id);
    }
}

impl ExecutorState {
    fn complete(&self, id: u64) {
        let should_quit = {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            lifecycle.live.remove(&id);
            lifecycle.closing && lifecycle.live.is_empty()
        };
        if should_quit {
            self.main_loop.quit();
        }
    }
}

impl GioExecutor {
    pub fn shared() -> Arc<Self> {
        let shared = SHARED.get_or_init(|| Mutex::new(None));
        let mut current = shared.lock().unwrap();
        if let Some(executor) = current.as_ref() {
            return executor.clone();
        }

        let executor = Arc::new(Self::start());
        *current = Some(executor.clone());
        executor
    }

    fn start() -> Self {
        let (ready, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("gio-executor".into())
            .spawn(move || {
                let context = glib::MainContext::new();
                let main_loop = glib::MainLoop::new(Some(&context), false);
                context
                    .with_thread_default(|| {
                        ready
                            .send((context.clone(), main_loop.clone()))
                            .expect("GIO executor starter must receive its private context");
                        main_loop.run();
                    })
                    .expect("GIO executor context must become thread-default");
            })
            .expect("GIO executor thread must start");
        let (context, main_loop) = receiver
            .recv()
            .expect("GIO executor thread must publish its private context");

        Self {
            context,
            state: Arc::new(ExecutorState {
                main_loop,
                lifecycle: Mutex::new(Lifecycle {
                    closing: false,
                    next_request: 0,
                    live: HashMap::new(),
                }),
            }),
            thread: Mutex::new(Some(thread)),
        }
    }

    /// Starts `job` on the executor thread.
    ///
    /// The `GioRequest` passed to the job is the request's native-lifetime
    /// token. Move it into the native completion callback and call `complete`
    /// only after releasing native request values.
    pub fn submit<F>(&self, cancellation: CancellationSignal, job: F) -> Result<(), SubmitError>
    where
        F: FnOnce(GioRequest) + Send + 'static,
    {
        let request = {
            let mut lifecycle = self.state.lifecycle.lock().unwrap();
            if lifecycle.closing {
                return Err(SubmitError::ShuttingDown);
            }
            let id = lifecycle.next_request;
            lifecycle.next_request += 1;
            lifecycle.live.insert(id, cancellation.clone());
            (id, self.state.clone())
        };

        self.context.spawn(async move {
            // This is deliberately inside the executor future: no native
            // cancellable crosses from the requesting context.
            let cancellable = gio::Cancellable::new();
            let request = GioRequest {
                _cancellation_lease: cancellation.attach(&cancellable),
                cancellable,
                cancellation,
                completion: Some(RequestCompletion {
                    id: request.0,
                    state: request.1,
                }),
            };
            request.synchronize_cancellation();
            job(request)
        });

        Ok(())
    }
    /// Cancels all live jobs, drains their completion callbacks, and joins the
    /// private executor thread. This is intentionally explicit: it never
    /// switches work to the process-global default context.
    #[doc(hidden)]
    pub fn shutdown_for_tests(&self) {
        let (cancellables, should_quit) = {
            let mut lifecycle = self.state.lifecycle.lock().unwrap();
            if lifecycle.closing {
                return;
            }
            lifecycle.closing = true;
            (
                lifecycle.live.values().cloned().collect::<Vec<_>>(),
                lifecycle.live.is_empty(),
            )
        };

        for cancellation in cancellables {
            cancellation.cancel();
        }
        if should_quit {
            self.state.main_loop.quit();
        }

        if let Some(thread) = self.thread.lock().unwrap().take() {
            thread.join().expect("GIO executor thread must not panic");
        }
        Self::clear_shared(self);
    }

    fn clear_shared(this: &Self) {
        let mut shared = SHARED.get_or_init(|| Mutex::new(None)).lock().unwrap();
        if shared
            .as_ref()
            .is_some_and(|executor| std::ptr::eq(Arc::as_ptr(executor), this))
        {
            *shared = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Mutex, OnceLock,
        },
        time::Duration,
    };

    fn serial_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn submitted_callback_observes_executor_context_as_thread_default() {
        let _serial = serial_test_guard();
        let executor = GioExecutor::shared();
        let expected_context = executor.context.clone();
        let (sender, receiver) = mpsc::sync_channel(1);

        executor
            .submit(CancellationSignal::new(), move |request| {
                let on_executor_context =
                    glib::MainContext::ref_thread_default() == expected_context;
                sender.send(on_executor_context).unwrap();
                request.complete();
            })
            .unwrap();

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(true));
        executor.shutdown_for_tests();
    }

    #[test]
    fn shared_executor_remains_process_wide_until_explicit_shutdown() {
        let _serial = serial_test_guard();
        let first = GioExecutor::shared();
        let first_weak = Arc::downgrade(&first);
        drop(first);

        let second = GioExecutor::shared();
        let retained = first_weak
            .upgrade()
            .is_some_and(|first| Arc::ptr_eq(&first, &second));
        second.shutdown_for_tests();

        assert!(retained);
    }

    #[test]
    fn shutdown_cancels_queued_work_and_joins_after_completion_callbacks_run() {
        let _serial = serial_test_guard();
        let executor = GioExecutor::shared();
        let (started, started_receiver) = mpsc::sync_channel(1);
        let callbacks = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));

        executor
            .submit(CancellationSignal::new(), {
                let callbacks = callbacks.clone();
                let cancelled = cancelled.clone();
                move |request| {
                    let mut request = Some(request);
                    let source = glib::timeout_source_new(
                        Duration::from_millis(25),
                        None,
                        glib::Priority::DEFAULT,
                        move || {
                            let request = request
                                .take()
                                .expect("completion source must run only once");
                            if request.cancellable().is_cancelled() {
                                cancelled.fetch_add(1, Ordering::SeqCst);
                            }
                            callbacks.fetch_add(1, Ordering::SeqCst);
                            request.complete();
                            glib::ControlFlow::Break
                        },
                    );
                    source.attach(Some(&glib::MainContext::ref_thread_default()));
                    started.send(()).unwrap();
                }
            })
            .unwrap();

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        executor.shutdown_for_tests();

        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    }
}
