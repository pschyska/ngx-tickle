use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;

use libc::{
    EPOLLET, EPOLLIN, EPOLLRDHUP, O_CLOEXEC, O_NONBLOCK, eventfd, eventfd_read, eventfd_t,
    eventfd_write,
};
use nginx_sys::{NGX_OK, ngx_connection_t, ngx_event_actions, ngx_event_t};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::spawn::async_handler;

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

fn ensure_init() {
    let _ = INIT.get_or_init(|| {
        let fd = unsafe { eventfd(0, (O_NONBLOCK | O_CLOEXEC).try_into().unwrap()) };

        if fd == -1 {
            panic!("tickle: eventfd == -1");
        }

        #[allow(clippy::deref_addrof)]
        let ctx = unsafe { &mut *&raw mut CTX };

        let log = ngx_cycle_log().as_ptr();

        ctx.c.log = log;
        ctx.c.fd = fd;
        ctx.c.read = &raw mut ctx.rev;
        ctx.c.write = &raw mut ctx.wev;

        ctx.rev.log = log;
        ctx.rev.data = (&raw mut ctx.c).cast();
        ctx.rev.set_active(1);
        ctx.rev.handler = Some(async_handler);

        ctx.wev.log = log;
        ctx.wev.data = (&raw mut ctx.c).cast();
        let rc = unsafe {
            ngx_event_actions.add.unwrap()(
                &raw mut ctx.rev,
                (EPOLLIN | EPOLLRDHUP) as isize,
                EPOLLET as usize,
            )
        };
        if rc != NGX_OK as isize {
            panic!("tickle: ngx_add_event rc={rc}");
        }

        ctx.fd = fd;
    });
}

pub(crate) fn tickle() {
    ensure_init();

    let res = unsafe { eventfd_write(CTX.fd, 1) };
    if res != 0 {
        panic!("tickle: eventfd write failed: {res}");
    }

    ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (eventfd)");
}

/// drain eventfd — called from async_handler
pub(crate) fn on_tickled() {
    let mut buf: eventfd_t = 0;
    let _ = unsafe { eventfd_read(CTX.fd, &raw mut buf) };
}
