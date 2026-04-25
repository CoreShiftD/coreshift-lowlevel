# Public API Audit Checklist

This checklist is used to ensure the public API remains high-quality, documented, and safe.

## visibility
- [ ] No accidental `pub` items in internal modules.
- [ ] `pub(crate)` used where appropriate for internal sharing.
- [ ] Re-exports in `lib.rs` are intentional and minimal.

## documentation
- [ ] Every public module has a `//!` docstring.
- [ ] Every public struct/enum has a `///` docstring.
- [ ] Every public function/method has a `///` docstring with:
    - [ ] Brief summary.
    - [ ] `# Errors` section if it returns `Result`.
    - [ ] `# Examples` where helpful.
- [ ] `README.md` examples are up-to-date and compile.

## safety & correctness
- [ ] `unsafe` blocks are documented with `// SAFETY:` comments.
- [ ] Raw pointers are never exposed directly in high-level APIs.
- [ ] Error types are descriptive and implement `std::error::Error`.

## validation
- [ ] `cargo doc --no-deps` completes without warnings.
- [ ] `cargo test` passes.
- [ ] `cargo clippy` passes.
- [ ] `cargo public-api` (if available) shows no unexpected changes.

---

# v0.1.0-preview.1 API Surface Audit (for v0.2.0)

Date: 2026-04-25

## 1. Intentional Public Items
- `Reactor`, `Fd`, `Token`, `SysError`
- `DrainState`
- `SpawnOptions`, `SpawnOptionsBuilder`, `ProcessGroup`, `CancelPolicy`, `SpawnBackend`
- `InotifyDecoder`, `InotifyEvent`

## 2. Resolved in v0.2.0-preview.1

### `io::drain::FdSlot`
- **Status**: Resolved in `v0.2.0-preview.1`
- **Change**: `FdSlot` is now `pub(crate)`, and `DrainState` no longer returns slot ownership to public callers.

### `sys::ExecContext` and `sys::ExecArgv`
- **Status**: Resolved in `v0.2.0-preview.1`
- **Change**: Both types are now crate-private construction details behind the spawn builder path.

### `SpawnOptions` fields
- **Status**: Resolved in `v0.2.0-preview.1`
- **Change**: All fields are now private. `SpawnOptions::builder(...)` is the supported construction path.

### `Fd` raw-pointer read/write
- **Status**: Resolved in `v0.2.0-preview.1`
- **Change**: Public callers now use `Fd::read_slice` and `Fd::write_slice`; raw-pointer methods are no longer public API.

## 3. Remaining Open Issues

### `close_range_fast` Android syscall number
- **Status**: Open
- **Issue**: Android `close_range` still uses a hardcoded syscall number rather than a clearly documented per-arch constant path.

### Linux/Android module portability boundaries
- **Status**: Open
- **Issue**: `reactor` and `inotify` are target-specific APIs but the crate does not enforce those boundaries as clearly as it should at the module surface.

### Process-global shutdown helper semantics
- **Status**: Open
- **Issue**: `install_shutdown_flag` is deliberately process-global and improved, but it still deserves careful consumption and stronger long-term integration coverage.
