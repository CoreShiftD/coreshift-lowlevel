// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! Asynchronous event reactor.
//!
//! This module provides a lightweight wrapper around Linux `epoll` for
//! multiplexing I/O events. It is optimized for edge-triggered monitoring.

use crate::spawn::{SysError, syscall_ret};
use std::io::Error as IoError;

#[inline(always)]
fn errno() -> i32 {
    IoError::last_os_error().raw_os_error().unwrap_or(0)
}

/// A safe wrapper for file descriptors ensuring they are closed when dropped.
///
/// `Fd` provides atomic resource management for raw descriptors. It implements
/// `Drop` to ensure the descriptor is closed, and it is move-only to prevent
/// accidental double-closes.
pub struct Fd(RawFd);

use std::os::unix::io::{AsRawFd, RawFd};

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Fd {
    /// Wrap a raw file descriptor.
    ///
    /// # Errors
    /// Returns a [`SysError`] if the descriptor is negative.
    #[inline(always)]
    pub fn new(fd: RawFd, op: &'static str) -> Result<Self, SysError> {
        if fd < 0 {
            Err(SysError::sys(errno(), op))
        } else {
            Ok(Self(fd))
        }
    }

    /// Access the underlying raw file descriptor.
    ///
    /// NOTE: This is an escape hatch for low-level interactions. Prefer using
    /// the safe methods on `Fd` or implementing `AsRawFd`.
    #[inline(always)]
    pub(crate) fn raw(&self) -> RawFd {
        self.0
    }

    /// Perform a `dup2` syscall.
    pub fn dup2(&self, target: RawFd) -> Result<(), SysError> {
        loop {
            let r = unsafe { libc::dup2(self.0, target) };
            if r < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                return syscall_ret(r, "dup2");
            }
            return Ok(());
        }
    }

    /// Set the `O_NONBLOCK` flag on the descriptor.
    pub fn set_nonblock(&self) -> Result<(), SysError> {
        let flags = unsafe { libc::fcntl(self.0, libc::F_GETFL) };
        syscall_ret(flags, "fcntl(F_GETFL)")?;
        let r = unsafe { libc::fcntl(self.0, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        syscall_ret(r, "fcntl(F_SETFL)")
    }

    /// Set the `FD_CLOEXEC` flag on the descriptor.
    pub fn set_cloexec(&self) -> Result<(), SysError> {
        let flags = unsafe { libc::fcntl(self.0, libc::F_GETFD) };
        syscall_ret(flags, "fcntl(F_GETFD)")?;
        let r = unsafe { libc::fcntl(self.0, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        syscall_ret(r, "fcntl(F_SETFD)")
    }

    /// Read bytes into a raw buffer.
    ///
    /// ### Advanced API
    /// This is a low-level wrapper around `libc::read`. The caller is
    /// responsible for ensuring `buf` points to a valid memory region of at
    /// least `count` bytes.
    ///
    /// Returns `Ok(None)` if the operation would block (`EAGAIN`).
    pub fn read(&self, buf: *mut u8, count: usize) -> Result<Option<usize>, SysError> {
        loop {
            let n = unsafe { libc::read(self.0, buf as *mut libc::c_void, count) };
            if n < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                if e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                    return Ok(None);
                }
                return Err(SysError::sys(e, "read"));
            }
            return Ok(Some(n as usize));
        }
    }

    /// Write bytes from a raw buffer.
    ///
    /// ### Advanced API
    /// This is a low-level wrapper around `libc::write`. The caller is
    /// responsible for ensuring `buf` points to a valid memory region of at
    /// least `count` bytes.
    ///
    /// Returns `Ok(None)` if the operation would block (`EAGAIN`).
    pub fn write(&self, buf: *const u8, count: usize) -> Result<Option<usize>, SysError> {
        loop {
            let n = unsafe { libc::write(self.0, buf as *const libc::c_void, count) };
            if n < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                if e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                    return Ok(None);
                }
                return Err(SysError::sys(e, "write"));
            }
            return Ok(Some(n as usize));
        }
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

/// An opaque token representing a registered file descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token(u64);

#[allow(dead_code)]
impl Token {
    #[inline(always)]
    pub(crate) fn new(val: u64) -> Self {
        Self(val)
    }

    #[inline(always)]
    pub(crate) fn val(&self) -> u64 {
        self.0
    }
}

/// A readiness event generated by the reactor.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// Token associated with the ready descriptor.
    pub token: Token,
    /// Descriptor is ready for reading.
    pub readable: bool,
    /// Descriptor is ready for writing.
    pub writable: bool,
    /// Indicates an error or hangup (EPOLLERR | EPOLLHUP).
    ///
    /// NOTE: For edge-triggered readiness, an error condition often means both
    /// readable and writable are set to ensure the handler drains the FD.
    pub error: bool,
}

