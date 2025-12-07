#[cfg(feature = "eventfd")]
pub(crate) mod eventfd;
#[cfg(feature = "eventfd")]
pub(crate) use eventfd::{on_tickled, tickle};
