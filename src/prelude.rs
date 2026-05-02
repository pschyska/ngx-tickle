//! Convenience re-exports.
//!
//! ```ignore
//! use ngx_tickle::prelude::*;
//! ```
#[doc(no_inline)]
pub use crate::{RequestTask, Task, finalize_request, set_max_runnables_per_wakeup, spawn};

#[doc(no_inline)]
pub use crate::RequestSpawn as _;