/// A lightweight epoll reactor using edge-triggered monitoring (EPOLLET).
///
/// ### Edge-Triggered Contract
/// Because this reactor uses EPOLLET, all handlers MUST drain their respective
/// read or write sources until they receive an `EAGAIN` / `EWOULDBLOCK` error
/// (represented as `Ok(None)` in the `Fd` helpers).
///
/// Failure to drain a source will result in missing future readiness events
/// for that file descriptor until it is re-registered or another event occurs.
///
/// # Example
/// ```no_run
/// # use coreshift_lowlevel::reactor::{Reactor, Fd, Event};
/// # fn example(fd: Fd) -> Result<(), Box<dyn std::error::Error>> {
/// let mut reactor = Reactor::new()?;
/// let token = reactor.add(&fd, true, false)?;
///
/// let mut events = Vec::new();
/// loop {
///     reactor.wait(&mut events, 64, -1)?;
///     for ev in &events {
///         if ev.token == token {
///             // Drain fd...
///         }
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Reactor {
    epfd: RawFd,
    next_token: u64,
    events_buf: Vec<libc::epoll_event>,
    signalfd: Option<Fd>,
    /// Token for the signalfd (if initialized).
    pub sigchld_token: Option<Token>,
    /// Token for the inotify fd (if initialized).
    pub inotify_token: Option<Token>,
}

impl Reactor {
    /// Create a new epoll reactor.
    pub fn new() -> Result<Self, SysError> {
        let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        syscall_ret(epfd, "epoll_create1")?;
        Ok(Self {
            epfd,
            next_token: 1,
            events_buf: Vec::with_capacity(64),
            signalfd: None,
            sigchld_token: None,
            inotify_token: None,
        })
    }

    /// Initialize inotify and add it to the reactor.
    pub fn setup_inotify(&mut self) -> Result<Fd, SysError> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        syscall_ret(fd, "inotify_init1")?;

        let fd_obj = Fd::new(fd, "inotify")?;
        let token = self.add(&fd_obj, true, false)?;
        self.inotify_token = Some(token);

