[![crates.io](https://img.shields.io/crates/v/ngx-tickle)](https://crates.io/crates/ngx-tickle)
[![docs.rs](https://img.shields.io/docsrs/ngx-tickle/{ version })](https://docs.rs/ngx-tickle/{ version }/ngx_tickle/)

# ngx-tickle

Thread-safe async executor and event-loop wakeup for nginx modules

## Synopsis

```rust
use async_compat::CompatExt;
use ngx_tickle::prelude::*;

async fn async_handler(request: &mut ngx::http::Request) \{
    let response = reqwest::get("http://example.com").compat().await.unwrap();
    request.add_header_out("x-example-ran", &format!("\{}", response.status()));
    finalize_request(request, ngx::http::HTTPStatus::NO_CONTENT.into());
}

ngx::http_request_handler!(access_phase_handler, |request: &mut ngx::http::Request| \{
    if request.spawn(async_handler).is_err() \{
        return ngx::core::Status::NGX_ERROR;
    }

    ngx::core::Status::NGX_AGAIN
});
```

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

## Performance

Per-task overhead is small enough to disappear in nginx's own request pipeline.
In a preliminary benchmarking series on a 5850U running a single nginx worker, driven
by `wrk2 -t4 -c100 -d30s -R15000 --latency http://127.0.0.1…` on the same machine:

- ~30µs per request-bound task (spawn + first poll + finalize_request round-trip)
- ~10µs per nested subtask round-trip
- reported p50 request latency is ~770µs whether the handler is a bare `return 204;` in
  nginx.conf or a spawned ngx-tickle task — the integration cost seems to be below
  `wrk2`'s run-to-run noise floor at the request level
- the executor itself only takes a small percentage of CPU time; adding further
  infrastructure (`async-compat`) or `spawn_blocking` callers (`tokio::fs`) predictably
  increases overhead

## For ngx-rust users

`ngx::async_::spawn` calls translate one-for-one to [`spawn()`]. The synopsis above
also shows `request.spawn`, the new request-bound entry point.

## License

ngx-tickle is distributed under the terms of the [MIT license](LICENSE-MIT), or the
[Apache License (Version 2.0)](LICENSE-APACHE), at your option.

## MSRV

1.85+ (edition 2024)

## Examples

- [`compat.rs`] — “I just want to use reqwest” via async-compat.
- [`sidecar.rs`] — off-thread work via a sidecar tokio runtime.
- [`yielding.rs`] — cooperative yielding / fairness demo.

[`ngx`]: https://docs.rs/ngx/latest/ngx/
[`ngx::async_::spawn()`]: https://docs.rs/ngx/latest/ngx/async_/fn.spawn.html
[`async_compat`]: https://docs.rs/async-compat/latest/async_compat/
[`tokio`]: https://docs.rs/tokio/latest/tokio/
[`spawn()`]: https://docs.rs/ngx-tickle/{ version }/ngx_tickle/fn.spawn.html
[`async_task`]: https://docs.rs/async-task/latest/async_task/
[`set_max_runnables_per_wakeup()`]: https://docs.rs/ngx-tickle/{ version }/ngx_tickle/fn.set_max_runnables_per_wakeup.html
[`ngx::http::Request`]: https://docs.rs/ngx/latest/ngx/http/struct.Request.html
[`compat.rs`]: https://github.com/pschyska/ngx-tickle/blob/main/examples/compat.rs
[`sidecar.rs`]: https://github.com/pschyska/ngx-tickle/blob/main/examples/sidecar.rs
[`yielding.rs`]: https://github.com/pschyska/ngx-tickle/blob/main/examples/yielding.rs
