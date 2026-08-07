//! Ephemeral authenticated observations for one Zone block and its imported
//! Tempo block.
//!
//! Concrete input and outcome fields are intentionally separate. Only the L1
//! and L2 adapters in this module construct them; there is no generic wrapper
//! that can relabel arbitrary data as authenticated.

mod abi;
mod error;
mod l1;
mod l2;
mod state;

pub(crate) use abi::{DecodedAdvanceTempo, ImportedDeposit, ImportedTempoHeader};
#[cfg(test)]
pub(crate) use abi::{DecodedPortalCall, decode_portal_call};
pub(crate) use error::{
    AcquisitionError, AcquisitionSource, AuthenticatedDataEvidence, AuthenticatedTransaction,
    DataSource, EnvelopeLocation, EnvelopeRule, ObservationError, PortalCallError,
    PortalCallFamily, ProtocolChain,
};
pub(crate) use l1::{L1BlockObservation, acquire_l1_header, acquire_portal_collateral, observe_l1};
#[cfg(test)]
pub(crate) use l2::observe_l2_block;
pub(crate) use l2::{L2BlockObservation, OrderedL2Outcome, observe_l2_block_with_context};
pub(crate) use state::{ExactStateLookup, ZonePostStateOutputs, acquire_zone_post_state};
