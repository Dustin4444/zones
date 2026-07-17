//! Seeded state-machine properties over the real L1 -> Zone -> L1 path.
//!
//! This test owns correctness: generated actions are applied to a small model
//! and to real in-process Tempo L1 and Zone nodes, then the observable balances,
//! policy decisions, receipts, and bridge settlement are compared after every
//! transition. High-rate randomized transaction generation remains a separate
//! txgen primitive; it does not duplicate these invariants.

use std::{collections::HashMap, time::Duration};

use alloy::{
    network::ReceiptResponse,
    primitives::{Address, B256, U256},
    providers::{Provider, ProviderBuilder},
};
use eyre::WrapErr;
use proptest::{collection::vec, prelude::*};
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};
use tempo_alloy::rpc::TempoCallBuilderExt;
use tempo_chainspec::spec::TEMPO_T1_BASE_FEE;
use tempo_contracts::precompiles::{ITIP20, ITIP403Registry};
use tempo_precompiles::{PATH_USD_ADDRESS, TIP403_REGISTRY_ADDRESS};
use tempo_zone_contracts::ZONE_TOKEN_ADDRESS;

use crate::utils::{
    L1TestNode, TIP20_TX_GAS, ZoneAccount, ZoneTestNode, run_e2e_proptest, spawn_sequencer,
};

const L1_TIMEOUT: Duration = Duration::from_secs(30);
const WITHDRAWAL_TIMEOUT: Duration = Duration::from_secs(60);
const ACCOUNT_DEPOSIT: u128 = 25_000_000;

#[derive(Clone, Copy, Debug)]
enum PolicyMode {
    RejectAll,
    AllowAll,
    Whitelist,
    Blacklist,
    Compound,
}

#[derive(Clone, Copy, Debug)]
enum RecipientClass {
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug)]
struct TransferCase {
    token_index: usize,
    mode: PolicyMode,
    recipient: RecipientClass,
    amount: u128,
}

#[derive(Clone, Copy, Debug)]
struct TokenCase {
    l1: Address,
    l2: Address,
}

#[derive(Clone, Copy, Debug)]
struct PolicyIds {
    whitelist: u64,
    blacklist: u64,
    compound: u64,
}

#[derive(Clone, Debug)]
struct PropertyInput {
    shuffle_seed: u64,
    transfer_amounts: Vec<u128>,
    withdrawal_token_index: usize,
    withdrawal_amount: u128,
}

impl PolicyIds {
    fn for_mode(self, mode: PolicyMode) -> u64 {
        match mode {
            PolicyMode::RejectAll => 0,
            PolicyMode::AllowAll => 1,
            PolicyMode::Whitelist => self.whitelist,
            PolicyMode::Blacklist => self.blacklist,
            PolicyMode::Compound => self.compound,
        }
    }
}

fn property_input_strategy() -> impl Strategy<Value = PropertyInput> {
    (
        any::<u64>(),
        vec(1_000u128..=25_000, 16),
        0usize..2,
        50_000u128..=100_000,
    )
        .prop_map(
            |(shuffle_seed, transfer_amounts, withdrawal_token_index, withdrawal_amount)| {
                PropertyInput {
                    shuffle_seed,
                    transfer_amounts,
                    withdrawal_token_index,
                    withdrawal_amount,
                }
            },
        )
}

fn generated_cases(input: &PropertyInput, token_count: usize) -> Vec<TransferCase> {
    let behaviors = [
        (PolicyMode::RejectAll, RecipientClass::Denied),
        (PolicyMode::AllowAll, RecipientClass::Allowed),
        (PolicyMode::Whitelist, RecipientClass::Allowed),
        (PolicyMode::Whitelist, RecipientClass::Denied),
        (PolicyMode::Blacklist, RecipientClass::Allowed),
        (PolicyMode::Blacklist, RecipientClass::Denied),
        (PolicyMode::Compound, RecipientClass::Allowed),
        (PolicyMode::Compound, RecipientClass::Denied),
    ];

    let mut cases = Vec::with_capacity(token_count * behaviors.len());
    for token_index in 0..token_count {
        for (mode, recipient) in behaviors {
            let amount = input.transfer_amounts[cases.len()];
            cases.push(TransferCase {
                token_index,
                mode,
                recipient,
                amount,
            });
        }
    }
    cases.shuffle(&mut StdRng::seed_from_u64(input.shuffle_seed));
    cases
}

#[test]
fn test_bridge_policy_transfer_state_machine_property() {
    run_e2e_proptest(property_input_strategy(), |input| {
        run_bridge_policy_transfer_property(input)
    });
}

