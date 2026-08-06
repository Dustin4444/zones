//! Checker-owned protocol vocabulary and independent commitment primitives.
//!
//! This module is deliberately pure. It contains no provider, database, ExEx,
//! clock, async, or production transition-helper dependency. Its byte-level
//! vectors are the expected-value authority for the stateful checker goals that
//! follow Goal 0 in `DESIGN.md`.

pub(crate) mod accounting;
pub(crate) mod adapter;
pub(crate) mod constants;
pub(crate) mod encoding;
pub(crate) mod events;
pub(crate) mod fees;
mod input;
pub(crate) mod output;
pub(crate) mod ownership;
pub(crate) mod state;
pub(crate) mod state_layout;
pub(crate) mod transition;

#[cfg(test)]
mod test_vectors;
