// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! Process spawning and lifecycle management.
//!
//! This module provides high-level primitives for spawning processes with
//! resource constraints, I/O redirection, and automatic cleanup. It supports
//! both `posix_spawn` and `fork`/`exec` backends, with automatic selection
//! based on platform capabilities.

use std::fmt;
use std::mem::MaybeUninit;
use std::os::unix::io::RawFd;

use crate::reactor::Fd;
use crate::sys::{CancelPolicy, ExecContext, ProcessGroup, SignalRuntime};
use libc::{
    O_CLOEXEC, O_NONBLOCK, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG, c_char, pid_t, pipe2,
    waitpid,
};

unsafe extern "C" {
    pub(crate) static mut environ: *mut *mut libc::c_char;
}

#[cfg(target_os = "android")]
unsafe extern "C" {
    pub(crate) fn __system_property_get(
        name: *const libc::c_char,
        value: *mut libc::c_char,
    ) -> libc::c_int;
}

pub(crate) const POSIX_SPAWN_SETPGROUP: i32 = 2;
pub(crate) const POSIX_SPAWN_SETSIGDEF: i32 = 4;
pub(crate) const POSIX_SPAWN_SETSIGMASK: i32 = 8;

unsafe extern "C" {
    pub(crate) fn posix_spawn(
        pid: *mut libc::pid_t,
        path: *const libc::c_char,
        file_actions: *const libc::posix_spawn_file_actions_t,
        attrp: *const libc::posix_spawnattr_t,
        argv: *const *mut libc::c_char,
        envp: *const *mut libc::c_char,
    ) -> libc::c_int;

    pub(crate) fn posix_spawn_file_actions_addclose(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;

    pub(crate) fn posix_spawn_file_actions_adddup2(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
        newfd: libc::c_int,
    ) -> libc::c_int;

    pub(crate) fn posix_spawn_file_actions_destroy(
        file_actions: *mut libc::posix_spawn_file_actions_t,
    ) -> libc::c_int;

    pub(crate) fn posix_spawn_file_actions_init(
        file_actions: *mut libc::posix_spawn_file_actions_t,
    ) -> libc::c_int;

    pub(crate) fn posix_spawnattr_destroy(attr: *mut libc::posix_spawnattr_t) -> libc::c_int;

    pub(crate) fn posix_spawnattr_init(attr: *mut libc::posix_spawnattr_t) -> libc::c_int;

    pub(crate) fn posix_spawnattr_setflags(
        attr: *mut libc::posix_spawnattr_t,
        flags: libc::c_short,
    ) -> libc::c_int;

    pub(crate) fn posix_spawnattr_setpgroup(
        attr: *mut libc::posix_spawnattr_t,
        pgroup: libc::pid_t,
    ) -> libc::c_int;

    pub(crate) fn posix_spawnattr_setsigdefault(
        attr: *mut libc::posix_spawnattr_t,
        sigdefault: *const libc::sigset_t,
    ) -> libc::c_int;

    pub(crate) fn posix_spawnattr_setsigmask(
        attr: *mut libc::posix_spawnattr_t,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;
}

use serde::{Deserialize, Serialize};

/// Error type for low-level system operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SysError {
    /// A syscall failed with the specified error code.
    Syscall {
        /// The raw OS error code.
        code: i32,
        /// The name of the failed operation.
        op: String,
    },
}

impl SysError {
    /// Construct a new syscall error.
    pub fn sys(code: i32, op: &str) -> Self {
        SysError::Syscall {
            code,
            op: op.to_string(),
        }
    }
    /// Return the raw OS error code if applicable.
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            SysError::Syscall { code, .. } => Some(*code),
        }
    }
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syscall { code, op } => write!(f, "{} failed (code={})", op, code),
        }
    }
}

impl std::error::Error for SysError {}

#[inline(always)]
pub(crate) fn syscall_ret(ret: i32, op: &'static str) -> Result<(), SysError> {
    if ret == -1 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        Err(SysError::sys(code, op))
    } else {
        Ok(())
    }
}

