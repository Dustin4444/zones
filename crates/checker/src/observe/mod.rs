//! Ephemeral authenticated observations for one Zone block and its imported
//! Tempo block.
//!
//! Input and outcome fields remain separate. Only this module's L1 and L2
//! adapters can construct them.

mod abi;
mod error;
pub(crate) mod events;
mod l1;
mod l2;
mod state;

pub(crate) use abi::ImportedTempoHeader;
pub(crate) use error::{AcquisitionError, ObservationError};
#[cfg(test)]
pub(crate) use error::{
    AcquisitionSource, AuthenticatedDataEvidence, AuthenticatedTransaction, DataSource,
    EnvelopeRule, PortalCallError, ProtocolChain,
};
pub(crate) use l1::{
    L1BlockObservation, acquire_l1_header, acquire_portal_token_balance, observe_l1,
    observe_l1_range,
};
pub(crate) use l2::{L2BlockObservation, OrderedL2Outcome, observe_l2_block_with_context};
pub(crate) use state::{ZonePostStateOutputs, acquire_zone_post_state};
