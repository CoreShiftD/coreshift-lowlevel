# Stability Policy

This document outlines the stability guarantees and versioning policy for `coreshift-lowlevel`.

## Semantic Versioning

`coreshift-lowlevel` follows [Semantic Versioning 2.0.0](https://semver.org/).

### Pre-1.0 (0.x.y)
- The crate is currently in **Public Preview**.
- Public APIs may change between minor versions (e.g., 0.1.0 to 0.2.0).
- Patch versions (0.x.y to 0.x.z) are reserved for bug fixes and additive, non-breaking changes.

### Post-1.0
- Once 1.0.0 is released, we guarantee backwards compatibility.
- Breaking changes will require a major version bump.
- Deprecated APIs will be maintained until the next major version.

## Minimum Supported Rust Version (MSRV)

- The current MSRV is **Rust 1.85.0** (due to use of the 2024 edition).
- We aim to maintain a stable MSRV. Changes to MSRV are considered minor breaking changes and will be communicated in the changelog.

## Platform Support

### Linux
- Full support for modern Linux kernels (5.x+).
- Supports both `glibc` and `musl`.

### Android
- Primary support for Android API level 32 and above.
- Compatibility fallbacks (e.g., `fork`/`exec` instead of `posix_spawn`) are implemented for older API levels.

## Breaking Changes

Breaking changes are never taken lightly. During the 0.x phase, we will provide clear migration paths in the `CHANGELOG.md` for any breaking changes.
