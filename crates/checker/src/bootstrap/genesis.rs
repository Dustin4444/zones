use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_storage_api::StateProviderFactory;

use crate::observe::acquire_zone_post_state;

use super::error::BootstrapError;

pub(crate) fn validate_zero_genesis_supply<P>(
    zone_provider: &P,
    zone_genesis_hash: B256,
    tokens: impl IntoIterator<Item = Address>,
) -> eyre::Result<()>
where
    P: StateProviderFactory + ?Sized,
{
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let outputs = acquire_zone_post_state(zone_provider, zone_genesis_hash, &tokens)?;
    if let Some((&token, &actual)) = outputs
        .token_supplies
        .iter()
        .find(|(_, supply)| !supply.is_zero())
    {
        return Err(BootstrapError::NonzeroZoneGenesisSupply { token, actual }.into());
    }
    Ok(())
}

pub(crate) fn genesis_anchor<P>(
    zone_provider: &P,
    zone_genesis: BlockNumHash,
) -> eyre::Result<BlockNumHash>
where
    P: StateProviderFactory + ?Sized,
{
    let outputs = acquire_zone_post_state(zone_provider, zone_genesis.hash, &[])?;
    if outputs.tempo_block_hash.is_zero() {
        return Err(BootstrapError::UnsupportedBootstrapStyle.into());
    }
    if !outputs.processed_deposit_queue_hash.is_zero()
        || outputs.processed_deposit_number != 0
        || !outputs.withdrawal_queue_hash.is_zero()
        || outputs.withdrawal_batch_index != 0
    {
        return Err(BootstrapError::NonzeroZoneGenesisProgress {
            processed_deposit_queue_hash: outputs.processed_deposit_queue_hash,
            processed_deposit_number: outputs.processed_deposit_number,
            withdrawal_queue_hash: outputs.withdrawal_queue_hash,
            withdrawal_batch_index: outputs.withdrawal_batch_index,
        }
        .into());
    }
    Ok(BlockNumHash::new(
        outputs.tempo_block_number,
        outputs.tempo_block_hash,
    ))
}
