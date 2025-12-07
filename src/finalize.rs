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

/// Finalize Request by posting a ngx_event_t. This allows async tasks to finish normally.
/// SAFETY:
/// - Must only be run in the event loop thread — i.e. in a nginx handler or from an async task
///   spawned by this crate.
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
