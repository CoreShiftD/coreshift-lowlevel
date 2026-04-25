// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/

//! `coreshift-lowlevel` is a public-preview, policy-neutral substrate for
//! system policy engines.
//!
//! ### Substrate Contract
//! - **OS Boundary**: This module owns all direct interactions with the Linux
//!   kernel, `libc`, and `procfs`.
//! - **Policy Neutral**: No high-level daemon policies, feature-specific logic,
//!   or business rules should exist here.
//! - **Mechanisms, Not Policy**: Provide the building blocks (spawn, reactor,
//!   inotify) that higher layers use to implement policy.
//! - **Preview Discipline**: This crate is still pre-1.0 and may take breaking
//!   cleanup between preview releases when the public API needs tightening.

pub mod inotify;
pub mod io;
pub mod reactor;
pub mod spawn;
pub mod sys;

#[cfg(test)]
mod tests;
