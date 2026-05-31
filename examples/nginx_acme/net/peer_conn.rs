// Copyright (c) F5, Inc.
//
// This source code is licensed under the Apache License, Version 2.0 license found in the
// LICENSE file in the root directory of this source tree.

use core::ffi::CStr;
use core::pin::Pin;
use core::ptr::{self, NonNull};
use core::task::{self, Poll};
use core::{future, mem};
use std::io;

use nginx_sys::{
    ngx_addr_t, ngx_connection_t, ngx_destroy_pool, ngx_event_connect_peer, ngx_event_get_peer,
    ngx_int_t, ngx_log_t, ngx_msec_t, ngx_peer_connection_t, ngx_pool_t, ngx_str_t,
};
use ngx::allocator::{AllocError, Box};
use ngx::core::Status;

use super::super::util;
use super::connection::{Connection, ConnectionLogError};

const ACME_DEFAULT_READ_TIMEOUT: ngx_msec_t = 60000;

/// Async wrapper over an [ngx_peer_connection_t].
pub struct PeerConnection {
    pub pool: util::OwnedPool,
    pub pc: ngx_peer_connection_t,
    pub rev: Option<task::Waker>,
    pub wev: Option<task::Waker>,
}

impl hyper::rt::Read for PeerConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let Some(c) = self.connection_mut() else {
            return Poll::Ready(Err(io::ErrorKind::InvalidInput.into()));
        };

        if c.read().timedout() != 0 {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }

        let n = c.recv(unsafe { buf.as_mut() });

        if n == nginx_sys::NGX_ERROR as isize {
            return Poll::Ready(Err(io::Error::last_os_error()));
        }

        let rev = c.read();

        if Status(unsafe { nginx_sys::ngx_handle_read_event(rev, 0) }) != Status::NGX_OK {
            return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
        }

        if rev.active() != 0 {
            unsafe { nginx_sys::ngx_add_timer(rev, ACME_DEFAULT_READ_TIMEOUT) };
        } else if rev.timer_set() != 0 {
            unsafe { nginx_sys::ngx_del_timer(rev) };
        }

        if n == nginx_sys::NGX_AGAIN as isize {
            self.rev = Some(cx.waker().clone());
            return Poll::Pending;
        }

        if n > 0 {
            unsafe { buf.advance(n as _) };
        }

        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for PeerConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let Some(c) = self.connection_mut() else {
            return Poll::Ready(Err(io::ErrorKind::InvalidInput.into()));
        };

        let n = c.send(buf);

        if n == nginx_sys::NGX_AGAIN as ngx_int_t {
            self.wev = Some(cx.waker().clone());
            Poll::Pending
        } else if n > 0 {
            Poll::Ready(Ok(n as usize))
        } else {
            Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()))
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.poll_shutdown(cx)
    }
}

impl PeerConnection {
    pub fn new(log: NonNull<ngx_log_t>) -> Result<Self, io::Error> {
        let mut pool =
            util::OwnedPool::with_default_size(log).map_err(|_| io::ErrorKind::OutOfMemory)?;

        // We need a copy of the log object to avoid modifying log.connection on a cycle log.
        let new_log = {
            let mut new_log = unsafe { log.read() };
            new_log.action = ptr::null_mut();
            new_log.data = ptr::null_mut(); // no final address
            new_log.handler = Some(Self::log_handler);
            ngx::allocator::allocate(new_log, &*pool).map_err(|_| io::ErrorKind::OutOfMemory)?
        };

        (*pool).as_mut().log = new_log.as_ptr();

        let mut this = Self {
            pool,
            pc: unsafe { mem::zeroed() },
            rev: None,
            wev: None,
        };

        let pc = &mut this.pc;
        pc.get = Some(ngx_event_get_peer);
        pc.log = new_log.as_ptr();
        pc.set_log_error(ConnectionLogError::Info as _);

        Ok(this)
    }

    pub async fn connect(mut self: Pin<&mut Self>, addr: &ngx_addr_t) -> Result<(), io::Error> {
        // copy sockaddr to the memory of the current connection
        let addr = copy_sockaddr(&self.pool, addr).map_err(|_| io::ErrorKind::OutOfMemory)?;
        let name =
            Box::try_new_in(addr.name, &*self.pool).map_err(|_| io::ErrorKind::OutOfMemory)?;
        self.pc.name = Box::leak(name);
        self.pc.sockaddr = addr.sockaddr;
        self.pc.socklen = addr.socklen;

        future::poll_fn(|cx| self.as_mut().poll_connect(cx)).await
    }

    fn connect_peer(&mut self) -> Status {
        let rc = Status(unsafe { ngx_event_connect_peer(&mut self.pc) });

        if rc == Status::NGX_ERROR || rc == Status::NGX_BUSY || rc == Status::NGX_DECLINED {
            return rc;
        }

        let c = unsafe { &mut *self.pc.connection };
        c.data = ptr::from_mut(self).cast();

        if c.pool.is_null() {
            c.pool = ptr::from_mut(self.pool.as_mut());
        }

        unsafe {
            (*c.log).connection = c.number;
            (*c.read).handler = Some(ngx_peer_conn_read_handler);
            (*c.write).handler = Some(ngx_peer_conn_write_handler);
        }

        rc
    }

