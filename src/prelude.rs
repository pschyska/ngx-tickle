//! Convenience re-exports.
//!
//! ```ignore
//! use ngx_tickle::prelude::*;
//! ```
#[doc(no_inline)]
pub use crate::{RequestTask, Task, finalize_request, set_batch_size, spawn};

#[doc(no_inline)]
pub use crate::RequestSpawn as _;
