use gio::prelude::CancellableExt;
use glib::prelude::ObjectType;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, OnceLock, Weak,
    },
    thread::{self, JoinHandle},
};

static SHARED: OnceLock<Mutex<Option<Arc<GioExecutor>>>> = OnceLock::new();
static APPLICATION_DEFAULT: OnceLock<Mutex<Option<Weak<GioExecutor>>>> = OnceLock::new();

/// A process-wide owner for native GIO asynchronous work.
///
/// Every submitted job starts on one of this executor's dedicated worker
/// threads, whose private `MainContext` remains thread-default until explicit
/// shutdown. A `GioRequest` must be retained by a native completion callback
/// and consumed there, after the callback has released its GIO values.
///
/// Jobs run their native GIO calls synchronously, so a single worker would let
/// one slow transfer or remote enumeration head-of-line block every other
/// listing, metadata and search request. The executor therefore owns a small
/// pool that shares one job queue: a worker only takes a job while it is
/// otherwise idle, so a blocking job cannot starve work that another worker
/// could run. Each job still runs entirely on the worker that took it, so its
/// GIO values are created, used and released on one thread-default context,
/// while lifecycle accounting, cancellation and shutdown stay executor-wide.
pub struct GioExecutor {
    workers: Vec<Worker>,
    state: Arc<ExecutorState>,
}

struct Worker {
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// One queued job, run on whichever worker takes it.
type Job = Box<dyn FnOnce() + Send>;

struct ExecutorState {
    /// Worker contexts, used only to wake idle workers.
    contexts: Vec<glib::MainContext>,
    queue: Mutex<VecDeque<Job>>,
    lifecycle: Mutex<Lifecycle>,
    join_complete: AtomicBool,
}

struct Lifecycle {
    closing: bool,
    next_request: u64,
    live: HashMap<u64, CancellationSignal>,
}
/// Owner-context lifetime accounting for executor requests.
///
/// A `futures_channel` sender that the executor uses to publish DTOs holds a
/// `Waker` backed by a GLib source owned by the *receiving* context, and both
/// sending and dropping that sender unref one of those sources from an executor
/// worker thread. glib requires the receiving task to be finalized on the
/// thread that owns its `MainContext`, so an owner must keep its context alive
/// until every executor request that can still touch it has released its
/// bridge, and must perform the final `MainContext` unref itself.
///
/// Adapters hold this guard and take one lease per submitted request. Dropping
/// the last guard handle drains, which is why an owner that simply drops its
/// adapter before its context is already safe.
#[derive(Clone)]
pub struct OwnerContextGuard {
    outstanding: Arc<AtomicUsize>,
    context: glib::MainContext,
    handle: Arc<()>,
}

/// One outstanding executor reference into an owner context.
pub struct OwnerContextLease {
    outstanding: Arc<AtomicUsize>,
}

/// The executor half of one executor-to-owner DTO bridge.
///
/// Dropping this releases the DTO sender first, then the owner context it wakes,
/// and only then the owner's drain lease. That order is what keeps the owner's
/// final `MainContext` unref on the owner thread.
pub struct OwnerBridge<T> {
    sender: Option<futures_channel::mpsc::UnboundedSender<T>>,
    context: Option<glib::MainContext>,
    lease: Option<OwnerContextLease>,
}

impl OwnerContextGuard {
    /// Creates the guard for adapters delivering results to `context`.
    pub fn new(context: glib::MainContext) -> Self {
        Self {
            outstanding: Arc::new(AtomicUsize::new(0)),
            context,
            handle: Arc::new(()),
        }
    }

    /// Opens the executor half of a bridge that publishes `T` to this owner.
    pub fn bridge<T>(&self, sender: futures_channel::mpsc::UnboundedSender<T>) -> OwnerBridge<T> {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        OwnerBridge {
            sender: Some(sender),
            context: Some(self.context.clone()),
            lease: Some(OwnerContextLease {
                outstanding: self.outstanding.clone(),
            }),
        }
    }

    /// The number of executor requests that still hold a reference into this
    /// owner context.
    pub fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }

    /// Blocks until no executor request holds a reference into this owner
    /// context.
    ///
    /// This deliberately does not iterate the owner context: a closing owner
    /// must not start dispatching late DTOs to callbacks it is tearing down. No
    /// bridge needs owner-side progress to be released, so waiting is enough.
    pub fn drain(&self) {
        while self.outstanding() != 0 {
            thread::yield_now();
        }
    }
}

impl Drop for OwnerContextGuard {
    fn drop(&mut self) {
        // Only the last adapter handle stands between the executor and this
        // owner context being finalized, so only it has to wait.
        if Arc::strong_count(&self.handle) == 1 {
            self.drain();
        }
    }
}

