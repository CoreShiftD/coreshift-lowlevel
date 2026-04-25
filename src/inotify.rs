// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! Raw inotify helpers.
//!
//! This module provides low-level interaction with the Linux `inotify` subsystem.
//! It handles the initialization of watches, reading of raw events, and
//! decoding of the packed event stream.
//!
//! Higher-level modules should use these primitives to monitor configuration
//! files, log directories, or process markers.

use crate::reactor::Fd;
use crate::spawn::SysError;

/// A decoded inotify event header.
///
/// This structure represents the fixed-size portion of an `inotify_event`.
///
/// NOTE: The `name` field is currently skipped during decoding. Directory
/// watches that require filtering by child filename must be extended to
/// capture the variable-length name buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InotifyEvent {
    /// Watch descriptor that generated this event.
    pub wd: i32,
    /// Event mask (e.g., [`MODIFY_MASK`]).
    pub mask: u32,
    /// Length of the name field (0 if no name is present).
    pub name_len: u32,
}

/// File was modified.
pub const MODIFY_MASK: u32 = libc::IN_MODIFY;
/// Mask for monitoring package file state changes.
pub const PACKAGE_FILE_MASK: u32 = libc::IN_MODIFY | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;
/// Inotify event queue overflowed.
pub const QUEUE_OVERFLOW_MASK: u32 = libc::IN_Q_OVERFLOW;
/// Watch was removed (explicitly or because file was deleted).
pub const IGNORED_MASK: u32 = libc::IN_IGNORED;
/// Filesystem containing watched object was unmounted.
pub const UNMOUNT_MASK: u32 = libc::IN_UNMOUNT;
/// Watched file/directory was deleted.
pub const DELETE_SELF_MASK: u32 = libc::IN_DELETE_SELF;
/// Watched file/directory was moved.
pub const MOVE_SELF_MASK: u32 = libc::IN_MOVE_SELF;

/// Add a watch to an existing inotify instance.
///
/// # Arguments
/// * `fd` - The inotify file descriptor.
/// * `path` - Path to the file or directory to watch.
/// * `mask` - Events to monitor (e.g., [`MODIFY_MASK`]).
pub fn add_watch(fd: &Fd, path: &str, mask: u32) -> Result<i32, SysError> {
    let path = std::ffi::CString::new(path)
        .map_err(|_| SysError::sys(libc::EINVAL, "inotify path contains nul"))?;
    let wd = unsafe { libc::inotify_add_watch(fd.raw(), path.as_ptr(), mask) };
    if wd < 0 {
        return Err(SysError::sys(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            "inotify_add_watch",
        ));
    }
    Ok(wd)
}

/// Read all available inotify events from the descriptor.
///
/// This function drains the inotify file descriptor until no more events
/// are available (`EAGAIN`). It is safe to use with edge-triggered reactors.
pub fn read_events(fd: &Fd) -> Result<Vec<InotifyEvent>, SysError> {
    let mut all_events = Vec::new();
    let mut buf = vec![0u8; 4096];

    loop {
        match fd.read(buf.as_mut_ptr(), buf.len()) {
            Ok(Some(0)) => break,
            Ok(Some(n)) => {
                all_events.extend(decode_events(&buf[..n]));
            }
            Ok(None) => break, // EAGAIN
            Err(e) => return Err(e),
        }
    }

    Ok(all_events)
}

/// Decode packed inotify events from a raw byte buffer.
///
/// This handles multi-event buffers and handles unaligned reads safely.
/// Truncated events at the end of the buffer are ignored.
pub fn decode_events(buf: &[u8]) -> Vec<InotifyEvent> {
    let mut events = Vec::new();
    let mut offset = 0;
    let base = std::mem::size_of::<libc::inotify_event>();

    while offset + base <= buf.len() {
        // SAFETY: We have at least 'base' bytes. We use read_unaligned to
        // handle potential alignment issues in the raw buffer.
        let event: libc::inotify_event = unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(offset) as *const libc::inotify_event)
        };

        let size = base + event.len as usize;
        if offset + size > buf.len() {
            // Truncated event at the end of the buffer; ignore it.
            break;
        }

        events.push(InotifyEvent {
            wd: event.wd,
            mask: event.mask,
            name_len: event.len,
        });
        offset += size;
    }

    events
}
