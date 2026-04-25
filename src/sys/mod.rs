// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! Low-level execution context and signal helpers.
//!
//! `ExecContext` owns the exact C-compatible argv/env/cwd values that are later
//! passed into spawn backends. Validation happens here so higher layers cannot
//! silently drop malformed strings or rely on hidden fallbacks.
//!
//! Ownership and failure semantics:
//! - owned `CString` storage outlives the transient pointer arrays passed to
//!   `execve`-style backends
//! - validation failures are normal input errors and should be surfaced as
//!   spawn failures rather than repaired in place
//! - pointer helpers intentionally cap the pointer array size to keep stack
//!   usage bounded while preserving null termination

use crate::spawn::{SysError, syscall_ret};
use libc::sigset_t;
use std::os::unix::io::AsRawFd;

/// Probe whether a filesystem path is accessible and exists.
///
/// NOTE: This follows symbolic links. It uses `libc::access` with `F_OK`
/// so the check is a single syscall with no Rust allocator involvement.
/// Returns `true` if the path is accessible/visible, `false` on any error
/// (including `ENOENT`, `EACCES`, etc.).
///
/// This is the canonical low-level path-existence helper. Higher layers
/// must call this instead of `std::path::Path::exists()`.
pub fn path_exists(path: &str) -> bool {
    match std::ffi::CString::new(path) {
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::F_OK) == 0 },
        Err(_) => false,
    }
}

/// Probe whether a path exists without following symbolic links.
///
/// Returns `true` if the path exists (even as a dangling symlink).
pub fn path_lstat_exists(path: &str) -> bool {
    match std::ffi::CString::new(path) {
        Ok(c) => unsafe {
            let mut stat = std::mem::zeroed();
            libc::lstat(c.as_ptr(), &mut stat) == 0
        },
        Err(_) => false,
    }
}

/// Low-level procfs helpers may use `std::fs` because they operate as the
/// OS boundary layer where blocking I/O on pseudo-files is acceptable.
pub fn read_to_string(path: &str) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

/// Advise the kernel to begin reading file data into the page cache.
///
/// This is an advisory hint only. It can help warm likely-needed file ranges,
/// but the kernel may ignore the request, perform only part of it, or return
/// before the data is fully resident in memory.
///
/// The `offset` and `len` identify the byte range to prefetch for `fd`.
/// Success means the kernel accepted the request, not that subsequent reads are
/// guaranteed to be cache hits.
pub fn readahead(fd: impl AsRawFd, offset: u64, len: usize) -> Result<(), SysError> {
    readahead_raw(fd.as_raw_fd(), offset, len)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn readahead_raw(fd: libc::c_int, offset: u64, len: usize) -> Result<(), SysError> {
    if offset > i64::MAX as u64 {
        return Err(SysError::sys(libc::EINVAL, "readahead"));
    }

    let ret =
        unsafe { libc::syscall(readahead_syscall_number(), fd, offset as libc::off64_t, len) };

    if ret == -1 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        Err(SysError::sys(code, "readahead"))
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn readahead_raw(_fd: libc::c_int, _offset: u64, _len: usize) -> Result<(), SysError> {
    Err(SysError::sys(libc::ENOSYS, "readahead"))
}

#[cfg(all(target_os = "android", target_arch = "aarch64"))]
#[inline(always)]
const fn readahead_syscall_number() -> libc::c_long {
    213
}

#[cfg(any(
    target_os = "linux",
    all(target_os = "android", not(target_arch = "aarch64"))
))]
#[inline(always)]
const fn readahead_syscall_number() -> libc::c_long {
    libc::SYS_readahead
}

/// A snapshot of process status information from procfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStatus {
    /// Command name of the process.
    pub name: String,
    /// Real UID of the process.
    pub uid: u32,
}

/// Read process status from /proc/pid/status.
///
/// Returns `Err` with `ErrorKind::NotFound` if the process does not exist
/// (ENOENT). This is a normal condition for transient processes.
pub fn read_proc_status(pid: i32) -> Result<ProcStatus, std::io::Error> {
    let path = format!("/proc/{}/status", pid);
    parse_proc_status(&std::fs::read_to_string(path)?)
}

/// Read process command line from /proc/pid/cmdline.
///
/// Returns `Err` with `ErrorKind::NotFound` if the process does not exist
/// (ENOENT). This is a normal condition for transient processes.
pub fn read_proc_cmdline(pid: i32) -> Result<String, std::io::Error> {
    let path = format!("/proc/{}/cmdline", pid);
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .replace('\0', " "))
}

/// Parse the contents of a /proc/pid/status file.
pub fn parse_proc_status(content: &str) -> Result<ProcStatus, std::io::Error> {
    let mut name = None;
    let mut uid = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Name:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok());
        }

        if name.is_some() && uid.is_some() {
            break;
        }
    }

    match (name, uid) {
        (Some(name), Some(uid)) => Ok(ProcStatus { name, uid }),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "proc status missing Name or Uid",
        )),
    }
}

