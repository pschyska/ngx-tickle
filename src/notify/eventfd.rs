use std::mem;
use std::sync::OnceLock;

use libc::{O_CLOEXEC, O_NONBLOCK, eventfd, eventfd_read, eventfd_t, eventfd_write};
use nginx_sys::{NGX_OK, ngx_connection_t, ngx_event_t};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::tickle_abort;
use crate::{notify::ngx_tickle_add_read_event, spawn::async_handler};

struct NotifyContext {
    c: ngx_connection_t,
    rev: ngx_event_t,
    wev: ngx_event_t,
    fd: i32,
}
static mut CTX: NotifyContext = NotifyContext {
    c: unsafe { mem::zeroed() },
    rev: unsafe { mem::zeroed() },
    wev: unsafe { mem::zeroed() },
    fd: -1,
};

static INIT: OnceLock<()> = OnceLock::new();

/// Register the wakeup eventfd on this worker's event loop. Idempotent. Must run on the
/// nginx main thread, in a worker/single process — see [`crate::init`], which guards the
/// process type and is the only caller.
#[allow(dead_code)]
pub(crate) fn init() {
    let _ = INIT.get_or_init(|| {
        let fd = unsafe { eventfd(0, O_NONBLOCK | O_CLOEXEC) };
        if fd == -1 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            tickle_abort!("tickle: eventfd failed, errno={errno:?}");
        }

        #[allow(clippy::deref_addrof)]
        let ctx = unsafe { &mut *&raw mut CTX };

        let log = ngx_cycle_log().as_ptr();

        ctx.fd = fd;

        ctx.c.log = log;
        ctx.c.fd = fd;
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

        ngx_log_debug!(log, "tickle: initialized (eventfd)");
    });
}

/// Abort if [`init`] hasn't run in this process. Called on the wakeup path instead of
/// lazily initializing, so a missing `ngx_tickle::init()` (or a `spawn()` in the master
/// process) fails loudly rather than registering the fd from the wrong thread/process.
#[allow(dead_code)]
fn expect_init() {
    if INIT.get().is_none() {
        tickle_abort!(
            "tickle: not initialized — call ngx_tickle::init() from your module's init_process (see ngx_tickle::init docs)"
        );
    }
}

#[allow(dead_code)]
pub(crate) fn tickle() {
    expect_init();

    loop {
        let rc = unsafe { eventfd_write(CTX.fd, 1) };

        if rc == 0 {
            ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (eventfd)");
            return;
        }

        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return, // eventfd full → pending
            errno => {
                tickle_abort!("tickle: eventfd_write failed, errno={errno:?}");
            }
        }
    }
}

/// drain eventfd — called from async_handler
#[allow(dead_code)]
pub(crate) fn on_tickled() {
    let mut _val: eventfd_t = 0;

    loop {
        let rc = unsafe { eventfd_read(CTX.fd, &raw mut _val) };

        if rc == 0 {
            return;
        }

        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return, // already drained
            errno => {
                tickle_abort!("tickle: eventfd_read failed, errno={errno:?}");
            }
        }
    }
}
