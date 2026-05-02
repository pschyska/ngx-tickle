#![warn(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]
#![doc = include_str!("../README.md")]
//!
//! # Spawning tasks
//!
//! Most users will want `use ngx_tickle::prelude::*;` to bring the common items into
//! scope.
//!
//! Two complementary APIs:
//!
//! - [`crate::RequestSpawn::spawn()`] — `request.spawn(handler)` for *request-bound* tasks. The
//!   handler may borrow `&mut Request` directly, no unsafe `'static` lift required. The task is
//!   anchored in the request pool and cancelled at request teardown. This is the recommended entry
//!   point for in-request async work, including from sync `http_request_handler!` bodies.
//!   See the [trait docs](crate::RequestSpawn) for usage patterns and examples.
//!
//! - [`crate::spawn()`] — top-level spawn. Returns a [`crate::Task<T>`] handle. The future must be
//!   `'static`. Use this for background work that outlives the originating request
//!   (e.g. caches set up from `init_process`), or for futures that don't need to
//!   borrow request data.
//!
//! # Tokio integration
//!
//! Many useful async crates ([`reqwest`], [`hyper`], …) expect a [`tokio`] runtime to
//! be available. ngx-tickle's executor is not tokio, so you have to bring tokio context
//! in. There are two common approaches:
//!
//! ## async-compat
//!
//! [`async_compat`] transparently provides [`tokio`] and [`futures`] contexts, letting
//! you use crates that expect either directly. Note that it provides these *runtimes*,
//! without using their *executors*. All futures are still polled on the ngx-tickle
//! executor, on the nginx main thread.
//!
//! ### Advantages
//!
//! - easier to use; just wrap your future via `Compat::new(fut)` or `fut.compat()`
//!   (both from [`async_compat`]).
//!
//! ### Disadvantages
//!
//! - compute-heavy tasks block nginx and can starve its I/O.
//!
//! ## Sidecar runtime
//!
//! Run I/O or CPU-bound work on a separate [`tokio`] runtime, then process results back
//! on the main thread.
//!
//! ### Advantages
//!
//! - more control.
//! - allows running compute-intensive tasks off-main-thread, concurrently with nginx.
//!
//! ### Disadvantages
//!
//! - more complexity.
//! - sidecar futures can't be `!Send` and can't reference nginx-owned data; you have to
//!   copy data in and copy results out.
//!
//! Both approaches (and a combination of the two) are demonstrated in the [`compat`
//! example]. The sidecar pattern alone is shown in the [`sidecar` example].
//!
//! If unsure, start with [`async_compat`].
//!
//! # Reentrancy
//!
//! If a task is woken while running ([`async_task::ScheduleInfo::woken_while_running`]),
//! usually because it yielded itself to the scheduler, it is forced through the queue.
//! This is to prevent unbounded stack growth.
//!
//! See the [`yielding` example](https://github.com/pschyska/ngx-tickle/blob/main/examples/yielding.rs)
//!
//! # Feature flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `selfpipe` | no | Force the self-pipe notify mechanism (instead of `eventfd` on Linux) |
//!
//! [`sidecar` example]: https://github.com/pschyska/ngx-tickle/blob/main/examples/sidecar.rs
//! [`futures`]: https://docs.rs/futures/latest/futures/
//! [`reqwest`]: https://docs.rs/reqwest/latest/reqwest/
//! [`hyper`]: https://docs.rs/hyper/latest/hyper/
//! [`compat` example]: https://github.com/pschyska/ngx-tickle/blob/main/examples/compat.rs
//! [`yielding` example]: https://github.com/pschyska/ngx-tickle/blob/main/examples/yielding.rs

mod finalize;
pub use finalize::finalize_request;
mod notify;
mod spawn;
pub use spawn::{RequestSpawn, RequestTask, Task, set_max_runnables_per_wakeup, spawn};
pub mod prelude;
