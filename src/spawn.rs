use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};

pub use async_task::Task;
use async_task::{Runnable, ScheduleInfo, WithInfo};
use crossbeam_channel::{Receiver, Sender, unbounded};
use nginx_sys::{ngx_event_t, ngx_thread_tid};
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

pub(crate) extern "C" fn async_handler(_ev: *mut ngx_event_t) {
    // initialize MAIN_TID on first execution
    let tid = unsafe { ngx_thread_tid().into() };
    let _ = MAIN_TID.compare_exchange(-1, tid, Ordering::Relaxed, Ordering::Relaxed);

    on_tickled();

    let scheduler = scheduler();

    if scheduler.rx.is_empty() {
        return;
    }
    let mut cnt = 0;
    while let Ok(r) = scheduler.rx.try_recv() {
        r.run();
        cnt += 1;
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
        // If we are on the event loop thread it's safe to simply run the Runnable, otherwise we
        // enqueue the Runnable and tickle nginx. The event handler then runs it on the main thread.
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

/// Creates a new task running on the NGINX event loop.
/// The Scheduler is thread-safe. In particular, schedule() may be called from any thread.
/// The Runnables are always .run() on the event loop thread.
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