#[inline(always)]
pub(crate) fn posix_ret(ret: i32, op: &'static str) -> Result<(), SysError> {
    if ret != 0 {
        Err(SysError::sys(ret, op))
    } else {
        Ok(())
    }
}

#[inline(always)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Creates a pipe with O_CLOEXEC | O_NONBLOCK flags.
/// Invariants: FDs returned are strictly non-negative and will close automatically on drop.
#[inline(always)]
fn make_pipe() -> Result<(Fd, Fd), SysError> {
    let mut fds = [0; 2];
    let r = unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC | O_NONBLOCK) };
    syscall_ret(r, "pipe2")?;
    Ok((Fd::new(fds[0], "pipe2")?, Fd::new(fds[1], "pipe2")?))
}

struct Pipes {
    stdin_r: Option<Fd>,
    stdin_w: Option<Fd>,
    stdout_r: Option<Fd>,
    stdout_w: Option<Fd>,
    stderr_r: Option<Fd>,
    stderr_w: Option<Fd>,
}

impl Pipes {
    fn new(in_buf: Option<&[u8]>, out: bool, err: bool) -> Result<Self, SysError> {
        let (stdin_r, stdin_w) = if in_buf.is_some() {
            let (r, w) = make_pipe()?;
            (Some(r), Some(w))
        } else {
            (None, None)
        };

        let (stdout_r, stdout_w) = if out {
            let (r, w) = make_pipe()?;
            (Some(r), Some(w))
        } else {
            (None, None)
        };

        let (stderr_r, stderr_w) = if err {
            let (r, w) = make_pipe()?;
            (Some(r), Some(w))
        } else {
            (None, None)
        };

        Ok(Self {
            stdin_r,
            stdin_w,
            stdout_r,
            stdout_w,
            stderr_r,
            stderr_w,
        })
    }

    #[inline(always)]
    fn close_all(&mut self) {
        self.stdin_r.take();
        self.stdin_w.take();
        self.stdout_r.take();
        self.stdout_w.take();
        self.stderr_r.take();
        self.stderr_w.take();
    }
}

/// Close FDs quickly.
/// Invariant: "FDs >= 3 are always closed in child except those specified to keep".
unsafe fn close_range_fast(keep_fd: Option<RawFd>) {
    #[cfg(target_os = "android")]
    {
        // try SYS_close_range (available on 5.9+)
        if let Some(fd) = keep_fd {
            let r1 = unsafe { libc::syscall(436, 3, (fd - 1).max(2) as libc::c_uint, 0) };
            let r2 = unsafe { libc::syscall(436, (fd + 1) as libc::c_uint, !0u32, 0) };
            if r1 == 0 && r2 == 0 {
                return;
            }
        } else {
            if unsafe { libc::syscall(436, 3, !0u32, 0) } == 0 {
                return;
            }
        }
    }
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    {
        if let Some(fd) = keep_fd {
            let r1 = unsafe {
                libc::syscall(libc::SYS_close_range, 3, (fd - 1).max(2) as libc::c_uint, 0)
            };
            let r2 =
                unsafe { libc::syscall(libc::SYS_close_range, (fd + 1) as libc::c_uint, !0u32, 0) };
            if r1 == 0 && r2 == 0 {
                return;
            }
        } else {
            if unsafe { libc::syscall(libc::SYS_close_range, 3, !0u32, 0) } == 0 {
                return;
            }
        }
    }

    let skip_fd = keep_fd.unwrap_or(-1);
    let dir_fd = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if dir_fd >= 0 {
        let dir = unsafe { libc::fdopendir(dir_fd) };
        if !dir.is_null() {
            loop {
                let entry = unsafe { libc::readdir(dir) };
                if entry.is_null() {
                    break;
                }
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
                if let Ok(s) = name.to_str()
                    && let Ok(fd) = s.parse::<i32>()
                    && fd > 2
                    && fd != skip_fd
                    && fd != dir_fd
                    && fd >= 0
                {
                    unsafe {
                        libc::close(fd);
                    }
                }
            }
            unsafe {
                libc::closedir(dir);
            }
        } else {
            unsafe {
                libc::close(dir_fd);
            }
        }
    }
}

