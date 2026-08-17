#![allow(clippy::too_many_arguments)]

//! Strict Portal event decoding.

use alloy_primitives::Log;
use alloy_sol_types::SolEvent as _;
use tempo_zone_contracts::{MAX_SEQUENCERS, ZonePortal as Portal};
use zone_precompiles::ecies::ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE;

use super::{
    ProtocolEventError, required_topic, strict_decode_interface, unsupported, validate_exact_bytes,
    validate_token_metadata,
};

pub(super) fn decode(log: &Log) -> Result<Option<Portal::ZonePortalEvents>, ProtocolEventError> {
    match required_topic(log)? {
        Portal::DepositMade::SIGNATURE_HASH
        | Portal::TokenEnabled::SIGNATURE_HASH
        | Portal::BatchSubmitted::SIGNATURE_HASH
        | Portal::WithdrawalProcessed::SIGNATURE_HASH
        | Portal::WithdrawalBounceBack::SIGNATURE_HASH
        | Portal::DepositBounceBack::SIGNATURE_HASH
        | Portal::DepositBounceBackPending::SIGNATURE_HASH
        | Portal::RefundClaimed::SIGNATURE_HASH
        | Portal::BouncebackGasUpdated::SIGNATURE_HASH
        | Portal::SequencerEncryptionKeyUpdated::SIGNATURE_HASH
        | Portal::ZoneGasRateUpdated::SIGNATURE_HASH
        | Portal::MaxTempoGasRateUpdated::SIGNATURE_HASH
        | Portal::AdminTransferStarted::SIGNATURE_HASH
        | Portal::AdminTransferred::SIGNATURE_HASH
        | Portal::RoleUpdated::SIGNATURE_HASH
        | Portal::EnforcementModesUpdated::SIGNATURE_HASH
        | Portal::SequencerSetUpdated::SIGNATURE_HASH
        | Portal::LeaderUpdated::SIGNATURE_HASH
        | Portal::DepositsPaused::SIGNATURE_HASH
        | Portal::DepositsResumed::SIGNATURE_HASH
        | Portal::PortalPaused::SIGNATURE_HASH
        | Portal::PortalResumed::SIGNATURE_HASH
        | Portal::AbdicationScheduled::SIGNATURE_HASH
        | Portal::RpcUrlUpdated::SIGNATURE_HASH => {}
        _ => return Err(unsupported(log)),
    }

    let decoded = strict_decode_interface::<Portal::ZonePortalEvents>(log, "Portal event")?;
    validate_dynamic_bounds(log, &decoded)?;

    let changes_checker_state = match &decoded {
        Portal::ZonePortalEvents::DepositMade(_)
        | Portal::ZonePortalEvents::TokenEnabled(_)
        | Portal::ZonePortalEvents::BatchSubmitted(_)
        | Portal::ZonePortalEvents::WithdrawalProcessed(_)
        | Portal::ZonePortalEvents::WithdrawalBounceBack(_)
        | Portal::ZonePortalEvents::DepositBounceBack(_)
        | Portal::ZonePortalEvents::DepositBounceBackPending(_)
        | Portal::ZonePortalEvents::RefundClaimed(_)
        | Portal::ZonePortalEvents::BouncebackGasUpdated(_) => true,
        Portal::ZonePortalEvents::DepositsPaused(_)
        | Portal::ZonePortalEvents::DepositsResumed(_)
        | Portal::ZonePortalEvents::PortalPaused(_)
        | Portal::ZonePortalEvents::PortalResumed(_)
        | Portal::ZonePortalEvents::AbdicationScheduled(_)
        | Portal::ZonePortalEvents::RpcUrlUpdated(_)
        | Portal::ZonePortalEvents::SequencerEncryptionKeyUpdated(_)
        | Portal::ZonePortalEvents::ZoneGasRateUpdated(_)
        | Portal::ZonePortalEvents::MaxTempoGasRateUpdated(_)
        | Portal::ZonePortalEvents::AdminTransferStarted(_)
        | Portal::ZonePortalEvents::AdminTransferred(_)
        | Portal::ZonePortalEvents::RoleUpdated(_)
        | Portal::ZonePortalEvents::EnforcementModesUpdated(_)
        | Portal::ZonePortalEvents::SequencerSetUpdated(_)
        | Portal::ZonePortalEvents::LeaderUpdated(_) => false,
    };
    Ok(changes_checker_state.then_some(decoded))
}

fn validate_dynamic_bounds(
    log: &Log,
    event: &Portal::ZonePortalEvents,
) -> Result<(), ProtocolEventError> {
    match event {
        Portal::ZonePortalEvents::DepositMade(event) => validate_exact_bytes(
            log,
            "DepositMade",
            "ciphertext",
            event.ciphertext.len(),
            ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE,
        ),
        Portal::ZonePortalEvents::TokenEnabled(event) => validate_token_metadata(
            log,
            "TokenEnabled",
            &event.name,
            &event.symbol,
            &event.currency,
        ),
        Portal::ZonePortalEvents::SequencerSetUpdated(event)
            if event.sequencers.len() > MAX_SEQUENCERS =>
        {
            Err(super::malformed(
                log,
                "SequencerSetUpdated",
                format!(
                    "address array length {} exceeds {MAX_SEQUENCERS}",
                    event.sequencers.len()
                ),
            ))
        }
        _ => Ok(()),
    }
}
