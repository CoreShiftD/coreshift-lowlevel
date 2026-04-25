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

## 2. Items exposed primarily due to visibility constraints
- **`FdSlot`**: Used as a return type in `DrainState`.
- **`ExecContext`**, **`ExecArgv`**: Used as public fields in `SpawnOptions`.

## 3. Proposal for v0.2.0 (Breaking Changes)

### `io::drain::FdSlot`
- **Current**: `pub struct FdSlot { pub token: Option<Token>, pub fd: Fd }`
- **Issue**: Leaks internal resource tracking structure.
- **Proposal**: Make `pub(crate)`. Change `DrainState` methods (`read_fd`, `write_stdin`) to return `Result<Option<Token>, SysError>`. The `Fd` is already managed by `DrainState` or dropped when the slot is taken.

### `sys::ExecContext` and `sys::ExecArgv`
- **Current**: `pub`
- **Issue**: Exposes implementation details of how arguments are stored (using `CString`).
- **Proposal**: Make `pub(crate)`. Hide `SpawnOptions::ctx` or make it a private field.

### `SpawnOptions` fields
- **Current**: All fields are `pub`.
- **Issue**: Prevents adding new options without a breaking change.
- **Proposal**: Make all fields private. Provide accessors if needed, but encourage use of `SpawnOptionsBuilder`.

### `Fd::read` and `Fd::write`
- **Current**: Public methods taking raw pointers.
- **Issue**: Encourages unsafe usage at the OS boundary.
- **Proposal**: Provide slice-based safe alternatives (e.g., `read(&mut [u8])`). Consider making the raw pointer versions `unsafe` or `pub(crate)`.
