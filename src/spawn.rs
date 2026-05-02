use std::error::Error;
use std::fmt::Display;
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::task::{Context, Poll};

/// Re-export of [`async_task::Task`]. Returned by [`spawn()`] as the handle for a
/// top-level task. For request-bound tasks see [`RequestTask`].
pub use async_task::Task;
use async_task::{Runnable, ScheduleInfo, WithInfo};
use crossbeam_channel::{Receiver, Sender, unbounded};
use nginx_sys::{ngx_event_t, ngx_thread_tid};
use ngx::http::Request;
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::notify::*;

static MAIN_TID: AtomicI64 = AtomicI64::new(-1);

#[inline]
pub fn on_main_thread() -> bool {
    let main_tid = MAIN_TID.load(Ordering::Relaxed);
    let tid: i64 = unsafe { ngx_thread_tid().into() };
    main_tid == tid
}

static MAX_RUNNABLES_PER_WAKEUP: AtomicU32 = AtomicU32::new(8);

/// Set the maximum number of processed runnables per wakeup. Might starve nginx native events if
/// set too high.
/// Only applies to off-thread and reentrant wakeups — runnables are run in-place otherwise.
///
/// Default: 8
pub fn set_max_runnables_per_wakeup(value: u32) {
    MAX_RUNNABLES_PER_WAKEUP.store(value, Ordering::Relaxed);
}

pub(crate) extern "C" fn async_handler(_ev: *mut ngx_event_t) {
    on_tickled();
    // initialize MAIN_TID on first execution
    let tid = unsafe { ngx_thread_tid().into() };
    let _ = MAIN_TID.compare_exchange(-1, tid, Ordering::Relaxed, Ordering::Relaxed);
    let limit = MAX_RUNNABLES_PER_WAKEUP.load(Ordering::Relaxed);

    let scheduler = scheduler();

    if scheduler.rx.is_empty() {
        return;
    }
    let mut cnt = 0;
    while let Ok(r) = scheduler.rx.try_recv() {
        r.run();
        cnt += 1;
        if cnt >= limit {
            ngx_log_debug!(
                ngx_cycle_log().as_ptr(),
                "tickle: suspend processing after {limit} items"
            );
            // re-schedule ourselves
            tickle();
            return;
        }
    }
    ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: processed {cnt} items");
}

struct Scheduler {
    rx: Receiver<Runnable>,
    tx: Sender<Runnable>,
}

impl Scheduler {
    fn new() -> Self {
        let (tx, rx) = unbounded();
        Scheduler { tx, rx }
    }

    fn schedule(&self, runnable: Runnable, info: ScheduleInfo) {
        let main = on_main_thread();
        // If we are on the main thread it's safe to simply run the Runnable, otherwise we enqueue
        // the Runnable and tickle nginx. The event handler then runs it on the main thread.
        //
        // If woken_while_running, it indicates that a task has yielded itself to the Scheduler.
        // Force round-trip via queue to limit reentrancy.
        if main && !info.woken_while_running {
            runnable.run();
        } else {
            self.tx.send(runnable).expect("send");

            tickle();
        }
    }
}

static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

fn scheduler() -> &'static Scheduler {
    SCHEDULER.get_or_init(Scheduler::new)
}

fn schedule(runnable: Runnable, info: ScheduleInfo) {
    let scheduler = scheduler();
    scheduler.schedule(runnable, info);
}

/// Spawn a top-level task on the nginx event loop. Returns a [`Task<T>`] handle.
///
/// The future must be `'static`. Use this for background work that outlives the
/// originating request (e.g. caches set up from `init_process`) or for futures that
/// don't need to borrow request data. For most in-request async work, prefer
/// [`RequestSpawn::spawn`] which handles request-bound lifetimes for you.
pub fn spawn<F, T>(future: F) -> Task<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: spawning new task");
    let (runnable, task) = unsafe { async_task::spawn_unchecked(future, WithInfo(schedule)) };
    runnable.schedule();
    task
}

#[derive(Debug)]
pub struct AllocationFailed;

impl Display for AllocationFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unable to allocate Task in request pool")
    }
}

impl Error for AllocationFailed {}

