// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! High-level process I/O management.
//!
//! This module provides the [`DrainState`] structure, which coordinates the
//! simultaneous reading from process output pipes and writing to process
//! input pipes.

use crate::io::buffer::BufferState;
use crate::io::writer::WriterState;
use crate::reactor::{Fd, Token};
use crate::spawn::SysError;

/// Associates a file descriptor with an optional reactor token.
pub struct FdSlot {
    /// Token assigned by the reactor for this descriptor.
    pub token: Option<Token>,
    /// The managed file descriptor.
    pub fd: Fd,
}

/// Orchestrates non-blocking process I/O.
///
/// `DrainState` tracks the state of stdin, stdout, and stderr pipes for a
/// single process. It handles the multiplexing of data between these pipes
/// and internal buffers.
///
/// # Example
/// ```no_run
/// # use coreshift_lowlevel::io::DrainState;
/// # use coreshift_lowlevel::reactor::Reactor;
/// # fn example(mut drain: DrainState<fn(&[u8]) -> bool>, mut reactor: Reactor) -> Result<(), Box<dyn std::error::Error>> {
/// while !drain.is_done() {
///     let mut events = Vec::new();
///     reactor.wait(&mut events, 64, -1)?;
///     for ev in events {
///         // Map event tokens to drain calls...
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[repr(align(64))]
pub struct DrainState<F>
where
    F: FnMut(&[u8]) -> bool,
{
    pub(crate) stdout_slot: Option<FdSlot>,
    pub(crate) stderr_slot: Option<FdSlot>,
    pub(crate) stdin_slot: Option<FdSlot>,

    pub(crate) buffer: BufferState,
    pub(crate) writer: WriterState,

    pub(crate) early_exit: Option<F>,
}

impl<F> DrainState<F>
where
    F: FnMut(&[u8]) -> bool,
{
    /// Initialize a new drain state for the provided descriptors.
    ///
    /// This consumes the descriptors and sets them to non-blocking mode.
    pub fn new(
        _job_id: u64,
        stdin_fd: Option<Fd>,
        stdin_buf: Option<Box<[u8]>>,
        stdout_fd: Option<Fd>,
        stderr_fd: Option<Fd>,
        limit: usize,
        early_exit: Option<F>,
    ) -> Result<Self, SysError> {
        let mut stdin_slot = None;
        let mut stdout_slot = None;
        let mut stderr_slot = None;

        // Tokens remain purely unassigned until explicitly mapped by a Reactor
        if let (Some(fd), Some(_)) = (&stdin_fd, &stdin_buf) {
            fd.set_nonblock()?;
            stdin_slot = Some(FdSlot {
                token: None,
                fd: stdin_fd.unwrap(),
            });
        }

        if let Some(fd) = &stdout_fd {
            fd.set_nonblock()?;
            stdout_slot = Some(FdSlot {
                token: None,
                fd: stdout_fd.unwrap(),
            });
        }

        if let Some(fd) = &stderr_fd {
            fd.set_nonblock()?;
            stderr_slot = Some(FdSlot {
                token: None,
                fd: stderr_fd.unwrap(),
            });
        }

        Ok(Self {
            stdin_slot,
            stdout_slot,
            stderr_slot,
            buffer: BufferState::new(limit),
            writer: WriterState::new(stdin_buf),
            early_exit,
        })
    }

    /// Returns `true` if all pipes have been closed or fully drained.
    #[inline(always)]
    pub fn is_done(&self) -> bool {
        self.stdin_slot.is_none() && self.stdout_slot.is_none() && self.stderr_slot.is_none()
    }

    /// Perform a non-blocking write to stdin if pending.
    #[inline(always)]
    pub fn write_stdin(&mut self) -> Result<Option<FdSlot>, SysError> {
        let fd = if let Some(s) = &self.stdin_slot {
            &s.fd
        } else {
            return Ok(None);
        };

        let done = self.writer.write_to_fd(fd)?;
        if done {
            let slot = self.stdin_slot.take();
            return Ok(slot);
        }
        Ok(None)
    }

    /// Perform a non-blocking read from stdout or stderr.
    #[inline(always)]
    pub fn read_fd(&mut self, is_stdout: bool) -> Result<Option<FdSlot>, SysError> {
        let eof = {
            let slot = if is_stdout {
                &self.stdout_slot
            } else {
                &self.stderr_slot
            };
            let fd = if let Some(s) = slot {
                &s.fd
            } else {
                return Ok(None);
            };
            self.buffer
                .read_from_fd(fd, is_stdout, &mut self.early_exit)?
        };

        if eof {
            if is_stdout {
                let slot = self.stdout_slot.take();
                return Ok(slot);
            } else {
                let slot = self.stderr_slot.take();
                return Ok(slot);
            }
        }

        Ok(None)
    }

    /// Extract all active slots for cleanup or reactor removal.
    pub fn take_all_slots(&mut self) -> Vec<FdSlot> {
        let mut slots = Vec::new();
        if let Some(slot) = self.stdin_slot.take() {
            slots.push(slot);
        }
        if let Some(slot) = self.stdout_slot.take() {
            slots.push(slot);
        }
        if let Some(slot) = self.stderr_slot.take() {
            slots.push(slot);
        }
        slots
    }

    /// Consume the state and return (stdout, stderr) buffers.
    pub fn into_parts(mut self) -> (Vec<u8>, Vec<u8>) {
        std::mem::take(&mut self.buffer).into_parts()
    }
}
