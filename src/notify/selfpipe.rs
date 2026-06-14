use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;

use libc::{
    F_GETFD, F_GETFL, F_SETFD, F_SETFL, FD_CLOEXEC, O_NONBLOCK, c_int, fcntl, pipe, read, write,
};
use nginx_sys::{NGX_OK, ngx_connection_t, ngx_event_t};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use super::ngx_tickle_add_read_event;
use crate::spawn::async_handler;
use crate::tickle_abort;

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

fn set_nonblocking(fd: c_int) {
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags == -1 {
        tickle_abort!("tickle: fcntl(F_GETFL) failed");
    }

    let rc = unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
    if rc == -1 {
        tickle_abort!("tickle: fcntl(F_SETFL, O_NONBLOCK) failed");
    }
}

fn set_cloexec(fd: c_int) {
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags == -1 {
        tickle_abort!("tickle: fcntl(F_GETFD) failed");
    }

    let rc = unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) };
    if rc == -1 {
        tickle_abort!("tickle: fcntl(F_SETFD, FD_CLOEXEC) failed");
    }
}

fn make_pipe() -> [c_int; 2] {
    let mut fds = [-1 as c_int; 2];

    let rc = unsafe { pipe(fds.as_mut_ptr()) };
    if rc == -1 {
        tickle_abort!("tickle: pipe == -1");
    }

    set_nonblocking(fds[0]);
    set_nonblocking(fds[1]);
    set_cloexec(fds[0]);
    set_cloexec(fds[1]);

    fds
}

/// Register the wakeup self-pipe on this worker's event loop. Idempotent. Must run on the
/// nginx main thread, in a worker/single process — see [`crate::init`], which guards the
/// process type and is the only caller.
#[allow(dead_code)]
pub(crate) fn init() {
    let _ = INIT.get_or_init(|| {
        let fds = make_pipe();
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
            tickle_abort!("tickle: ngx_add_event == {rc}");
        }

        ngx_log_debug!(log, "tickle: initialized (self-pipe)");
    });
}

/// Abort if [`init`] hasn't run in this process. Called on the wakeup path instead of
/// lazily initializing, so a missing `ngx_tickle::init()` (or a `spawn()` in the master
/// process) fails loudly rather than registering the fds from the wrong thread/process.
#[allow(dead_code)]
fn expect_init() {
    if INIT.get().is_none() {
        tickle_abort!(
            "tickle: not initialized — call ngx_tickle::init() from your module's init_process (see ngx_tickle::init docs)"
        );
    }
}

fn is_again(errno: Option<i32>) -> bool {
    matches!(errno, Some(e) if e == libc::EAGAIN || e == libc::EWOULDBLOCK)
}

#[allow(dead_code)]
pub(crate) fn tickle() {
    expect_init();

    let byte: u8 = 1;
    let ptr = &byte as *const u8 as *const c_void;

    loop {
        let rc = unsafe { write(CTX.write_fd, ptr, 1) };

        if rc == 1 {
            ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (self-pipe)");
            return;
        }

        if rc == -1 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                errno if is_again(errno) => return, // pipe full → pending
                errno => {
                    tickle_abort!("tickle: self-pipe write failed, errno={:?}", errno);
                }
            }
        }

        tickle_abort!("tickle: short self-pipe write: {rc}");
    }
}

#[allow(dead_code)]
pub(crate) fn on_tickled() {
    let mut buf = [0u8; 128];

    loop {
        let rc = unsafe { read(CTX.read_fd, buf.as_mut_ptr().cast::<c_void>(), buf.len()) };

        if rc > 0 {
            continue;
        }

        if rc == 0 {
            tickle_abort!("tickle: self-pipe read EOF");
        }

        let errno = std::io::Error::last_os_error().raw_os_error();

        match errno {
            Some(libc::EINTR) => continue,
            errno if is_again(errno) => break,
            errno => {
                tickle_abort!("tickle: unexpected error in on_tickled(), errno={errno:?}");
            }
        }
    }
}
