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
