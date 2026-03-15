# Changelog

## [Unreleased]

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

[unreleased]: https://github.com/olivierlacan/keep-a-changelog/compare/0.1.0...HEAD
[0.1.0]: https://github.com/pschyska/ngx-tickle/releases/tag/0.1.0