/// Represents the termination status of a process.
#[derive(Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// Process exited normally with the specified code.
    Exited(i32),
    /// Process was terminated by a signal.
    Signaled(i32),
}

/// Advisory selection of the process spawning backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnBackend {
    /// Let the system choose the best backend (preferred).
    Auto,
    /// Force the use of `posix_spawn`.
    PosixSpawn,
    /// Force the use of `fork`/`exec`.
    Fork,
}

#[inline(always)]
fn decode_status(status: i32) -> ExitStatus {
    if WIFEXITED(status) {
        ExitStatus::Exited(WEXITSTATUS(status))
    } else if WIFSIGNALED(status) {
        ExitStatus::Signaled(WTERMSIG(status))
    } else {
        ExitStatus::Exited(-1)
    }
}

/// A handle to a spawned process.
pub struct Process {
    pid: pid_t,
}

impl Process {
    /// Create a handle for an existing PID.
    pub fn new(pid: pid_t) -> Self {
        Self { pid }
    }

    /// Return the process ID.
    pub fn pid(&self) -> pid_t {
        self.pid
    }

    /// Perform a non-blocking wait for process termination.
    pub fn wait_step(&self) -> Result<Option<ExitStatus>, SysError> {
        loop {
            let mut status = 0;
            let r = unsafe { waitpid(self.pid, &mut status, libc::WNOHANG) };
            if r == 0 {
                return Ok(None);
            }
            if r < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                return Err(SysError::sys(e, "waitpid_step"));
            }
            return Ok(Some(decode_status(status)));
        }
    }

    /// Block until the process terminates.
    pub fn wait_blocking(&self) -> Result<ExitStatus, SysError> {
        loop {
            let mut status = 0;
            let r = unsafe { waitpid(self.pid, &mut status, 0) };
            if r < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                return Err(SysError::sys(e, "waitpid_blocking"));
            }
            return Ok(decode_status(status));
        }
    }

    /// Send a signal to the process.
    pub fn kill(&self, sig: i32) -> Result<(), SysError> {
        let r = unsafe { libc::kill(self.pid, sig) };
        if r < 0 {
            let e = errno();
            if e == libc::ESRCH {
                return Ok(());
            }
            syscall_ret(-1, "kill")?;
        }
        Ok(())
    }

    /// Send a signal to the process group.
    pub fn kill_pgroup(&self, sig: i32) -> Result<(), SysError> {
        let r = unsafe { libc::kill(-self.pid, sig) };
        if r < 0 {
            let e = errno();
            if e == libc::ESRCH {
                return Ok(());
            }
            syscall_ret(-1, "kill_pgroup")?;
        }
        Ok(())
    }
}

/// Configuration options for spawning a new process.
#[derive(Clone)]
pub struct SpawnOptions {
    /// Execution context (argv, env, cwd).
    pub ctx: ExecContext,
    /// Optional buffer to write to the child's stdin.
    pub stdin: Option<Box<[u8]>>,
    /// Capture the child's stdout.
    pub capture_stdout: bool,
    /// Capture the child's stderr.
    pub capture_stderr: bool,
    /// Wait for the process to terminate.
    pub wait: bool,
    /// Process group and isolation settings.
    pub pgroup: ProcessGroup,
    /// Maximum number of bytes to capture from output streams.
    pub max_output: usize,
    /// Optional execution timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// Grace period in milliseconds before `SIGKILL` after `SIGTERM`.
    pub kill_grace_ms: u32,
    /// Policy for handling process cancellation/timeout.
    pub cancel: CancelPolicy,
    /// Advisory selection of the spawning backend.
    pub backend: SpawnBackend,
    /// Optional closure called on each stdout chunk; return `true` to exit early.
    pub early_exit: Option<fn(&[u8]) -> bool>,
}