/// Utilities for process signal management.
pub struct SignalRuntime;

/// Return the system clock ticks per second.
#[inline(always)]
pub fn get_clk_tck() -> u64 {
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 }
}

impl SignalRuntime {
    /// Create an empty signal set.
    pub fn empty_set() -> sigset_t {
        let mut set: sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut set) };
        set
    }

    /// Create a signal set containing the specified signals.
    pub fn set_with(signals: &[i32]) -> sigset_t {
        let mut set: sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut set) };
        for &sig in signals {
            unsafe { libc::sigaddset(&mut set, sig) };
        }
        set
    }

    /// Unblock all signals for the current thread.
    pub fn unblock_all() -> Result<(), SysError> {
        let empty_mask = Self::empty_set();
        let r = unsafe { libc::sigprocmask(libc::SIG_SETMASK, &empty_mask, std::ptr::null_mut()) };
        syscall_ret(r, "sigprocmask")
    }

    /// Reset a signal to its default kernel handler.
    pub fn reset_default(sig: i32) {
        unsafe { libc::signal(sig, libc::SIG_DFL) };
    }
}
use libc::{c_char, pid_t};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::ptr;

/// Policy for handling process cancellation or timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CancelPolicy {
    /// Do nothing on cancellation; let the process run to completion.
    #[default]
    None,
    /// Send SIGTERM, then SIGKILL after a grace period.
    Graceful,
    /// Send SIGKILL immediately.
    Kill,
}

/// Process group and session configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessGroup {
    /// Join an existing process group leader.
    pub leader: Option<pid_t>,
    /// Create a new session (setsid).
    pub isolated: bool,
}

impl ProcessGroup {
    /// Create a new process group configuration.
    pub fn new(leader: Option<pid_t>, isolated: bool) -> Self {
        Self { leader, isolated }
    }
}

/// Owned argument vector storage.
#[derive(Clone)]
pub enum ExecArgv {
    /// Dynamically allocated C-compatible strings.
    Dynamic(Vec<CString>),
}

/// Validated execution context for process spawning.
#[derive(Clone)]
pub struct ExecContext {
    /// The argument vector.
    pub argv: ExecArgv,
    /// Optional environment variables.
    pub envp: Option<Vec<CString>>,
    /// Optional working directory.
    pub cwd: Option<CString>,
}

impl ExecContext {
    /// Build a validated execution context for process spawn.
    ///
    /// Rejections are explicit:
    /// - empty argv is invalid
    /// - interior NUL bytes in argv/env/cwd are invalid
    pub fn new(
        argv: Vec<String>,
        env: Option<Vec<String>>,
        cwd: Option<String>,
    ) -> Result<Self, crate::spawn::SysError> {
        if argv.is_empty() {
            return Err(crate::spawn::SysError::sys(libc::EINVAL, "exec argv empty"));
        }

        let c_argv: Vec<CString> = argv
            .into_iter()
            .map(|s| {
                CString::new(s).map_err(|_| {
                    crate::spawn::SysError::sys(libc::EINVAL, "exec argv contains nul")
                })
            })
            .collect::<Result<_, _>>()?;

        let c_envp = match env {
            Some(vars) => Some(
                vars.into_iter()
                    .map(|s| {
                        CString::new(s).map_err(|_| {
                            crate::spawn::SysError::sys(libc::EINVAL, "exec env contains nul")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };

        let c_cwd =
            match cwd {
                Some(c) => Some(CString::new(c).map_err(|_| {
                    crate::spawn::SysError::sys(libc::EINVAL, "exec cwd contains nul")
                })?),
                None => None,
            };

        Ok(Self {
            argv: ExecArgv::Dynamic(c_argv),
            envp: c_envp,
            cwd: c_cwd,
        })
    }

    /// Return a vector of pointers to the argument strings.
    ///
    /// ### Advanced API
    /// This returns raw pointers to the underlying `CString` storage. The
    /// pointers are only valid as long as the `ExecContext` is not dropped or
    /// modified.
    pub(crate) fn get_argv_ptrs(&self) -> Vec<*mut c_char> {
        let mut ptrs = Vec::new();
        match &self.argv {
            ExecArgv::Dynamic(v) => {
                for s in v {
                    ptrs.push(s.as_ptr() as *mut c_char);
                }
            }
        }
        ptrs.push(ptr::null_mut());
        ptrs
    }

    /// Return a vector of pointers to the environment strings.
    ///
    /// ### Advanced API
    /// This returns raw pointers to the underlying `CString` storage. The
    /// pointers are only valid as long as the `ExecContext` is not dropped or
    /// modified.
    pub(crate) fn get_envp_ptrs(&self) -> Option<Vec<*mut c_char>> {
        self.envp.as_ref().map(|envp| {
            let mut ptrs = Vec::new();
            for s in envp {
                ptrs.push(s.as_ptr() as *mut c_char);
            }
            ptrs.push(ptr::null_mut());
            ptrs
        })
    }
}