impl Drop for OwnerContextLease {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<T> OwnerBridge<T> {
    /// Publishes one DTO to the owner context.
    pub fn send(&self, value: T) {
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.unbounded_send(value);
        }
    }
}

impl<T> Drop for OwnerBridge<T> {
    fn drop(&mut self) {
        // The sender's last waker unref must happen while the owner context is
        // still referenced here, and the owner may only finish draining once
        // this half holds nothing of theirs.
        self.sender.take();
        self.context.take();
        self.lease.take();
    }
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

/// The lifecycle half of an executor request while native work is running on
/// the requesting main context. It deliberately owns no GLib or GTK values.
pub struct OwnerMountHandoff {
    cancellation: CancellationSignal,
    completion: Option<RequestCompletion>,
}

/// Retains an executor request until an owner-context mount callback has
/// released its native GTK/GIO values.
pub struct OwnerMountRequest {
    // Drop the cancellation bridge before the owner callback releases its
    // cancellable, so shutdown cannot observe a freed native object.
    _cancellation_lease: CancellationLease,
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

    /// Releases the executor-native request and retains only its lifecycle
    /// accounting while an owner-context mount operation is in flight.
    pub fn into_owner_handoff(mut self) -> OwnerMountHandoff {
        OwnerMountHandoff {
            cancellation: self.cancellation.clone(),
            completion: self.completion.take(),
        }
    }
}

impl OwnerMountHandoff {
    /// Begins the owner-context half of a mount request. Executor shutdown
    /// cancels this cancellable and waits for `OwnerMountRequest::complete`.
    pub fn begin_owner_mount(self, cancellable: &gio::Cancellable) -> OwnerMountRequest {
        OwnerMountRequest {
            _cancellation_lease: self.cancellation.attach(cancellable),
            completion: self.completion,
        }
    }
}

impl OwnerMountRequest {
    /// Marks the owner-context mount callback and all of its native values as
    /// released. Dropping the request has the same lifecycle effect.
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
        self.lifecycle.lock().unwrap().live.remove(&id);
        // An idle worker is blocked in its context poll, so it has to be woken
        // to observe that the executor may now stop.
        self.wake_workers();
    }

    fn wake_workers(&self) {
        for context in &self.contexts {
            context.wakeup();
        }
    }

    fn take_job(&self) -> Option<Job> {
        self.queue.lock().unwrap().pop_front()
    }

    /// The executor may stop only once shutdown has begun and every request,
    /// including any still queued, has released its native values.
    fn should_stop(&self) -> bool {
        let lifecycle = self.lifecycle.lock().unwrap();
        lifecycle.closing && lifecycle.live.is_empty()
    }

    /// One worker: flush native completion callbacks, then take at most one job
    /// at a time so a blocking job never hides queued work from idle workers.
    fn run_worker(&self, context: &glib::MainContext) {
        loop {
            while context.pending() {
                context.iteration(false);
            }
            if let Some(job) = self.take_job() {
                job();
                continue;
            }
            if self.should_stop() {
                return;
            }
            context.iteration(true);
        }
    }
}

impl GioExecutor {
    /// Creates the executor owned by one application instance.
    ///
    /// Application startup registers this instance as the default for its
    /// production adapters, leaving the process-wide fallback untouched.
    pub fn for_application() -> Arc<Self> {
        Arc::new(Self::start())
    }

    /// Makes an application-owned executor the default used by production
    /// adapters constructed during that application's lifetime.
    pub fn install_as_application_default(executor: &Arc<Self>) {
        *APPLICATION_DEFAULT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(Arc::downgrade(executor));
    }

    pub fn shared() -> Arc<Self> {
        if let Some(executor) = APPLICATION_DEFAULT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
        {
            return executor;
        }
        let shared = SHARED.get_or_init(|| Mutex::new(None));
        let mut current = shared.lock().unwrap();
        if let Some(executor) = current.as_ref() {
            return executor.clone();
        }

        let executor = Arc::new(Self::start());
        *current = Some(executor.clone());
        executor
    }

    /// The number of worker threads a pool-backed executor starts.
    ///
    /// Native GIO jobs block their worker, so the pool must be wide enough that
    /// a slow transfer cannot stall unrelated listings, while staying small
    /// enough to keep shutdown accounting and thread footprint predictable.
    fn default_workers() -> usize {
        thread::available_parallelism()
            .map_or(4, |parallelism| parallelism.get())
            .clamp(2, 8)
    }

    fn start() -> Self {
        Self::start_with_workers(Self::default_workers())
    }

