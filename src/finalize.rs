use std::mem;

use nginx_sys::{
    ngx_delete_posted_event, ngx_event_t, ngx_http_finalize_request, ngx_http_request_t, ngx_int_t,
    ngx_post_event, ngx_posted_events,
};
use ngx::core::{Pool, Status};
use ngx::http::Request;
use ngx::log::ngx_cycle_log;

pub struct Finalization {
    event: ngx_event_t,
    request: *mut ngx_http_request_t,
    rc: ngx_int_t,
}

impl Drop for Finalization {
    fn drop(&mut self) {
        if self.event.posted() != 0 {
            unsafe { ngx_delete_posted_event(&mut self.event) };
        }
    }
}

/// Enqueue request finalization
///
/// This helper makes it easier to finalize properly from a task.
/// Calling [`nginx_sys::ngx_http_finalize_request`] directly in a task that is stored in the
/// request context would trigger immediate cleanup and abort it via the context's [`Drop`].
///
/// If you call finalize_request and **don't .await afterwards**, the task will run to completion
/// first.
///
/// # Thread safety
///
/// Must be called from the nginx main thread (i.e. from a task spawned via [`crate::spawn()`]).
pub fn finalize_request(request: &mut Request, rc: Status) {
    let request: *mut ngx_http_request_t = request.into();
    unsafe {
        let pool = Pool::from_ngx_pool((*request).pool);
        let mut event: ngx_event_t = mem::zeroed();
        event.handler = Some(fini);
        event.log = ngx_cycle_log().as_ptr();

        let rc = rc.0;
        let fin = pool.allocate(Finalization { event, request, rc });
        (*fin).event.data = fin.cast();

        ngx_post_event(&raw mut (*fin).event, &raw mut ngx_posted_events);
    }
}

extern "C" fn fini(ev: *mut ngx_event_t) {
    unsafe {
        let fin: *mut Finalization = (*ev).data.cast();
        ngx_http_finalize_request((*fin).request, (*fin).rc);
    }
}