    pub fn poll_connect(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        if let Some(c) = self.connection_mut() {
            let rv = if c.read().timedout() != 0 || c.write().timedout() != 0 {
                c.close();
                Err(io::ErrorKind::TimedOut.into())
            } else if let Err(err) = c.test_connect() {
                Err(io::Error::from_raw_os_error(err))
            } else {
                c.read().handler = Some(ngx_peer_conn_read_handler);
                c.write().handler = Some(ngx_peer_conn_write_handler);
                Ok(())
            };

            self.unset_log_action();
            return Poll::Ready(rv);
        }

        self.set_log_action(c"connecting");

        match self.connect_peer() {
            Status::NGX_OK => {
                let c = self.connection_mut().unwrap();
                debug!(c.log, "connected");
                self.unset_log_action();
                Poll::Ready(Ok(()))
            }
            Status::NGX_AGAIN => {
                let c = self.connection_mut().unwrap();
                debug!(c.log, "connect returned NGX_AGAIN");

                c.read().handler = Some(ngx_peer_conn_read_handler);
                c.write().handler = Some(ngx_peer_conn_write_handler);

                unsafe { nginx_sys::ngx_add_timer(c.read(), ACME_DEFAULT_READ_TIMEOUT) };

                self.rev = Some(cx.waker().clone());
                self.wev = Some(cx.waker().clone());

                Poll::Pending
            }

            x => {
                debug!(self.pc.log, "connect returned {x:?}");
                Poll::Ready(Err(io::ErrorKind::ConnectionRefused.into()))
            }
        }
    }

    pub fn poll_shutdown(
        mut self: Pin<&mut Self>,
        _cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.set_log_action(c"closing connection");

        let Some(c) = self.connection_mut() else {
            return Poll::Ready(Ok(()));
        };

        let pool = c.pool;
        c.close();
        self.pc.connection = ptr::null_mut();

        if !ptr::eq::<ngx_pool_t>(self.pool.as_ref(), pool) {
            unsafe { ngx_destroy_pool(pool) };
        }

        Poll::Ready(Ok(()))
    }

    pub fn connection_mut(&mut self) -> Option<&mut Connection> {
        if self.pc.connection.is_null() {
            None
        } else {
            Some(unsafe { Connection::from_ptr_mut(self.pc.connection) })
        }
    }

    fn close(&mut self) {
        let Some(c) = self.connection_mut() else {
            return;
        };

        let pool = c.pool;
        c.close();
        self.pc.connection = ptr::null_mut();

        if !ptr::eq::<ngx_pool_t>(self.pool.as_ref(), pool) {
            unsafe { ngx_destroy_pool(pool) };
        }
    }

    fn set_log_action(self: &Pin<&mut Self>, action: &'static CStr) {
        if let Some(log) = unsafe { self.pc.log.as_mut() } {
            log.data = ptr::from_ref(&self.pc).cast_mut().cast();
            log.action = action.as_ptr().cast_mut().cast();
        }
    }

    fn unset_log_action(&self) {
        if let Some(log) = unsafe { self.pc.log.as_mut() } {
            log.action = ptr::null_mut();
        }
    }

    unsafe extern "C" fn log_handler(
        log: *mut ngx_log_t,
        mut buf: *mut u8,
        mut len: usize,
    ) -> *mut u8 {
        unsafe {
            // SAFETY: log is never empty when calling log->handler
            let log = &mut *log;
            // SAFETY: log is an unique object owned by self, and log.data is either NULL or
            // initialized with a stable pointer to self.pc.
            let Some(pc) = log.data.cast::<ngx_peer_connection_t>().as_ref() else {
                return buf;
            };

            if !log.action.is_null() {
                let p = nginx_sys::ngx_snprintf(buf, len, c" while %s".as_ptr(), log.action);
                len -= p.offset_from(buf) as usize;
                buf = p;
            }

            if !pc.name.is_null() {
                let p = nginx_sys::ngx_snprintf(buf, len, c", server: %V".as_ptr(), pc.name);
                len -= p.offset_from(buf) as usize;
                buf = p;
            }

            if pc.socklen != 0 {
                let p = nginx_sys::ngx_snprintf(buf, len, c", addr: ".as_ptr());
                len -= p.offset_from(buf) as usize;

                let n = nginx_sys::ngx_sock_ntop(pc.sockaddr, pc.socklen, p, len, 1);
                buf = p.byte_add(n);
            }

            buf
        }
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe extern "C" fn ngx_peer_conn_read_handler(ev: *mut nginx_sys::ngx_event_t) {
    unsafe {
        let c: *mut ngx_connection_t = (*ev).data.cast();
        let this: *mut PeerConnection = (*c).data.cast();

        if let Some(waker) = (*this).rev.take() {
            waker.wake();
        }
    }
}

unsafe extern "C" fn ngx_peer_conn_write_handler(ev: *mut nginx_sys::ngx_event_t) {
    unsafe {
        let c: *mut ngx_connection_t = (*ev).data.cast();
        let this: *mut PeerConnection = (*c).data.cast();

        if let Some(waker) = (*this).wev.take() {
            waker.wake();

        // Handle write events posted from the ngx_event_openssl code.
        } else if Status(nginx_sys::ngx_handle_write_event(ev, 0)) != Status::NGX_OK {
            warn!((&*c), "acme: ngx_handle_write_event() failed");
        }
    }
}

fn copy_sockaddr(pool: &ngx::core::Pool, addr: &ngx_addr_t) -> Result<ngx_addr_t, AllocError> {
    let sockaddr = pool.alloc(addr.socklen as usize) as *mut nginx_sys::sockaddr;
    if sockaddr.is_null() {
        Err(AllocError)?;
    }

    unsafe {
        addr.sockaddr
            .cast::<u8>()
            .copy_to_nonoverlapping(sockaddr.cast(), addr.socklen as usize)
    };

    let name =
        unsafe { ngx_str_t::from_bytes(pool.as_ptr(), addr.name.as_bytes()) }.ok_or(AllocError)?;

    Ok(ngx_addr_t {
        sockaddr,
        socklen: addr.socklen,
        name,
    })
}
