//! Dedicated typed MDBX persistence for the checker model.

// Reth's `tables!` macro emits public marker types whose associated key/value
// types must also be public. This module is private to the crate, so those
// schema types remain unreachable outside `zone-checker` by construction.
#![allow(unreachable_pub)]

pub(super) const SCHEMA_VERSION: u8 = 1;

mod codec;
pub(crate) mod db;
pub(crate) mod error;
pub(crate) mod history;
pub(crate) mod model_state;
pub(crate) mod operations;
pub(crate) mod schema;
#[cfg(test)]
mod tests;
pub(crate) mod value;