async fn wait_for_policy(
    l1: &L1TestNode,
    zone: &ZoneTestNode,
    token: TokenCase,
    policy_id: u64,
) -> eyre::Result<()> {
    let policy_block = l1.change_transfer_policy_id(token.l1, policy_id).await?;
    zone.wait_for_l2_tempo_finalized(policy_block, L1_TIMEOUT)
        .await?;
    let cache = zone.policy_cache().read();
    let cache_height = cache.last_l1_block();
    let observed = cache.get_token_policy(token.l1, cache_height);
    eyre::ensure!(
        observed == Some(policy_id),
        "finalized policy cache has {observed:?} for token {}, expected {policy_id} at L1 height {cache_height}",
        token.l1
    );
    Ok(())
}

async fn expected_authorization(
    zone: &ZoneTestNode,
    policy_id: u64,
    mode: PolicyMode,
    sender: Address,
    recipient: Address,
) -> eyre::Result<bool> {
    let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, zone.provider());
    if matches!(mode, PolicyMode::Compound) {
        Ok(registry
            .isAuthorizedSender(policy_id, sender)
            .call()
            .await?
            && registry
                .isAuthorizedRecipient(policy_id, recipient)
                .call()
                .await?)
    } else {
        Ok(registry.isAuthorized(policy_id, sender).call().await?
            && registry.isAuthorized(policy_id, recipient).call().await?)
    }
}

async fn transfer_succeeded(
    zone: &ZoneTestNode,
    signer: alloy::signers::local::PrivateKeySigner,
    token: Address,
    recipient: Address,
    amount: u128,
) -> eyre::Result<(bool, String)> {
    let provider = ProviderBuilder::new_with_network::<tempo_alloy::TempoNetwork>()
        .wallet(signer)
        .connect_http(zone.http_url().clone());
    let send = ITIP20::new(token, provider)
        .transfer(recipient, U256::from(amount))
        .fee_token(PATH_USD_ADDRESS)
        .max_fee_per_gas(TEMPO_T1_BASE_FEE as u128)
        .max_priority_fee_per_gas(0)
        .gas(TIP20_TX_GAS)
        .send()
        .await;

    match send {
        Err(err) => Ok((false, format!("submission rejected: {err:#}"))),
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            Ok((
                receipt.status(),
                format!(
                    "receipt status={}, gas_used={}, transaction={}",
                    receipt.status(),
                    receipt.gas_used,
                    receipt.transaction_hash
                ),
            ))
        }
    }
}

