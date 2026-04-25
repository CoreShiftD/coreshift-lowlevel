# CoreShift Low-Level Substrate

A stable, policy-neutral substrate for interacting with Linux system primitives.

`coreshift-lowlevel` provides foundational building blocks for building system policy engines, diagnostics tools, and process managers on Linux and Android. It focuses on safe resource ownership, non-blocking I/O multiplexing, and robust process lifecycle management.

## Features

### Asynchronous Reactor
A lightweight `epoll`-based reactor optimized for edge-triggered (`EPOLLET`) monitoring. It provides a simple token-based API for multiplexing I/O events with minimal overhead.

### File Descriptor Ownership
The `Fd` type provides atomic resource management. Descriptors are move-only and automatically closed when dropped, preventing leaks and double-close vulnerabilities.

### Process Management
Robust primitives for spawning processes with Redirection, resource constraints, and reliable cleanup. It supports both `posix_spawn` and `fork`/`exec` backends with automatic platform-specific selection (e.g., API-level detection on Android).

### Inotify Helpers
Type-safe interaction with the Linux `inotify` subsystem. Supports draining packed event streams safely and handles unaligned kernel structures.

### System Probes
Safe wrappers for `procfs` metadata, including process status, command lines, and system clock information.

## Intended Use

This crate is designed to be the "trusted OS boundary" for higher-level applications. It is strictly **policy-neutral**, providing the mechanisms (how to spawn, how to watch) while leaving the policy (what to spawn, when to watch) to the consumer.

It is particularly well-suited for:
- Android system daemons
- Lightweight process supervisors
- Performance monitoring tools

## Stability

The API is stable and intended as a foundation for production-grade software. While in public preview, additive improvements are preferred over breaking changes.

## License

Mozilla Public License, v. 2.0.
