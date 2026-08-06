//! One-block in-memory checking and typed implementation reconciliation.

pub(crate) mod finding;
pub(crate) mod pipeline;
mod reconcile;

#[cfg(test)]
mod tests;
