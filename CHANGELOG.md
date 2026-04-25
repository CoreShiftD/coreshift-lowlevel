# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3-preview.2]

### Fixed
- Hardened `sys::install_shutdown_flag` installation order and rollback semantics.
- Documented shutdown handler failure behavior and process-global semantics.

## [0.2.0-preview.1]

### Changed
- Replaced public raw-pointer `Fd` read/write API with safe slice-based `read_slice`/`write_slice`.
- Made `SpawnOptions` fields private; builder is now the construction path.
- Hid `ExecContext` and `ExecArgv` from public API.
- Hid `FdSlot` from public API.
- Simplified `DrainState` public completion API.

### Added
- `Fd::read_slice`
- `Fd::write_slice`

## [0.1.3-preview.1]

### Added
- Added `sys::install_shutdown_flag`
- Added lightweight shutdown signal support for SIGINT/SIGTERM

## [0.1.2-preview.1]

### Added
- Added `sys::path_uid` helper for cheap ownership checks.
- Added `sys::proc_uid` helper for cheap ownership checks.

## [0.1.1-preview.1]

### Added
- Added readahead helper.

## [0.1.0-preview] - 2026-04-25

### Added
- Initial public preview release.
- **Reactor**: Lightweight `epoll` wrapper with edge-triggered support.
- **Fd Ownership**: Move-only `Fd` type with automatic resource cleanup.
- **Inotify**: Type-safe inotify event decoding and watch management.
- **Spawn**: Robust process spawning with `posix_spawn` and `fork`/`exec` backends.
- **Procfs**: Helpers for reading process status and command lines.
- **I/O Drain**: Efficient non-blocking stdout/stderr capture and stdin writing.
