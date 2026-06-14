//! eventfd notify backend (Linux). The shared context + wiring live in [`super`]; this
//! module provides only the fd and the three syscalls that touch it.
#![allow(dead_code)] // unused when the self-pipe backend is selected

use libc::{O_CLOEXEC, O_NONBLOCK, eventfd, eventfd_read, eventfd_t, eventfd_write};
use ngx::log::ngx_cycle_log;
use ngx::ngx_log_debug;

use crate::tickle_abort;

pub(crate) const NAME: &str = "eventfd";

/// Create the eventfd. It is both read and write end, so both fds are the same.
pub(crate) fn open() -> (i32, i32) {
    let fd = unsafe { eventfd(0, O_NONBLOCK | O_CLOEXEC) };
    if fd == -1 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        tickle_abort!("tickle: eventfd failed, errno={errno:?}");
    }
    (fd, fd)
}

pub(crate) fn signal(fd: i32) {
    loop {
        let rc = unsafe { eventfd_write(fd, 1) };
        if rc == 0 {
            ngx_log_debug!(ngx_cycle_log().as_ptr(), "tickle: notified (eventfd)");
            return;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return, // eventfd full → wakeup already pending
            errno => tickle_abort!("tickle: eventfd_write failed, errno={errno:?}"),
        }
    }
}

pub(crate) fn drain(fd: i32) {
    let mut _val: eventfd_t = 0;
    loop {
        let rc = unsafe { eventfd_read(fd, &raw mut _val) };
        if rc == 0 {
            return; // eventfd_read reset the counter to 0 → drained
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return, // already drained
            errno => tickle_abort!("tickle: eventfd_read failed, errno={errno:?}"),
        }
    }
}