    fn start_with_workers(workers: usize) -> Self {
        let workers = workers.max(1);
        let mut contexts = Vec::with_capacity(workers);
        let mut starters = Vec::with_capacity(workers);
        let mut threads = Vec::with_capacity(workers);
        for index in 0..workers {
            let (publish, published) = mpsc::sync_channel(1);
            let (start, begin) = mpsc::sync_channel::<Arc<ExecutorState>>(1);
            let thread = thread::Builder::new()
                .name(format!("gio-executor-{index}"))
                .spawn(move || {
                    let context = glib::MainContext::new();
                    context
                        .with_thread_default(|| {
                            publish
                                .send(context.clone())
                                .expect("GIO executor starter must receive its private context");
                            let Ok(state) = begin.recv() else {
                                return;
                            };
                            state.run_worker(&context);
                        })
                        .expect("GIO executor context must become thread-default");
                })
                .expect("GIO executor thread must start");
            contexts.push(
                published
                    .recv()
                    .expect("GIO executor thread must publish its private context"),
            );
            starters.push(start);
            threads.push(thread);
        }

        let state = Arc::new(ExecutorState {
            contexts,
            queue: Mutex::new(VecDeque::new()),
            lifecycle: Mutex::new(Lifecycle {
                closing: false,
                next_request: 0,
                live: HashMap::new(),
            }),
            join_complete: AtomicBool::new(false),
        });
        for starter in starters {
            starter
                .send(state.clone())
                .expect("GIO executor worker must receive its shared state");
        }

        Self {
            workers: threads
                .into_iter()
                .map(|thread| Worker {
                    thread: Mutex::new(Some(thread)),
                })
                .collect(),
            state,
        }
    }

    #[doc(hidden)]
    pub fn isolated_for_tests() -> Arc<Self> {
        Arc::new(Self::start())
    }

    #[doc(hidden)]
    pub fn isolated_with_workers_for_tests(workers: usize) -> Arc<Self> {
        Arc::new(Self::start_with_workers(workers))
    }

    #[doc(hidden)]
    pub fn worker_count_for_tests(&self) -> usize {
        self.workers.len()
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

        let queued: Job = Box::new(move || {
            // This is deliberately inside the queued job: no native cancellable
            // crosses from the requesting context.
            let cancellable = gio::Cancellable::new();
            let native_request = GioRequest {
                _cancellation_lease: cancellation.attach(&cancellable),
                cancellable,
                cancellation,
                completion: Some(RequestCompletion {
                    id: request.0,
                    state: request.1,
                }),
            };
            native_request.synchronize_cancellation();
            job(native_request)
        });
        self.state.queue.lock().unwrap().push_back(queued);
        self.state.wake_workers();

        Ok(())
    }
    /// Test-only lifecycle observation for adapters that must prove a terminal
    /// DTO follows native request release.
    #[doc(hidden)]
    pub fn active_request_count_for_tests(&self) -> usize {
        self.state.lifecycle.lock().unwrap().live.len()
    }

    /// Reports that the executor thread has drained and joined.
    pub fn shutdown_complete(&self) -> bool {
        self.state.join_complete.load(Ordering::Acquire)
    }

    #[doc(hidden)]
    pub fn is_shutdown_for_tests(&self) -> bool {
        self.shutdown_complete()
    }

    /// Detaches and joins the shared executor only when no adapter still owns
    /// it and no native request is live. This lets lifecycle tests clean up
    /// deterministically without cancelling work owned by another test.
    #[doc(hidden)]
    pub fn shutdown_shared_if_idle_for_tests() -> bool {
        let executor = {
            let mut shared = SHARED.get_or_init(|| Mutex::new(None)).lock().unwrap();
            let Some(executor) = shared.as_ref() else {
                return true;
            };
            if Arc::strong_count(executor) != 1
                || !executor.state.lifecycle.lock().unwrap().live.is_empty()
            {
                return false;
            }
            shared
                .take()
                .expect("the shared executor must remain present while locked")
        };
        executor.shutdown_for_tests();
        true
    }

    /// Immediately prevents new work and cancels every native and
    /// owner-context request. Each worker loop continues until every native
    /// completion callback has released its request values.
    fn begin_shutdown(&self) -> bool {
        let cancellables = {
            let mut lifecycle = self.state.lifecycle.lock().unwrap();
            if lifecycle.closing {
                return false;
            }
            lifecycle.closing = true;
            lifecycle.live.values().cloned().collect::<Vec<_>>()
        };

        for cancellation in cancellables {
            cancellation.cancel();
        }
        self.state.wake_workers();

        true
    }

    /// Joins only after every worker loop has drained its native requests.
    fn join_after_drain(&self) {
        for worker in &self.workers {
            if let Some(thread) = worker.thread.lock().unwrap().take() {
                thread.join().expect("GIO executor thread must not panic");
            }
        }
        Self::clear_shared(self);
        Self::clear_application_default(self);
        self.state.join_complete.store(true, Ordering::Release);
    }

