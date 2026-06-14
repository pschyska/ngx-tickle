use std::mem;
use std::sync::OnceLock;

use nginx_sys::{NGX_OK, ngx_connection_t, ngx_event_t};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::spawn::async_handler;
use crate::tickle_abort;

// The two backends are nearly identical: they differ only in the wakeup fd and the three
// syscalls that touch it. The shared context, init/wiring, and the tickle/drain plumbing
// live here; each backend provides just `open`/`signal`/`drain` (+ a `NAME` for logs).
// This keeps the rarely-exercised self-pipe path running the exact same wiring as eventfd.
#[cfg(ngx_os = "linux")]
pub(crate) mod eventfd;
#[cfg(all(not(feature = "selfpipe"), ngx_os = "linux"))]
use eventfd as backend;

pub(crate) mod selfpipe;
#[cfg(any(feature = "selfpipe", not(ngx_os = "linux")))]
use selfpipe as backend;

unsafe extern "C" {
    // ../ffi/expand.c
    fn ngx_tickle_add_read_event(ev: *mut ngx_event_t) -> nginx_sys::ngx_int_t;
}

struct NotifyContext {
    c: ngx_connection_t,
    rev: ngx_event_t,
    wev: ngx_event_t,
    // For eventfd these are the same fd; for the self-pipe they're the two pipe ends.
    read_fd: i32,
    write_fd: i32,
}
static mut CTX: NotifyContext = NotifyContext {
    c: unsafe { mem::zeroed() },
    rev: unsafe { mem::zeroed() },
    wev: unsafe { mem::zeroed() },
    read_fd: -1,
    write_fd: -1,
};

static INIT: OnceLock<()> = OnceLock::new();

/// Open the backend wakeup fd and register its readable end on this worker's event loop.
/// Idempotent. Must run on the nginx main thread in a worker/single process — see
/// [`crate::init`], the only caller.
pub(crate) fn init() {
    let _ = INIT.get_or_init(|| {
        let (read_fd, write_fd) = backend::open();

        #[allow(clippy::deref_addrof)]
        let ctx = unsafe { &mut *&raw mut CTX };

        let log = ngx_cycle_log().as_ptr();

        ctx.read_fd = read_fd;
        ctx.write_fd = write_fd;

        ctx.c.log = log;
        ctx.c.fd = read_fd;
        ctx.c.read = &raw mut ctx.rev;
        ctx.c.write = &raw mut ctx.wev;

        ctx.rev.log = log;
        ctx.rev.data = (&raw mut ctx.c).cast();
        ctx.rev.handler = Some(async_handler);

        ctx.wev.log = log;
        ctx.wev.data = (&raw mut ctx.c).cast();

        let rc = unsafe { ngx_tickle_add_read_event(&raw mut ctx.rev) };
        if rc != NGX_OK as isize {
            tickle_abort!("tickle: ngx_add_event rc={rc}");
        }

        ngx_log_debug!(log, "tickle: initialized ({})", backend::NAME);
    });
}

/// Abort if [`init`] hasn't run in this process. Called on the wakeup path instead of
/// lazily initializing, so a missing `ngx_tickle::init()` fails loudly rather than
/// registering the fd from the wrong thread/process.
fn expect_init() {
    if INIT.get().is_none() {
        tickle_abort!(
            "tickle: not initialized — call ngx_tickle::init() from your module's init_process (see ngx_tickle::init docs)"
        );
    }
}

/// Wake the nginx event loop. Safe from any thread once [`init`] has run.
pub(crate) fn tickle() {
    expect_init();
    backend::signal(unsafe { CTX.write_fd });
}

/// Drain the wakeup fd. Called by `async_handler`, which only runs after [`init`].
pub(crate) fn on_tickled() {
    backend::drain(unsafe { CTX.read_fd });
}
