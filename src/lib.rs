#![warn(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]
#![doc = include_str!("../README.md")]
//!
//! # Usage
//!
//! ## Sidecar runtime
//!
//! Run I/O or CPU-bound work on a separate runtime like [`tokio`], then process results back on
//! the main thread.
//!
//! ### Advantages
//!
//! - more control
//! - allows running compute-intensive tasks off-main-thread, concurrently with nginx
//!
//! ### Disadvantages
//!
//! - more complexity
//! - futures can't be `!Send`; you have to copy required nginx-owned data in
//!
//! See the [`sidecar` example].
//!
//! ## async-compat
//!
//! [`async_compat`] transparently provides [`tokio`] and [`futures`] contexts, letting you use
//! crates that expect either of these directly.
//! Note that it provides these *runtimes*, without using its *executors*. All futures are polled
//! on the ngx-tickle executor, on the nginx main thread.
//!
//! ### Advantages
//!
//! - easier to use
//!
//! ### Disadvantages
//!
//! - compute-heavy tasks block nginx and can starve its I/O
//!
//! There are situations where both approaches could be combined, like spawning
//! [`async_compat::Compat`]-wrapped tasks from phase handlers to provide runtime compatibility,
//! and spawning compute-intensive subtasks or background tasks in a [`tokio`] runtime to not block
//! nginx.
//!
//! Both the simple [`async_compat`] and the "combined" approach are demonstrated in the
//! [`compat` example].
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
//! [`async_compat::Compat`]: https://docs.rs/async-compat/latest/async_compat/struct.Compat.html
//! [`compat` example]: https://github.com/pschyska/ngx-tickle/blob/main/examples/compat.rs
//! [`yielding` example]: https://github.com/pschyska/ngx-tickle/blob/main/examples/yielding.rs

mod finalize;
pub use finalize::finalize_request;
mod spawn;
pub use spawn::{Task, set_max_runnables_per_wakeup, spawn};
mod notify;