        Ok(fd_obj)
    }

    /// Initialize signalfd for SIGCHLD and add it to the reactor.
    pub fn setup_signalfd(&mut self) -> Result<(), SysError> {
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut mask) };
        unsafe { libc::sigaddset(&mut mask, libc::SIGCHLD) };

        // Block SIGCHLD so signalfd can intercept it
        let r = unsafe { libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) };
        syscall_ret(r, "sigprocmask")?;

        let sfd = unsafe { libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        syscall_ret(sfd, "signalfd")?;

        let fd = Fd::new(sfd, "signalfd")?;
        let token = self.add(&fd, true, false)?;

        self.signalfd = Some(fd);
        self.sigchld_token = Some(token);

        Ok(())
    }

    /// Drain the internal signalfd buffer.
    pub fn drain_signalfd(&self) {
        if let Some(fd) = &self.signalfd {
            let mut buf = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
            loop {
                match fd.read(buf.as_mut_ptr(), buf.len()) {
                    Ok(Some(n)) if n < buf.len() => break,
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }

    /// Register a file descriptor with the reactor.
    ///
    /// This assigns a new unique token for the descriptor and enables
    /// edge-triggered monitoring.
    #[inline(always)]
    pub fn add(&mut self, fd: &Fd, readable: bool, writable: bool) -> Result<Token, SysError> {
        let token = Token(self.next_token);
        self.next_token += 1;
        self.add_with_token(fd.raw(), token, readable, writable)?;
        Ok(token)
    }

    #[inline(always)]
    pub(crate) fn add_with_token(
        &mut self,
        raw_fd: RawFd,
        token: Token,
        readable: bool,
        writable: bool,
    ) -> Result<(), SysError> {
        let mut events = libc::EPOLLET as u32;
        if readable {
            events |= libc::EPOLLIN as u32;
        }
        if writable {
            events |= libc::EPOLLOUT as u32;
        }
        let mut ev = libc::epoll_event {
            events,
            u64: token.0,
        };
        let r = unsafe { libc::epoll_ctl(self.epfd, libc::EPOLL_CTL_ADD, raw_fd, &mut ev) };
        syscall_ret(r, "epoll_ctl_add")?;
        Ok(())
    }

    /// Remove a file descriptor from the reactor.
    #[inline(always)]
    pub fn del(&self, fd: &Fd) {
        self.del_raw(fd.raw());
    }

    /// Remove a raw descriptor from the reactor.
    ///
    /// NOTE: This is an escape hatch for low-level interactions. Prefer using
    /// [`del`](Self::del).
    #[inline(always)]
    pub(crate) fn del_raw(&self, raw: RawFd) {
        unsafe {
            let _ = libc::epoll_ctl(self.epfd, libc::EPOLL_CTL_DEL, raw, std::ptr::null_mut());
        }
    }

    /// Wait for events.
    ///
    /// This function blocks until at least one event is ready or the timeout
    /// expires. Ready events are appended to the `buffer`.
    ///
    /// Returns the number of events received.
    #[inline(always)]
    pub fn wait(
        &mut self,
        buffer: &mut Vec<Event>,
        max_events: usize,
        timeout: i32,
    ) -> Result<usize, SysError> {
        buffer.clear();

        if max_events == 0 {
            return Ok(0);
        }

        // Ensure buffer has enough capacity
        if buffer.capacity() < max_events {
            buffer.reserve(max_events.saturating_sub(buffer.len()));
        }

        if self.events_buf.capacity() < max_events {
            self.events_buf
                .reserve(max_events.saturating_sub(self.events_buf.len()));
        }

        let n = unsafe {
            libc::epoll_wait(
                self.epfd,
                self.events_buf.as_mut_ptr(),
                max_events as i32,
                timeout,
            )
        };

        if n > 0 {
            for i in 0..n as usize {
                let ev = unsafe { *self.events_buf.as_ptr().add(i) };
                let is_read = (ev.events & libc::EPOLLIN as u32) != 0;
                let is_write = (ev.events & libc::EPOLLOUT as u32) != 0;
                let is_err = (ev.events & (libc::EPOLLERR | libc::EPOLLHUP) as u32) != 0;

                buffer.push(Event {
                    token: Token(ev.u64),
                    readable: is_read || is_err,
                    writable: is_write || is_err,
                    error: is_err,
                });
            }
            return Ok(n as usize);
        }

        if n < 0 {
            let e = errno();
            if e == libc::EINTR {
                return Ok(0);
            }
            return Err(SysError::sys(e, "epoll_wait"));
        }
        Ok(0)
    }

    /// Return the raw epoll file descriptor.
    ///
    /// NOTE: This is an escape hatch for low-level interactions.
    #[allow(dead_code)]
    pub(crate) fn fd(&self) -> RawFd {
        self.epfd
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        if self.epfd >= 0 {
            unsafe {
                libc::close(self.epfd);
            }
        }
    }
}