    /// Cancels live jobs, drains native completion callbacks, and joins the
    /// private executor threads without switching work to the process-global
    /// default context.
    pub fn shutdown(&self) {
        if self.begin_shutdown() {
            self.join_after_drain();
        }
    }

    /// Starts shutdown without blocking the caller's GLib owner context.
    ///
    /// This is required for application shutdown because an owner-context
    /// mount callback may still hold the final request lease needed to stop
    /// the executor loops. The join and final executor teardown run after that
    /// callback has drained, on a helper thread.
    pub fn shutdown_async(self: &Arc<Self>) {
        if !self.begin_shutdown() {
            return;
        }

        let executor = self.clone();
        thread::Builder::new()
            .name("gio-executor-shutdown".into())
            .spawn(move || executor.join_after_drain())
            .expect("GIO executor shutdown thread must start");
    }

    #[doc(hidden)]
    pub fn shutdown_for_tests(&self) {
        self.shutdown();
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

    fn clear_application_default(this: &Self) {
        let mut application = APPLICATION_DEFAULT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap();
        if application
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|executor| std::ptr::eq(Arc::as_ptr(&executor), this))
        {
            *application = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gio::prelude::CancellableExtManual;
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc,
        },
        thread,
        time::Duration,
    };

    #[test]
    fn submitted_callback_observes_a_worker_context_as_thread_default() {
        let executor = GioExecutor::isolated_for_tests();
        let worker_contexts = executor.state.contexts.clone();
        let (sender, receiver) = mpsc::sync_channel(1);

        executor
            .submit(CancellationSignal::new(), move |request| {
                let on_worker_context = worker_contexts
                    .iter()
                    .any(|context| *context == glib::MainContext::ref_thread_default());
                sender.send(on_worker_context).unwrap();
                request.complete();
            })
            .unwrap();

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(true));
        executor.shutdown_for_tests();
    }

    #[test]
    fn a_blocking_job_does_not_delay_work_submitted_behind_it() {
        // One worker would serialize these, so the second job could not report
        // progress until the first released its thread.
        let executor = GioExecutor::isolated_for_tests();
        assert!(executor.worker_count_for_tests() > 1);
        let release = Arc::new(AtomicBool::new(false));
        let (blocked, blocked_receiver) = mpsc::sync_channel(1);
        let (overtaken, overtaken_receiver) = mpsc::sync_channel(1);

        executor
            .submit(CancellationSignal::new(), {
                let release = release.clone();
                move |request| {
                    blocked.send(()).unwrap();
                    while !release.load(Ordering::SeqCst) {
                        thread::yield_now();
                    }
                    request.complete();
                }
            })
            .unwrap();
        blocked_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("the blocking job must start");

        executor
            .submit(CancellationSignal::new(), move |request| {
                overtaken.send(()).unwrap();
                request.complete();
            })
            .unwrap();

        let overtook = overtaken_receiver.recv_timeout(Duration::from_secs(2));
        release.store(true, Ordering::SeqCst);
        assert_eq!(
            overtook,
            Ok(()),
            "a blocking native job must not head-of-line block later requests"
        );
        executor.shutdown_for_tests();
    }

    #[test]
    fn shutdown_cancels_queued_work_and_joins_after_completion_callbacks_run() {
        let executor = GioExecutor::isolated_for_tests();
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

    #[test]
    fn shutdown_during_owner_mount_waits_for_the_owner_callback_to_release_native_values() {
        let executor = GioExecutor::isolated_for_tests();
        let (handoff_send, handoff_receive) = mpsc::sync_channel(1);

        executor
            .submit(CancellationSignal::new(), move |request| {
                handoff_send.send(request.into_owner_handoff()).unwrap();
            })
            .unwrap();

        let owner_cancellable = gio::Cancellable::new();
        let owner_request = handoff_receive
            .recv_timeout(Duration::from_secs(1))
            .expect("mount handoff must retain the executor request")
            .begin_owner_mount(&owner_cancellable);
        let (cancelled_send, cancelled_receive) = mpsc::sync_channel(1);
        owner_cancellable.connect_cancelled(move |_| cancelled_send.send(()).unwrap());

        let executor_for_shutdown = executor.clone();
        let shutdown = std::thread::spawn(move || executor_for_shutdown.shutdown_for_tests());
        cancelled_receive
            .recv_timeout(Duration::from_secs(1))
            .expect("executor shutdown must cancel the owner-context mount");
        assert!(
            !shutdown.is_finished(),
            "shutdown must not join before the owner mount callback releases its request"
        );

        owner_request.complete();
        shutdown.join().unwrap();
        assert_eq!(executor.active_request_count_for_tests(), 0);
    }
}
