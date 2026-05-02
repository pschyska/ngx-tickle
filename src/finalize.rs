use std::mem;

use nginx_sys::{
    ngx_delete_posted_event, ngx_event_t, ngx_http_finalize_request, ngx_http_request_t,
    ngx_http_run_posted_requests, ngx_int_t, ngx_post_event, ngx_posted_events,
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

/// Enqueue request finalization for the next event-loop iteration.
///
/// Calling [`nginx_sys::ngx_http_finalize_request`] directly from a task whose storage is tied to
/// the request (e.g. a task anchored in the request pool via [`crate::RequestSpawn::spawn()`])
/// would trigger immediate request cleanup, tearing the task down while it is still running.
/// This helper posts an nginx event that runs the finalize on the next iteration, after the current
/// task has returned.
///
/// If you call `finalize_request` and **don't `.await` afterwards**, the task runs to completion
/// first; the request is finalized on the next event-loop tick.
///
/// # Thread safety
///
/// Must be called from the nginx main thread (i.e. from a task spawned via [`crate::spawn()`] or
/// [`crate::RequestSpawn::spawn()`]).
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

        let req = (*fin).request;
        let rc = (*fin).rc;

        ngx_http_finalize_request(req, rc);

        // Drain anything finalize posted (e.g. ngx_http_terminate_handler, which
        // destroys the pool). nginx's own event handlers do this on their way out;
        // our `fini` is a peer they don't know about, so we do it ourselves.
        ngx_http_run_posted_requests((*req).connection);
    }
}