/// The result of a process execution.
#[derive(Debug)]
pub struct Output {
    /// The PID of the finished process.
    pub pid: pid_t,
    /// Final exit status (None if `wait=false`).
    pub status: Option<ExitStatus>,
    /// Captured stdout buffer.
    pub stdout: Vec<u8>,
    /// Captured stderr buffer.
    pub stderr: Vec<u8>,
    /// Whether the process timed out.
    pub timed_out: bool,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
enum Backend {
    PosixSpawn,
    Fork,
}

fn select_backend() -> Backend {
    static BACKEND: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();
    *BACKEND.get_or_init(|| {
        #[cfg(target_os = "android")]
        {
            let mut value = [0u8; 92]; // PROP_VALUE_MAX = 92
            let name = b"ro.build.version.sdk\0";
            let len = unsafe {
                __system_property_get(
                    name.as_ptr() as *const libc::c_char,
                    value.as_mut_ptr() as *mut libc::c_char,
                )
            };
            if len > 0 {
                let s = std::str::from_utf8(&value[..len as usize]).unwrap_or("");
                if let Ok(api) = s.parse::<u32>() {
                    if api < 32 {
                        return Backend::Fork;
                    }
                }
            }
        }
        Backend::PosixSpawn
    })
}

#[inline(always)]
fn force_fork(opts: &SpawnOptions) -> bool {
    opts.pgroup.isolated || opts.ctx.cwd.is_some()
}

fn resolve_backend(opts: &SpawnOptions) -> Backend {
    if force_fork(opts) {
        return Backend::Fork;
    }

    match opts.backend {
        SpawnBackend::Auto => select_backend(),
        SpawnBackend::PosixSpawn => Backend::PosixSpawn,
        SpawnBackend::Fork => Backend::Fork,
    }
}

use crate::io::DrainState;

/// Specialized drain state for process spawning.
pub type SpawnDrain = DrainState<fn(&[u8]) -> bool>;

/// A process that is currently running and being monitored.
pub struct RunningProcess {
    /// Handle to the process.
    pub process: Process,
    /// I/O management state.
    pub drain: SpawnDrain,
}

use crate::reactor::Reactor;

/// Start spawning a process and return a monitor handle.
///
/// This initializes the pipes and starts the process, but does not block.
/// The caller is responsible for adding the resulting pipe descriptors to a
/// reactor and draining them.
pub fn spawn_start(job_id: u64, opts: SpawnOptions) -> Result<RunningProcess, SysError> {
    if !opts.wait && (opts.stdin.is_some() || opts.capture_stdout || opts.capture_stderr) {
        return Err(SysError::sys(
            libc::EINVAL,
            "background I/O capture not supported (wait must be true)",
        ));
    }

    let backend = resolve_backend(&opts);

    let (pid, drain) = match backend {
        Backend::PosixSpawn => spawn_posix_internal(job_id, opts)?,
        Backend::Fork => spawn_fork_internal(job_id, opts)?,
    };

    Ok(RunningProcess {
        process: Process::new(pid),
        drain,
    })
}

/// Spawn a process and block until completion or timeout.
///
/// This is the primary high-level interface for process execution. It handles
/// the full lifecycle, including I/O multiplexing and signal management.
pub fn spawn(opts: SpawnOptions) -> Result<Output, SysError> {
    let wait = opts.wait;
    let timeout_ms = opts.timeout_ms;
    let kill_grace_ms = opts.kill_grace_ms;
    let cancel = opts.cancel;
    let pgroup = opts.pgroup;

    let mut reactor = Reactor::new()?;
    let running = spawn_start(0, opts)?; // ID=0 is arbitrary for synchronous unmanaged spawn

    let pid = running.process.pid();
    let mut drain = running.drain;

    // To prevent FD leak and adhere to ownership, we shouldn't use `mem::forget`.
    if let Some(mut slot) = drain.stdin_slot.take() {
        slot.token = Some(reactor.add(&slot.fd, false, true)?);
        drain.stdin_slot = Some(slot);
    }
    if let Some(mut slot) = drain.stdout_slot.take() {
        slot.token = Some(reactor.add(&slot.fd, true, false)?);
        drain.stdout_slot = Some(slot);
    }
    if let Some(mut slot) = drain.stderr_slot.take() {
        slot.token = Some(reactor.add(&slot.fd, true, false)?);
        drain.stderr_slot = Some(slot);
    }

    if !wait {
        let (stdout, stderr) = drain.into_parts();
        return Ok(Output {
            pid,
            status: None,
            stdout,
            stderr,
            timed_out: false,
        });
    }

    wait_loop(
        pid,
        drain,
        reactor,
        timeout_ms,
        kill_grace_ms,
        cancel,
        pgroup,
    )
}

fn spawn_posix_internal(job_id: u64, opts: SpawnOptions) -> Result<(pid_t, SpawnDrain), SysError> {
    let mut pipes = Pipes::new(
        opts.stdin.as_deref(),
        opts.capture_stdout,
        opts.capture_stderr,
    )?;

    let exe_ptr = match &opts.ctx.argv {
        crate::sys::ExecArgv::Dynamic(v) => v[0].as_ptr(),
    };

    let argv = opts.ctx.get_argv_ptrs();
    let envp = opts.ctx.get_envp_ptrs();

    let actions = MaybeUninit::zeroed();
    let mut actions = unsafe { actions.assume_init() };
    if let Err(e) = posix_ret(
        unsafe { posix_spawn_file_actions_init(&mut actions) },
        "file_actions_init",
    ) {
        pipes.close_all();
        return Err(e);
    }

    struct Actions(*mut libc::posix_spawn_file_actions_t);
    impl Drop for Actions {
        fn drop(&mut self) {
            unsafe {
                posix_spawn_file_actions_destroy(self.0);
            }
        }
    }
    let _guard = Actions(&mut actions);

    if let (Some(r), Some(w)) = (&pipes.stdin_r, &pipes.stdin_w) {
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_adddup2(&mut actions, r.raw(), 0) },
            "dup2 stdin",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, r.raw()) },
            "close stdin pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, w.raw()) },
            "close stdin write pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
    }

    if let (Some(r), Some(w)) = (&pipes.stdout_r, &pipes.stdout_w) {
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_adddup2(&mut actions, w.raw(), 1) },
            "dup2 stdout",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, w.raw()) },
            "close stdout pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, r.raw()) },
            "close stdout read pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
    }

    if let (Some(r), Some(w)) = (&pipes.stderr_r, &pipes.stderr_w) {
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_adddup2(&mut actions, w.raw(), 2) },
            "dup2 stderr",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, w.raw()) },
            "close stderr pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
        if let Err(e) = posix_ret(
            unsafe { posix_spawn_file_actions_addclose(&mut actions, r.raw()) },
            "close stderr read pipe",
        ) {
            pipes.close_all();
            return Err(e);
        }
    }

    // Explicitly tracked FDs that are already handled above.
    let mut handled_fds = arrayvec::ArrayVec::<i32, 8>::new();
    if let Some(fd) = &pipes.stdin_r {
        let _ = handled_fds.try_push(fd.raw());
    }
    if let Some(fd) = &pipes.stdin_w {
        let _ = handled_fds.try_push(fd.raw());
    }
    if let Some(fd) = &pipes.stdout_r {
        let _ = handled_fds.try_push(fd.raw());
    }
    if let Some(fd) = &pipes.stdout_w {
        let _ = handled_fds.try_push(fd.raw());
    }
    if let Some(fd) = &pipes.stderr_r {
        let _ = handled_fds.try_push(fd.raw());
    }
    if let Some(fd) = &pipes.stderr_w {
        let _ = handled_fds.try_push(fd.raw());
    }

    // Prevent FD leaks in posix_spawn by strictly closing open descriptors
    // instead of blindly closing all possible FDs.
    let dir_fd = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if dir_fd >= 0 {
        let dir = unsafe { libc::fdopendir(dir_fd) };
        if !dir.is_null() {
            loop {
                let entry = unsafe { libc::readdir(dir) };
                if entry.is_null() {
                    break;
                }
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
                if let Ok(s) = name.to_str()
                    && let Ok(fd) = s.parse::<i32>()
                    && fd > 2
                    && fd != dir_fd
                    && !handled_fds.contains(&fd)
                {
                    // Note: actions run in the child process, so we close the fd there
                    unsafe {
                        posix_spawn_file_actions_addclose(&mut actions, fd);
                    }
                }
            }
            unsafe {
                libc::closedir(dir);
            }
        } else {
            unsafe {
                libc::close(dir_fd);
            }
        }
    }

    let attr = MaybeUninit::zeroed();
    let mut attr = unsafe { attr.assume_init() };
    if let Err(e) = posix_ret(unsafe { posix_spawnattr_init(&mut attr) }, "attr_init") {
        pipes.close_all();
        return Err(e);
    }

    struct Attr(*mut libc::posix_spawnattr_t);
    impl Drop for Attr {
        fn drop(&mut self) {
            unsafe {
                posix_spawnattr_destroy(self.0);
            }
        }
    }
    let _attr = Attr(&mut attr);

    let mut flags = 0;

    if let Some(pg) = opts.pgroup.leader {
        flags |= POSIX_SPAWN_SETPGROUP;
        if let Err(e) = posix_ret(
            unsafe { posix_spawnattr_setpgroup(&mut attr, pg) },
            "setpgroup",
        ) {
            pipes.close_all();
            return Err(e);
        }
    }

    flags |= POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF;

    if let Err(e) = posix_ret(
        unsafe { posix_spawnattr_setflags(&mut attr, flags as _) },
        "setflags",
    ) {
        pipes.close_all();
        return Err(e);
    }

    let empty_mask = SignalRuntime::empty_set();
    let def = SignalRuntime::set_with(&[libc::SIGPIPE]);

    if let Err(e) = posix_ret(
        unsafe { posix_spawnattr_setsigmask(&mut attr, &empty_mask) },
        "setsigmask",
    ) {
        pipes.close_all();
        return Err(e);
    }
    if let Err(e) = posix_ret(
        unsafe { posix_spawnattr_setsigdefault(&mut attr, &def) },
        "setsigdefault",
    ) {
        pipes.close_all();
        return Err(e);
    }

    let mut pid: pid_t = 0;

    let envp_ptr = envp.as_ref().map_or_else(
        || unsafe { environ as *const *mut c_char },
        |e: &arrayvec::ArrayVec<*mut c_char, 64>| e.as_ptr(),
    );

    if let Err(e) = posix_ret(
        unsafe { posix_spawn(&mut pid, exe_ptr, &actions, &attr, argv.as_ptr(), envp_ptr) },
        "posix_spawn",
    ) {
        pipes.close_all();
        return Err(e);
    }

    drop(pipes.stdin_r.take());
    drop(pipes.stdout_w.take());
    drop(pipes.stderr_w.take());

    let drain = crate::io::DrainState::new(
        job_id,
        pipes.stdin_w.take().filter(|_| opts.stdin.is_some()),
        opts.stdin,
        pipes.stdout_r.take(),
        pipes.stderr_r.take(),
        opts.max_output,
        opts.early_exit,
    )?;

    Ok((pid, drain))
}

