//! self-pipe notify backend (portable fallback, e.g. macOS where `eventfd` is absent).
//! The shared context + wiring live in [`super`]; this module provides only the fds and
//! the syscalls that touch them.
#![allow(dead_code)] // unused when the eventfd backend is selected

use std::ffi::c_void;

use libc::{
    F_GETFD, F_GETFL, F_SETFD, F_SETFL, FD_CLOEXEC, O_NONBLOCK, c_int, fcntl, pipe, read, write,
};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::tickle_abort;

pub(crate) const NAME: &str = "self-pipe";

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

fn is_again(errno: Option<i32>) -> bool {
    matches!(errno, Some(e) if e == libc::EAGAIN || e == libc::EWOULDBLOCK)
}

/// Create the self-pipe: `(read_fd, write_fd)`, both non-blocking + close-on-exec.
pub(crate) fn open() -> (i32, i32) {
    let mut fds = [-1 as c_int; 2];
    let rc = unsafe { pipe(fds.as_mut_ptr()) };
    if rc == -1 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        tickle_abort!("tickle: pipe failed, errno={errno:?}");
    }
    set_nonblocking(fds[0]);
    set_nonblocking(fds[1]);
    set_cloexec(fds[0]);
    set_cloexec(fds[1]);
    (fds[0], fds[1]) // (read_fd, write_fd)
}

pub(crate) fn signal(fd: i32) {
    let byte: u8 = 1;
    let ptr = &byte as *const u8 as *const c_void;
    loop {
        let rc = unsafe { write(fd, ptr, 1) };
        if rc == 1 {
            ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (self-pipe)");
            return;
        }
        if rc == -1 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                errno if is_again(errno) => return, // pipe full → wakeup already pending
                errno => tickle_abort!("tickle: self-pipe write failed, errno={errno:?}"),
            }
        }
        tickle_abort!("tickle: short self-pipe write: {rc}");
    }
}

pub(crate) fn drain(fd: i32) {
    let mut buf = [0u8; 128];
    loop {
        let rc = unsafe { read(fd, buf.as_mut_ptr().cast::<c_void>(), buf.len()) };
        if rc > 0 {
            continue;
        }
        if rc == 0 {
            tickle_abort!("tickle: self-pipe read EOF");
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            errno if is_again(errno) => break,
            errno => tickle_abort!("tickle: unexpected error draining self-pipe, errno={errno:?}"),
        }
    }
}
