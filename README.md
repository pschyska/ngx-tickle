[![crates.io](https://img.shields.io/crates/v/ngx-tickle)](https://crates.io/crates/ngx-tickle)
[![docs.rs](https://img.shields.io/docsrs/ngx-tickle/0.1.0)](https://docs.rs/ngx-tickle/0.1.0/ngx_tickle/)

# ngx-tickle

Thread-safe async executor and event-loop wakeup for nginx modules

## When do I need this?
[`ngx`] provides its own [`ngx::async_::spawn()`] for running async tasks on nginx's
event loop. This works well for purely single-threaded futures — but as soon as a
secondary thread is involved, things get tricky:

- **Wakeups from other threads** can cause futures to be polled outside the main thread,
  violating nginx's single-threaded model. This leads to segfaults and memory corruption.
- **No event-loop wakeup mechanism** means that if nginx is blocked in `epoll_wait` when
  a background task completes, the result won't be delivered until an unrelated I/O event
  happens. This causes requests to hang unpredictably.

Secondary threads appear whenever you integrate another runtime — whether through
[`async_compat`] or a dedicated “sidecar”-runtime like [`tokio`].

ngx-tickle provides a drop-in replacement [`spawn()`] that handles all of this
correctly: It allows wakeups from any thread, but ensures futures always run on the main
thread, to maintain the single-threaded requirement of nginx core and will wake up —
“tickle” — the event loop, if needed, to ensure prompt processing.

## How it works

**Executor**: [`spawn()`] creates async tasks using the same [`async_task`] primitives
as [`ngx`]. The difference is in scheduling: when a wakeup arrives on a secondary
thread, the runnable is placed on a thread-safe queue instead of being executed
immediately. A native nginx event is scheduled; its handler is guaranteed to run on the
main thread and drains the queue. An event-loop notification (“tickle“) ensures nginx
picks it up promptly.

**Notify**: A lightweight wakeup mechanism — `eventfd` on Linux, a self-pipe elsewhere —
registered as a read event on the nginx event loop. When a runnable is queued from
another thread, writing to this fd causes `epoll_wait`/`kqueue` to return, and nginx
will process our event immediately.

**Fairness**: The queue is drained in bounded batches (configurable via
[`set_max_runnables_per_wakeup()`]), ensuring nginx's own I/O events are not starved.

**Key invariant**: Futures are *always* polled on the nginx main thread. [`spawn()`]
does not require `Send`, so they can freely hold references to nginx structures like
[`ngx::http::Request`].

[`ngx`]: https://docs.rs/ngx/latest/ngx/
[`ngx::async_::spawn()`]: https://docs.rs/ngx/latest/ngx/async_/fn.spawn.html
[`async_compat`]: https://docs.rs/async-compat/latest/async_compat/
[`tokio`]: https://docs.rs/tokio/latest/tokio/
[`spawn()`]: https://docs.rs/ngx-tickle/0.1.0/ngx_tickle/fn.spawn.html
[`async_task`]: https://docs.rs/async-task/latest/async_task/
[`set_max_runnables_per_wakeup()`]: https://docs.rs/ngx-tickle/0.1.0/ngx_tickle/fn.set_max_runnables_per_wakeup.html
[`ngx::http::Request`]: https://docs.rs/ngx/latest/ngx/http/struct.Request.html
