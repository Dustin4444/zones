//! Direct-Portal calldata decoding from authenticated block envelopes.

use alloy_primitives::{Address, B256, TxKind};
use tempo_primitives::TempoTxEnvelope;

use super::super::{
    abi::{DecodedPortalCall, decode_portal_call},
    error::{
        AuthenticatedTransaction, ObservationError, PortalCallError, PortalCallFamily,
        ProtocolChain,
    },
};

pub(super) fn decode_direct_portal_call(
    envelope: &TempoTxEnvelope,
    portal: Address,
    transaction_index: usize,
    transaction_hash: B256,
    expected: PortalCallFamily,
) -> Result<DecodedPortalCall, ObservationError> {
    let calldata = sole_portal_calldata(envelope, portal, transaction_hash)?;
    let coordinate =
        AuthenticatedTransaction::new(ProtocolChain::TempoL1, transaction_index, transaction_hash);
    let decoded = decode_portal_call(calldata, coordinate)?;
    let actual = decoded.family();
    if actual != expected {
        return Err(PortalCallError::FamilyMismatch {
            transaction_hash,
            expected,
            actual,
        }
        .into());
    }
    if expected == PortalCallFamily::ProcessWithdrawals
        && !decoded.is_nonempty_process_withdrawals()
    {
        return Err(PortalCallError::EmptyProcessWithOutcomes { transaction_hash }.into());
    }
    Ok(decoded)
}

pub(super) fn sole_portal_calldata(
    envelope: &TempoTxEnvelope,
    portal: Address,
    transaction_hash: B256,
) -> Result<&[u8], ObservationError> {
    let mut calls = envelope.calls();
    let Some((kind, calldata)) = calls.next() else {
        return Err(PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target: None,
        }
        .into());
    };
    let target = match kind {
        TxKind::Call(target) => Some(target),
        TxKind::Create => None,
    };
    if calls.next().is_some() || target != Some(portal) {
        return Err(PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target,
        }
        .into());
    }
    Ok(calldata)
}