fn spawn_fork_internal(job_id: u64, opts: SpawnOptions) -> Result<(pid_t, SpawnDrain), SysError> {
    let mut pipes = Pipes::new(
        opts.stdin.as_deref(),
        opts.capture_stdout,
        opts.capture_stderr,
    )?;

    let exe_ptr = match &opts.ctx.argv {
        crate::sys::ExecArgv::Dynamic(v) => v[0].as_ptr(),
    };

    let argv = opts.ctx.get_argv_ptrs();
    let envp = opts.ctx.get_envp_ptrs();
    let cwd_cstr = &opts.ctx.cwd;

    let pid = unsafe { libc::fork() };

    if pid < 0 {
        pipes.close_all();
        syscall_ret(-1, "fork")?;
    }

    if pid == 0 {
        // Child

        // dup stdin
        if let (Some(r), Some(_)) = (&pipes.stdin_r, &pipes.stdin_w)
            && r.raw() != 0
        {
            // SAFETY: r.raw() is a valid fd. Target 0 is valid.
            unsafe {
                libc::dup2(r.raw(), 0);
            }
        }

        // dup stdout
        if let (Some(_), Some(w)) = (&pipes.stdout_r, &pipes.stdout_w)
            && w.raw() != 1
        {
            // SAFETY: w.raw() is a valid fd. Target 1 is valid.
            unsafe {
                libc::dup2(w.raw(), 1);
            }
        }

        // dup stderr
        if let (Some(_), Some(w)) = (&pipes.stderr_r, &pipes.stderr_w)
            && w.raw() != 2
        {
            // SAFETY: w.raw() is a valid fd. Target 2 is valid.
            unsafe {
                libc::dup2(w.raw(), 2);
            }
        }

        // SAFETY: Closes all unused file descriptors.
        unsafe {
            close_range_fast(None);
        }

        // setsid
        if opts.pgroup.isolated {
            // SAFETY: safe to call setsid in child.
            unsafe {
                libc::setsid();
            }
        }

        // chdir
        if let Some(cwd) = cwd_cstr {
            // SAFETY: cwd is a valid null-terminated CString.
            unsafe {
                if libc::chdir(cwd.as_ptr()) != 0 {
                    libc::_exit(127);
                }
            }
        }

        // setpgid
        if let Some(pg) = opts.pgroup.leader {
            // SAFETY: valid pgroup.
            unsafe {
                libc::setpgid(0, pg);
            }
        }

        let envp_ptr = envp.as_ref().map_or_else(
            || unsafe { environ as *const *mut c_char },
            |e: &arrayvec::ArrayVec<*mut c_char, 64>| e.as_ptr(),
        );

        // unblock signals and reset SIGPIPE
        // SAFETY: valid signal mask array manipulation
        let _ = SignalRuntime::unblock_all();
        SignalRuntime::reset_default(libc::SIGPIPE);

        // exec
        // SAFETY: exe_ptr is null-terminated. argv and envp_ptr are valid null-terminated arrays.
        unsafe {
            libc::execve(
                exe_ptr,
                argv.as_ptr() as *const *const _,
                envp_ptr as *const *const _,
            );
            libc::_exit(127);
        }
    }

    // Parent
    drop(pipes.stdin_r.take());
    drop(pipes.stdout_w.take());
    drop(pipes.stderr_w.take());

    let drain = crate::io::DrainState::new(
        job_id,
        pipes.stdin_w.take().filter(|_| opts.stdin.is_some()),
        opts.stdin,
        pipes.stdout_r.take(),
        pipes.stderr_r.take(),
        opts.max_output,
        opts.early_exit,
    )?;

    Ok((pid, drain))
}

