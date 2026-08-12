//! Replays authenticated Tempo imports into the checker kernel.

use alloy_provider::DynProvider;
use tempo_alloy::TempoNetwork;

use crate::{
    CheckerConfig,
    adapter::adapt_imported,
    kernel::{State, apply_imported},
    observe::{ImportedTempoHeader, L1BlockObservation, acquire_portal_token_balance},
};

/// Apply one authenticated import and verify its effects and Portal collateral.
pub(super) async fn imported_block(
    state: &mut State,
    observation: &L1BlockObservation,
    header: &ImportedTempoHeader,
    config: &CheckerConfig,
    provider: &DynProvider<TempoNetwork>,
) -> eyre::Result<()> {
    let adaptation = adapt_imported(
        observation,
        header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?;
    let facts = adaptation.facts;
    let effects = adaptation.effects;
    let candidate = apply_imported(state, &facts)?;
    if effects != candidate.expected_effects() {
        eyre::bail!("imported effects differ from expected effects");
    }
    for (token, accounting) in candidate.expected_accounting()? {
        let actual = acquire_portal_token_balance(
            provider,
            token,
            observation.portal_address(),
            observation.block_hash(),
        )
        .await?;
        if accounting
            .collateral()
            .is_none_or(|required| actual < required)
        {
            eyre::bail!("imported collateral is insufficient for token {token}");
        }
    }
    *state = candidate.into_state();
    Ok(())
}
