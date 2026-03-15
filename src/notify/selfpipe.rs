use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;

use libc::{O_CLOEXEC, O_NONBLOCK, pipe2, read, write};
use nginx_sys::{NGX_OK, ngx_connection_t, ngx_event_t};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use super::ngx_tickle_add_read_event;
use crate::spawn::async_handler;

struct NotifyContext {
    c: ngx_connection_t,
    rev: ngx_event_t,
    wev: ngx_event_t,
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

fn ensure_init() {
    let _ = INIT.get_or_init(|| {
        let mut fds = [0i32; 2];
        let rc = unsafe { pipe2(fds.as_mut_ptr(), O_NONBLOCK | O_CLOEXEC) };

        if rc == -1 {
            panic!("tickle: pipe2 == -1");
        }
        let read_fd = fds[0];
        let write_fd = fds[1];

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
            panic!("tickle: ngx_add_event == {rc}");
        }
    });
}

#[allow(dead_code)]
pub(crate) fn tickle() {
    ensure_init();

    let val: u64 = 1;
    let ptr = &val as *const u64 as *const c_void;
    let rc = unsafe { write(CTX.write_fd, ptr, core::mem::size_of::<u64>()) };
    if rc < 0 {
        panic!("async: self-pipe write failed: {rc}");
    }
    ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (self-pipe)");
}

#[allow(dead_code)]
pub(crate) fn on_tickled() {
    let mut buf: u64 = 0;
    let ptr = &mut buf as *mut u64 as *mut c_void;
    loop {
        let rc = unsafe { read(CTX.read_fd, ptr, core::mem::size_of::<u64>()) };
        if rc <= 0 {
            // EAGAIN / no more data / error → done. We just want to clear readiness
            break;
        }
    }
}
