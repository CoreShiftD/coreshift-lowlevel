// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! Asynchronous I/O buffering.
//!
//! This module provides the [`BufferState`] structure which accumulates
//! stdout and stderr data from monitored processes.

use crate::reactor::Fd;
use crate::spawn::SysError;

const READ_CHUNK: usize = 65536;

/// Accumulates output from process streams.
///
/// `BufferState` manages the collection of bytes from stdout and stderr pipes.
/// It enforces a combined memory limit to prevent runaway memory usage by
/// misbehaving processes.
#[derive(Default)]
#[repr(align(64))]
pub(crate) struct BufferState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    limit: usize,
}

impl BufferState {
    /// Create a new buffer state with the specified memory limit.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            stdout: Vec::with_capacity(1024),
            stderr: Vec::with_capacity(1024),
            limit,
        }
    }

    /// Drain available data from a file descriptor into internal storage.
    ///
    /// # Returns
    /// * `Ok(true)` if EOF was reached.
    /// * `Ok(false)` if the operation would block (`EAGAIN`).
    #[inline(always)]
    pub(crate) fn read_from_fd(
        &mut self,
        fd: &Fd,
        is_stdout: bool,
        early_exit: &mut Option<impl FnMut(&[u8]) -> bool>,
    ) -> Result<bool, SysError> {
        let dest = if is_stdout {
            &mut self.stdout
        } else {
            &mut self.stderr
        };

        loop {
            let len = dest.len();
            let remaining_limit = self.limit.saturating_sub(len);

            if remaining_limit == 0 {
                // Limit reached, just discard data.
                let mut drop_buf = [0u8; 8192];
                match fd.read(drop_buf.as_mut_ptr(), drop_buf.len()) {
                    Ok(Some(n)) if n > 0 => continue,
                    Ok(Some(_)) => {
                        return Ok(true); // EOF
                    }
                    Ok(None) => {
                        return Ok(false);
                    } // Would block
                    Err(e) => {
                        return Err(e);
                    }
                }
            }

            // Ensure space and read directly into the Vec.
            // We resize with 0s to remain safe (no UB with uninitialized memory).
            let to_read = remaining_limit.min(READ_CHUNK);
            dest.resize(len + to_read, 0);

            match fd.read(dest[len..].as_mut_ptr(), to_read) {
                Ok(Some(n)) if n > 0 => {
                    dest.truncate(len + n);

                    if is_stdout
                        && let Some(f) = early_exit
                        && f(&dest[len..len + n])
                    {
                        return Ok(true); // Early exit implies EOF/done
                    }
                }
                Ok(Some(_)) => {
                    dest.truncate(len);
                    return Ok(true); // EOF
                }
                Ok(None) => {
                    dest.truncate(len);
                    return Ok(false);
                } // Would block
                Err(e) => {
                    dest.truncate(len);
                    return Err(e);
                }
            }
        }
    }

    /// Consume the state and return the accumulated buffers.
    pub(crate) fn into_parts(mut self) -> (Vec<u8>, Vec<u8>) {
        (
            std::mem::take(&mut self.stdout),
            std::mem::take(&mut self.stderr),
        )
    }
}
