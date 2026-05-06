# Changelog

## [unreleased]

### Fixed
- recursive-poll self-deadlock when a future invokes a `Waker` for another task while
  holding a non-reentrant lock (e.g. `std::Mutex` inside hyper's connection pool, via
  `Pooled<T>::drop`). The scheduler now always queues; runnables are never polled
  inline as a side effect of another task's wake. Matches tokio's executor behavior.

## [0.2.1]

### Added
- `RequestSpawn` trait for request-bound async tasks ([#7](https://github.com/pschyska/ngx-tickle/pull/7))
- `prelude` module for convenience re-exports

### Fixed
- missing call to `ngx_http_run_posted_requests` in `finalize_request`,
  which could leave the request pool un-destroyed after some error paths

## [0.2.0]

### Added
- selfpipe notify module for non-Linux targets (can be forced with feature `selfpipe`)
- C wrapper to expand macros: `ngx_add_event()`
- docs and surrounding machinery, “docs” CI workflow
- ”compat” example

### Changed
- limit maximum number of runnables processed per wakeup, default: 8, configurable with
  `set_max_runnables_per_wakeup()`
- bump dependencies and devShell
- use `eventfd_{read,write}()` instead `libc::read()`

### Removed
- `vendored` feature; there is no need to proxy `ngx/vendored`

## [0.1.0]

### Added

- Initial public release of spawn(), Scheduler and eventfd notify implementation

[unreleased]: https://github.com/pschyska/ngx-tickle/compare/0.2.1...HEAD
[0.2.1]: https://github.com/pschyska/ngx-tickle/compare/0.2.0...0.2.1
[0.2.0]: https://github.com/pschyska/ngx-tickle/compare/0.1.0...0.2.0
[0.1.0]: https://github.com/pschyska/ngx-tickle/releases/tag/0.1.0
