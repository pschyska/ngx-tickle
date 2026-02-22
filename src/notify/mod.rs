#[cfg(ngx_os = "linux")]
pub(crate) mod eventfd;
#[cfg(all(not(feature = "selfpipe"), ngx_os = "linux"))]
pub(crate) use eventfd::{on_tickled, tickle};

pub(crate) mod selfpipe;
#[cfg(any(feature = "selfpipe", not(ngx_os = "linux")))]
pub(crate) use selfpipe::{on_tickled, tickle};

unsafe extern "C" {
    fn ngx_tickle_add_read_event(ev: *mut nginx_sys::ngx_event_t) -> nginx_sys::ngx_int_t;
}