/// For every generated transition:
///
/// - a deposit mints exactly the bridged amount on the Zone;
/// - the Zone observes the latest L1 TIP-403 assignment;
/// - an L2 transfer succeeds iff the active policy authorizes both endpoints;
/// - successful transfers change the modeled recipient balance by exactly the
///   transfer amount, while rejected transfers leave it unchanged; and
/// - an L2 withdrawal reduces the Zone balance and settles the same amount on L1.
///
/// Set `PROPTEST_RNG_SEED=<u64>` to replay a generated input and
/// `ZONE_E2E_PROPERTY_CASES=<u32>` to run more complete E2E cases.
async fn run_bridge_policy_transfer_property(input: PropertyInput) -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let case_id = format!("proptest input {input:?}");
    let l1 = L1TestNode::start()
        .await
        .wrap_err_with(|| case_id.clone())?;
    let portal = l1.deploy_zone().await?;
    let zone = ZoneTestNode::start_from_l1(l1.http_url(), l1.ws_url(), portal).await?;
    zone.wait_for_l2_tempo_finalized(0, L1_TIMEOUT).await?;
    eyre::ensure!(
        zone.policy_cache()
            .read()
            .tracked_tokens()
            .contains(&PATH_USD_ADDRESS),
        "startup policy cache did not seed pathUSD"
    );
    let _sequencer = spawn_sequencer(&l1, &zone, portal, l1.dev_signer()).await;

    let custom_token = l1
        .create_tip20("Property USD", "pUSD", B256::with_last_byte(0x43))
        .await?;
    l1.enable_token_on_portal(portal, custom_token).await?;
    let enable_block = l1.provider().get_block_number().await?;
    zone.wait_for_l2_tempo_finalized(enable_block, L1_TIMEOUT)
        .await?;
    eyre::ensure!(
        !zone.provider().get_code_at(custom_token).await?.is_empty(),
        "enabled custom token was not initialized on the Zone"
    );

    let sender_signer = l1.user_signer();
    let sender = sender_signer.address();
    let allowed_recipient = l1.signer_at(3).address();
    let denied_recipient = l1.signer_at(4).address();

    let whitelist = l1.create_whitelist_policy().await?;
    l1.whitelist_address(whitelist, sender).await?;
    l1.whitelist_address(whitelist, allowed_recipient).await?;
    let blacklist = l1.create_blacklist_policy().await?;
    l1.blacklist_address(blacklist, denied_recipient).await?;
    let compound = l1.create_compound_policy(1, whitelist, 1).await?;
    let policy_ids = PolicyIds {
        whitelist,
        blacklist,
        compound,
    };
    let policy_block = l1.provider().get_block_number().await?;
    zone.wait_for_l2_tempo_finalized(policy_block, L1_TIMEOUT)
        .await?;

    let tokens = [
        TokenCase {
            l1: PATH_USD_ADDRESS,
            l2: ZONE_TOKEN_ADDRESS,
        },
        TokenCase {
            l1: custom_token,
            l2: custom_token,
        },
    ];

    l1.fund_user(sender, ACCOUNT_DEPOSIT).await?;
    l1.mint_tip20(custom_token, sender, ACCOUNT_DEPOSIT).await?;
    let mut account = ZoneAccount::from_l1_and_zone(&l1, &zone, portal);
    let path_balance = account.deposit(ACCOUNT_DEPOSIT, L1_TIMEOUT, &zone).await?;
    eyre::ensure!(
        path_balance == U256::from(ACCOUNT_DEPOSIT),
        "pathUSD deposit minted {path_balance}, expected {ACCOUNT_DEPOSIT}"
    );
    let custom_balance = account
        .deposit_token(
            custom_token,
            custom_token,
            ACCOUNT_DEPOSIT,
            L1_TIMEOUT,
            &zone,
        )
        .await?;
    eyre::ensure!(
        custom_balance == U256::from(ACCOUNT_DEPOSIT),
        "custom token deposit minted {custom_balance}, expected {ACCOUNT_DEPOSIT}"
    );

    let mut recipient_model = HashMap::<(Address, Address), U256>::new();
    // TIP20s start on builtin policy 1. Reassigning the same policy is a no-op
    // and emits no TransferPolicyUpdate, so model that initial state directly.
    // A dynamically enabled token will exercise PolicyProvider's L1 fallback
    // if its first generated action also uses policy 1.
    let mut active_policies = [1u64; 2];
    for case in generated_cases(&input, tokens.len()) {
        let token = tokens[case.token_index];
        let policy_id = policy_ids.for_mode(case.mode);
        let recipient = match case.recipient {
            RecipientClass::Allowed => allowed_recipient,
            RecipientClass::Denied => denied_recipient,
        };
        if active_policies[case.token_index] != policy_id {
            wait_for_policy(&l1, &zone, token, policy_id)
                .await
                .wrap_err_with(|| format!("{case_id}, case {case:?}: policy sync failed"))?;
            active_policies[case.token_index] = policy_id;
        }

        let authorized = expected_authorization(&zone, policy_id, case.mode, sender, recipient)
            .await
            .wrap_err_with(|| format!("{case_id}, case {case:?}: authorization failed"))?;
        let (succeeded, transfer_detail) = transfer_succeeded(
            &zone,
            sender_signer.clone(),
            token.l2,
            recipient,
            case.amount,
        )
        .await
        .wrap_err_with(|| format!("{case_id}, case {case:?}: transfer failed"))?;
        eyre::ensure!(
            succeeded == authorized,
            "{case_id}, case {case:?}: receipt success={succeeded}, policy authorization={authorized}; {transfer_detail}"
        );

        let modeled = recipient_model
            .entry((token.l2, recipient))
            .or_insert(U256::ZERO);
        if succeeded {
            *modeled += U256::from(case.amount);
        }
        let observed = zone.balance_of(token.l2, recipient).await?;
        eyre::ensure!(
            observed == *modeled,
            "{case_id}, case {case:?}: recipient balance {observed}, model {modeled}"
        );
    }

    for (token_index, token) in tokens.into_iter().enumerate() {
        if active_policies[token_index] != 1 {
            wait_for_policy(&l1, &zone, token, 1).await?;
        }
    }

    let withdrawal_token = tokens[input.withdrawal_token_index];
    let withdrawal_amount = input.withdrawal_amount;
    let l1_before = l1.balance_of(withdrawal_token.l1, sender).await?;
    let l2_before = zone.balance_of(withdrawal_token.l2, sender).await?;
    account
        .withdraw_token(withdrawal_token.l2, withdrawal_amount)
        .await
        .wrap_err_with(|| format!("{case_id}: withdrawal request failed"))?;
    let l2_after = zone.balance_of(withdrawal_token.l2, sender).await?;
    eyre::ensure!(
        l2_after <= l2_before - U256::from(withdrawal_amount),
        "{case_id}: withdrawal did not reduce the L2 balance by at least {withdrawal_amount}"
    );
    l1.wait_for_balance(
        withdrawal_token.l1,
        sender,
        l1_before + U256::from(withdrawal_amount),
        WITHDRAWAL_TIMEOUT,
    )
    .await
    .wrap_err_with(|| format!("{case_id}: withdrawal did not settle on L1"))?;
    l1.assert_batch_submitted(portal).await?;
    l1.assert_withdrawal_processed(portal, sender, withdrawal_amount)
        .await?;

    Ok(())
}