/// Spawn a request-bound task. The task is anchored in the request pool and is
/// cancelled (its future dropped) when the request is torn down.
///
/// Returns a `&mut RequestTask` handle that, while alive, holds the borrow of the
/// request — so you can't touch the request directly until the handle goes out of
/// scope, is awaited, or is cancelled.
///
/// Two intended usage patterns:
///
/// - **Main task (from a sync entry point)**: spawn from `http_request_handler!` and
///   ignore the handle. The task continues running, anchored in the pool, and tears
///   down with the request.
/// - **Subtask (from inside another task)**: await the handle for the result, or call
///   [`RequestTask::cancel`] to abort early.
///
/// Returns `Err(AllocationFailed)` only if the request pool refuses to register the
/// cleanup (effectively OOM).
///
/// # Examples
///
/// ```ignore
/// async fn handler(req: &mut Request) -> Status { Status::NGX_OK }
///
/// // Main task: ignore the handle, return NGX_AGAIN.
/// http_request_handler!(my_handler, |request: &mut http::Request| {
///     if request.spawn(handler).is_err() {
///         return Status::NGX_ERROR;
///     }
///     Status::NGX_AGAIN
/// });
///
/// // Subtask: await for the result.
/// async fn outer(request: &mut Request) {
///     let result = request.spawn(handler).unwrap().await;
/// }
/// ```
///
/// # Notes
///
/// Pool cleanups run LIFO. If your future captures owned values whose `Drop`
/// touches other pool-allocated state, ordering may matter. Capturing references
/// into the pool, or types with trivial `Drop`, is always fine.
pub trait RequestSpawn {
    /// Spawn a request-bound task. See the [trait docs](RequestSpawn) for usage
    /// patterns and lifetime semantics.
    fn spawn<F, T>(&mut self, f: F) -> Result<&mut RequestTask<T>, AllocationFailed>
    where
        F: AsyncFnOnce(&mut Self) -> T;
}

impl RequestSpawn for Request {
    fn spawn<F, T>(&mut self, f: F) -> Result<&mut RequestTask<T>, AllocationFailed>
    where
        F: AsyncFnOnce(&mut Self) -> T,
    {
        ngx_log_debug!(
            ngx_cycle_log().as_ptr(),
            "tickle: spawning new request-bound task"
        );
        let pool = self.pool();
        let fut = f(self);
        let (runnable, task) = unsafe { async_task::spawn_unchecked(fut, WithInfo(schedule)) };
        // Anchor the Task in the request pool: pool cleanup runs `drop_in_place`
        // on `ngx_destroy_pool`, dropping the task and cancelling the runnable.
        let slot = pool.allocate(RequestTask { task: Some(task) });
        if slot.is_null() {
            return Err(AllocationFailed);
        }
        runnable.schedule();
        Ok(unsafe { &mut *slot })
    }
}

/// Handle to a request-bound task spawned via [`RequestSpawn::spawn`].
///
/// While the handle (`&mut RequestTask`) is in scope, it holds the borrow of the
/// request — visible to the type system through the lifetime on the reference. The
/// request cannot be touched directly until the handle goes out of scope.
///
/// Dropping the handle (letting it go out of scope) does **not** cancel the task:
/// the `RequestTask` value lives in the request pool. The task either runs to
/// completion or is cancelled at request teardown. To cancel earlier, use
/// [`cancel`](Self::cancel).
pub struct RequestTask<T> {
    // `Option` so that `cancel(&mut self)` can move the `Task` out by value
    // (`Task::cancel` consumes self) without leaving the field in a moved-out
    // state that would be double-dropped. Niche-optimized: same size as `Task<T>`.
    task: Option<Task<T>>,
}

// `async_task::Task` is a small handle (a `NonNull` plus phantoms), not a state
// machine — `Unpin`. We forward that.
impl<T> Unpin for RequestTask<T> {}

impl<T> RequestTask<T> {
    /// `true` once the task has completed or been cancelled.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(Task::is_finished)
    }

    /// Cancel the task. Returns the output if it happened to complete before the
    /// cancellation took effect. After this returns, awaiting the handle panics.
    pub async fn cancel(&mut self) -> Option<T> {
        match self.task.take() {
            Some(t) => t.cancel().await,
            None => None,
        }
    }
}

impl<T> Future for RequestTask<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let task = self
            .get_mut()
            .task
            .as_mut()
            .expect("RequestTask polled after cancel");
        Pin::new(task).poll(cx)
    }
}