enum KillState {
    None,
    TermSent,
    KillSent,
}

fn wait_loop(
    pid: pid_t,
    mut drain: crate::io::DrainState<fn(&[u8]) -> bool>,
    mut reactor: Reactor,
    timeout_ms: Option<u32>,
    kill_grace_ms: u32,
    cancel: CancelPolicy,
    pgroup: ProcessGroup,
) -> Result<Output, SysError> {
    let process = Process::new(pid);
    let mut status_raw = process.wait_step()?;
    let mut state = KillState::None;
    let mut timed_out = false;

    let start_time = std::time::Instant::now();
    let deadline = timeout_ms.map(|t| std::time::Duration::from_millis(t as u64));

    loop {
        let mut poll_timeout = -1;

        if let Some(dl) = deadline {
            let elapsed = start_time.elapsed();
            if elapsed >= dl {
                timed_out = true;
                let elapsed_over = (elapsed - dl).as_millis();

                let target_is_group = pgroup.isolated || pgroup.leader.is_some();

                match state {
                    KillState::None => {
                        if cancel == CancelPolicy::Graceful {
                            let r = if target_is_group {
                                process.kill_pgroup(libc::SIGTERM)
                            } else {
                                process.kill(libc::SIGTERM)
                            };
                            if r.is_err() {
                                state = KillState::KillSent; // Process already gone
                            } else {
                                state = KillState::TermSent;
                            }
                        } else if cancel == CancelPolicy::Kill {
                            let _ = if target_is_group {
                                process.kill_pgroup(libc::SIGKILL)
                            } else {
                                process.kill(libc::SIGKILL)
                            };
                            state = KillState::KillSent;
                        } else {
                            // CancelPolicy::None just times out without killing
                        }
                    }
                    KillState::TermSent if elapsed_over > kill_grace_ms as u128 => {
                        let _ = if target_is_group {
                            process.kill_pgroup(libc::SIGKILL)
                        } else {
                            process.kill(libc::SIGKILL)
                        };
                        state = KillState::KillSent;
                    }
                    _ => {}
                }
                poll_timeout = 100; // Poll frequently while waiting for kill to take effect
            } else {
                let remaining = dl - elapsed;
                poll_timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
            }
        }

        if status_raw.is_none()
            && let Some(s) = process.wait_step()?
        {
            status_raw = Some(s);
        }

        if drain.is_done() {
            let s = match status_raw {
                Some(s) => s,
                None => process.wait_blocking()?,
            };

            for slot in drain.take_all_slots() {
                reactor.del(&slot.fd);
            }
            let (stdout, stderr) = drain.into_parts();
            return Ok(Output {
                pid,
                status: Some(s),
                stdout,
                stderr,
                timed_out,
            });
        }

        let timeout = if status_raw.is_some() {
            if poll_timeout == -1 || poll_timeout > 1 {
                1
            } else {
                poll_timeout
            }
        } else {
            poll_timeout
        };

        let mut events = Vec::new();
        let nevents = reactor.wait(&mut events, 64, timeout)?;

        for ev in events.iter().take(nevents) {
            let fd_token = Some(ev.token);

            if ev.error {
                if drain
                    .stdout_slot
                    .as_ref()
                    .is_some_and(|s| s.token == fd_token)
                {
                    if let Some(slot) = drain.stdout_slot.take() {
                        reactor.del(&slot.fd);
                    }
                } else if drain
                    .stderr_slot
                    .as_ref()
                    .is_some_and(|s| s.token == fd_token)
                {
                    if let Some(slot) = drain.stderr_slot.take() {
                        reactor.del(&slot.fd);
                    }
                } else if drain
                    .stdin_slot
                    .as_ref()
                    .is_some_and(|s| s.token == fd_token)
                    && let Some(slot) = drain.stdin_slot.take()
                {
                    reactor.del(&slot.fd);
                    drain.writer.buf = None;
                }
                continue;
            }

            if drain
                .stdout_slot
                .as_ref()
                .is_some_and(|s| s.token == fd_token)
                && ev.readable
            {
                let _ = drain.read_fd(true)?;
            } else if drain
                .stderr_slot
                .as_ref()
                .is_some_and(|s| s.token == fd_token)
                && ev.readable
            {
                let _ = drain.read_fd(false)?;
            } else if drain
                .stdin_slot
                .as_ref()
                .is_some_and(|s| s.token == fd_token)
                && ev.writable
            {
                let _ = drain.write_stdin()?;
            }
        }
    }
}
