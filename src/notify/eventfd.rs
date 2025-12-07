use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;

use nginx_sys::{
    EFD_CLOEXEC, EFD_NONBLOCK, EPOLL_EVENTS_EPOLLET, EPOLL_EVENTS_EPOLLIN, EPOLL_EVENTS_EPOLLRDHUP,
    NGX_OK, eventfd, ngx_connection_t, ngx_event_actions, ngx_event_t, read, write,
};
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

extern "C" fn _dummy_write_handler(_ev: *mut ngx_event_t) {}

fn ensure_init() {
    let _ = INIT.get_or_init(|| {
        let fd = unsafe { eventfd(0, (EFD_NONBLOCK | EFD_CLOEXEC).try_into().unwrap()) };

        if fd == -1 {
            panic!("tickle: eventfd = -1");
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
        ctx.wev.handler = Some(_dummy_write_handler); // can't be null
        let rc = unsafe {
            ngx_event_actions.add.unwrap()(
                &raw mut ctx.rev,
                (EPOLL_EVENTS_EPOLLIN | EPOLL_EVENTS_EPOLLRDHUP) as isize,
                EPOLL_EVENTS_EPOLLET as usize,
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

    let val: u64 = 1;
    let ptr = &val as *const u64 as *const c_void;
    let res = unsafe { write(CTX.fd, ptr, core::mem::size_of::<u64>()) };
    if res != core::mem::size_of::<u64>() as isize {
        panic!("tickle: eventfd write failed: {res}");
    }

    ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (eventfd)");
}

/// drain eventfd — called from async_handler
pub(crate) fn on_tickled() {
    let mut buf: u64 = 0;
    let ptr = &mut buf as *mut u64 as *mut c_void;
    let _ = unsafe { read(CTX.fd, ptr, core::mem::size_of::<u64>()) };
}
