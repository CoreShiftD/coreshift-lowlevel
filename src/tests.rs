// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

use crate::inotify::{InotifyEvent, decode_events};
use crate::reactor::{Fd, Reactor};
use crate::spawn::{Process, SpawnOptions, spawn_start};
use crate::sys::{
    CancelPolicy, ExecContext, ProcessGroup, parse_proc_status, path_exists, path_lstat_exists,
};

#[test]
fn test_decode_inotify_events() {
    // Mock multiple inotify_event records.
    let mut buf = Vec::new();

    // Event 1: wd=1, mask=0x2, len=0
    buf.extend_from_slice(&1i32.to_ne_bytes());
    buf.extend_from_slice(&2u32.to_ne_bytes());
    buf.extend_from_slice(&0u32.to_ne_bytes()); // cookie
    buf.extend_from_slice(&0u32.to_ne_bytes()); // len

    // Event 2: wd=2, mask=0x4, len=8 (with name padding)
    buf.extend_from_slice(&2i32.to_ne_bytes());
    buf.extend_from_slice(&4u32.to_ne_bytes());
    buf.extend_from_slice(&0u32.to_ne_bytes()); // cookie
    buf.extend_from_slice(&8u32.to_ne_bytes()); // len
    buf.extend_from_slice(b"file.txt"); // 8 bytes

    // Event 3: truncated (only 8 bytes of header)
    buf.extend_from_slice(&3i32.to_ne_bytes());
    buf.extend_from_slice(&8u32.to_ne_bytes());

    let events = decode_events(&buf);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        InotifyEvent {
            wd: 1,
            mask: 2,
            name_len: 0
        }
    );
    assert_eq!(
        events[1],
        InotifyEvent {
            wd: 2,
            mask: 4,
            name_len: 8
        }
    );
}

#[test]
fn test_parse_proc_status() {
    let content = "Name:\tcore_daemon\nState:\tR (running)\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";
    let status = parse_proc_status(content).unwrap();
    assert_eq!(status.name, "core_daemon");
    assert_eq!(status.uid, 1000);
}

#[test]
fn test_exec_context_validation() {
    // Empty argv
    let res = ExecContext::new(vec![], None, None);
    assert!(res.is_err());

    // Interior NUL in argv
    let res = ExecContext::new(
        vec!["valid".to_string(), "inv\0alid".to_string()],
        None,
        None,
    );
    assert!(res.is_err());

    // Valid
    let res = ExecContext::new(
        vec!["ls".to_string(), "-l".to_string()],
        None,
        Some("/tmp".to_string()),
    );
    assert!(res.is_ok());
}

#[test]
fn test_process_echild() {
    // Use an invalid PID that likely doesn't exist and isn't our child.
    let p = Process::new(999999);
    let res = p.wait_step();
    // Should be an error (ECHILD), not Ok(Some(Exited(0))).
    assert!(res.is_err());
}

#[test]
fn test_spawn_start_wait_false_validation() {
    let ctx = ExecContext::new(
        vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
        None,
        None,
    )
    .unwrap();
    let mut opts = SpawnOptions {
        ctx,
        stdin: None,
        capture_stdout: false,
        capture_stderr: false,
        wait: false,
        pgroup: ProcessGroup::default(),
        max_output: 1024,
        timeout_ms: None,
        kill_grace_ms: 1000,
        cancel: CancelPolicy::Kill,
        backend: crate::spawn::SpawnBackend::Auto,
        early_exit: None,
    };

    // Valid: wait=false, no I/O capture
    assert!(spawn_start(0, opts.clone()).is_ok());

    // Invalid: wait=false, capture_stdout=true
    opts.capture_stdout = true;
    assert!(spawn_start(0, opts.clone()).is_err());

    // Invalid: wait=false, stdin=Some(...)
    opts.capture_stdout = false;
    opts.stdin = Some(vec![1, 2, 3].into_boxed_slice());
    assert!(spawn_start(0, opts.clone()).is_err());
}

#[test]
fn test_reactor_wait_zero_events() {
    let mut reactor = Reactor::new().unwrap();
    let mut events = Vec::new();
    let res = reactor.wait(&mut events, 0, 0);
    assert!(res.is_ok());
    assert_eq!(res.unwrap(), 0);
}

#[test]
fn test_writer_state_epipe() {
    use crate::io::writer::WriterState;

    let mut fds = [0; 2];
    unsafe { libc::pipe(fds.as_mut_ptr()) };
    let r = Fd::new(fds[0], "pipe").unwrap();
    let w = Fd::new(fds[1], "pipe").unwrap();

    let mut writer = WriterState::new(Some(vec![0u8; 1024 * 1024].into_boxed_slice()));

    // Close read end to trigger EPIPE on next write
    drop(r);

    // Some kernels might not trigger EPIPE on the first write if the pipe buffer has space,
    // but with enough data it will fail.
    let mut last_res = Ok(false);
    for _ in 0..100 {
        last_res = writer.write_to_fd(&w);
        if last_res.is_err() || (last_res.is_ok() && writer.buf.is_none()) {
            break;
        }
    }

    // EPIPE should be handled as "done" (Ok(true))
    assert!(last_res.is_ok());
    assert!(last_res.unwrap());
    assert!(writer.buf.is_none());
}

#[test]
fn test_path_existence() {
    let temp_file = std::env::temp_dir().join("coreshift_test_path");
    let path_str = temp_file.to_str().unwrap();

    std::fs::write(&temp_file, "test").unwrap();
    assert!(path_exists(path_str));
    assert!(path_lstat_exists(path_str));

    std::fs::remove_file(&temp_file).unwrap();
    assert!(!path_exists(path_str));
    assert!(!path_lstat_exists(path_str));
}
