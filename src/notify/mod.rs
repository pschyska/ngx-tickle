#[cfg(all(not(feature = "selfpipe"), ngx_os = "linux"))]
pub(crate) mod eventfd;
#[cfg(all(not(feature = "selfpipe"), ngx_os = "linux"))]
pub(crate) use eventfd::{on_tickled, tickle};
#[cfg(any(feature = "selfpipe", not(ngx_os = "linux")))]
pub(crate) mod selfpipe;
#[cfg(any(feature = "selfpipe", not(ngx_os = "linux")))]
pub(crate) use selfpipe::{on_tickled, tickle};
