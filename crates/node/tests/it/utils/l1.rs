//! In-process Tempo L1 dev node harness ([`L1TestNode`]) and L1-side helpers.

use super::*;

use alloy::genesis::{Genesis, GenesisAccount};
use alloy_network::{EthereumWallet, ReceiptResponse};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use alloy_signer::SignerSync;
use alloy_signer_local::{MnemonicBuilder, coins_bip39::English};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use eyre::WrapErr;
use k256::{AffinePoint, ProjectivePoint, Scalar, elliptic_curve::sec1::ToEncodedPoint};
use reth_node_builder::{NodeBuilder, NodeConfig};
use reth_node_core::args::RpcServerArgs;
use reth_rpc_builder::RpcModuleSelection;
use reth_tasks::Runtime;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tempo_alloy::{TempoNetwork, rpc::TempoCallBuilderExt};
use tempo_chainspec::{hardfork::TempoHardfork, spec::TempoChainSpec};
use tempo_contracts::precompiles::{
    IRolesAuth, ITIP20, ITIP20Factory, ITIP403Registry, TIP403_REGISTRY_ADDRESS,
};
use tempo_precompiles::{
    PATH_USD_ADDRESS, TIP20_FACTORY_ADDRESS,
    storage::{Handler, PrecompileStorageProvider, StorageCtx, hashmap::HashMapStorageProvider},
    tip20::ISSUER_ROLE,
    tip403_registry::{
        ALLOW_ALL_POLICY_ID, AuthRole, CompoundPolicyData as RawCompoundPolicyData, PolicyData,
        PolicyType, TIP403Registry, tip403_registry_slots,
    },
};
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{
    ZONE_FACTORY_ADDRESS, ZONE_MESSENGER_ADDRESS, ZONE_PORTAL_IMPL_ADDRESS, ZONE_VERIFIER_ADDRESS,
    ZoneFactory,
    ZonePortal::{self, Role as PortalRole},
};
use zone_l1::L1StateCache;
use zone_precompiles::ecies;

pub(crate) fn enabled_deposits_active_token_config() -> B256 {
    let mut value = [0u8; 32];
    value[30] = 1; // TokenConfig.depositsActive
    value[31] = 1; // TokenConfig.enabled
    B256::new(value)
}

alloy_sol_types::sol! {
    #[sol(rpc)]
    contract TestStablecoinDEX {
        function createPair(address base) external returns (bytes32 key);
        function place(address token, uint128 amount, bool isBid, int16 tick) external returns (uint128 orderId);
        function quoteSwapExactAmountIn(address tokenIn, address tokenOut, uint128 amountIn) external view returns (uint128 amountOut);
    }

    #[sol(rpc)]
    contract TestZonePortalAdmin {
        function pauseDeposits(address token) external;
        function resumeDeposits(address token) external;
        function areDepositsActive(address token) external view returns (bool);
    }
}

/// Read a Foundry artifact from `specs/ref-impls/out` and return its deployment bytecode.
///
/// Requires `forge build` to have been run in `specs/ref-impls`.
pub(crate) fn forge_bytecode(contract: &str) -> eyre::Result<alloy_primitives::Bytes> {
    let specs_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/ref-impls/out");
    let path = specs_dir.join(format!("{contract}.sol/{contract}.json"));
    let json = std::fs::read_to_string(&path).wrap_err_with(|| {
        format!("{contract} artifact not found – run `forge build` in specs/ref-impls")
    })?;
    let artifact: serde_json::Value = serde_json::from_str(&json)?;
    let hex_str = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("missing bytecode in {contract} artifact"))?;
    Ok(alloy_primitives::Bytes::from(
        alloy_primitives::hex::decode(hex_str)?,
    ))
}

fn forge_deployed_bytecode(contract: &str) -> eyre::Result<alloy_primitives::Bytes> {
    let specs_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/ref-impls/out");
    let path = specs_dir.join(format!("{contract}.sol/{contract}.json"));
    let json = std::fs::read_to_string(&path).wrap_err_with(|| {
        format!("{contract} artifact not found – run `forge build` in specs/ref-impls")
    })?;
    let artifact: serde_json::Value = serde_json::from_str(&json)?;
    let hex_str = artifact["deployedBytecode"]["object"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("missing deployed bytecode in {contract} artifact"))?;
    Ok(alloy_primitives::Bytes::from(
        alloy_primitives::hex::decode(hex_str)?,
    ))
}

fn install_native_zone_factory(genesis: &mut Genesis, owner: Address) -> eyre::Result<()> {
    // Native TIP-1091 accounts use the non-empty 0xEF precompile marker. Slot 0 packs
    // `uint32 nextZoneId`, `address owner`, and the implementation lock flag.
    let packed_factory_config: U256 = U256::ONE | (U256::from_be_slice(owner.as_slice()) << 32);
    let mut factory_storage = BTreeMap::new();
    factory_storage.insert(B256::ZERO, B256::from(packed_factory_config.to_be_bytes()));

    genesis.alloc.insert(
        ZONE_FACTORY_ADDRESS,
        GenesisAccount::default()
            .with_nonce(Some(1))
            .with_code(Some(vec![0xef].into()))
            .with_storage(Some(factory_storage)),
    );
    genesis.alloc.insert(
        ZONE_VERIFIER_ADDRESS,
        GenesisAccount::default()
            .with_nonce(Some(1))
            .with_code(Some(forge_deployed_bytecode("Verifier")?)),
    );
    genesis.alloc.insert(
        ZONE_PORTAL_IMPL_ADDRESS,
        GenesisAccount::default()
            .with_nonce(Some(1))
            .with_code(Some(forge_deployed_bytecode("ZonePortal")?)),
    );
    genesis.alloc.insert(
        ZONE_MESSENGER_ADDRESS,
        GenesisAccount::default()
            .with_nonce(Some(1))
            .with_code(Some(forge_deployed_bytecode("ZoneMessenger")?)),
    );

    // The native factory requires the initial token's TIP-403 policy binding to exist.
    let token_policy_slot = keccak256(
        (
            PATH_USD_ADDRESS,
            tip403_registry_slots::TOKEN_TRANSFER_POLICIES,
        )
            .abi_encode(),
    );
    let packed_policy = U256::from(ALLOW_ALL_POLICY_ID) | (U256::ONE << u64::BITS);
    genesis
        .alloc
        .entry(TIP403_REGISTRY_ADDRESS)
        .or_default()
        .storage
        .get_or_insert_default()
        .insert(token_policy_slot, B256::from(packed_policy.to_be_bytes()));

    Ok(())
}

/// Helper to check TIP-403 authorization via TIP-20 operations.
///
/// Direct zone calls to TIP-403 registry are forbidden, so tests trigger checks using a TIP20 call.
pub(crate) struct Check403Registry {
    pub(crate) provider: DynProvider,
    pub(crate) token: Address,
}

impl Check403Registry {
    pub(crate) async fn is_auth_as(&self, from: Address, to: Address, role: AuthRole) -> bool {
        let (token, zero) = (ITIP20::new(self.token, &self.provider), U256::ZERO);
        match role {
            AuthRole::Transfer => token.transfer(from, zero).from(from).call().await.is_ok(),
            AuthRole::Sender => token.transfer(to, zero).from(from).call().await.is_ok(),
            AuthRole::Recipient => token.transfer(from, zero).from(to).call().await.is_ok(),
            AuthRole::MintRecipient => token.mint(from, zero).from(to).call().await.is_ok(),
        }
    }
}

/// Seed a TIP-1092 token-policy binding in the TIP-403 registry's raw L1 storage.
pub(crate) fn seed_raw_tip403_token_policy(
    cache: &mut zone_l1::state::L1StateCacheInner,
    block_number: u64,
    token: Address,
    policy_id: u64,
) {
    let slot = keccak256((token, tip403_registry_slots::TOKEN_TRANSFER_POLICIES).abi_encode());
    let packed: U256 = U256::from(policy_id) | (U256::ONE << 64);
    cache.set(
        TIP403_REGISTRY_ADDRESS,
        slot,
        block_number,
        B256::from(packed.to_be_bytes()),
    );
}

/// A TIP-403 policy write for [`seed_raw_tip403_policy`].
pub(crate) struct PolicySeed<'a> {
    pub(crate) id: u64,
    pub(crate) ty: PolicyType,
    pub(crate) members: &'a [(Address, bool)],
    pub(crate) compound: Option<(u64, u64, u64)>,
}

impl<'a> PolicySeed<'a> {
    pub(crate) fn simple(id: u64, ty: PolicyType, members: &'a [(Address, bool)]) -> Self {
        Self {
            id,
            ty,
            members,
            compound: None,
        }
    }

    pub(crate) fn compound(id: u64, sender: u64, recipient: u64, mint_recipient: u64) -> Self {
        Self {
            id,
            ty: PolicyType::COMPOUND,
            members: &[],
            compound: Some((sender, recipient, mint_recipient)),
        }
    }
}

/// Materialize one or more TIP-403 policy writes into the raw L1 cache.
/// A batch shares a single storage snapshot, so multiple policy writes can reference each other.
pub(crate) fn seed_raw_tip403_policy(
    cache: &L1StateCache,
    block_number: u64,
    policies: &[PolicySeed<'_>],
) -> eyre::Result<()> {
    let mut storage = HashMapStorageProvider::new_with_spec(1, TempoHardfork::T8);
    let registry = TIP403Registry::new();
    let counter_slot = registry.policy_id_counter.slot();
    let existing_next_policy_id = cache
        .lock()
        .get(TIP403_REGISTRY_ADDRESS, counter_slot.into(), block_number)
        .and_then(|value| U256::from_be_bytes(value.0).try_into().ok())
        .unwrap_or(2u64);
    let mut slots = vec![counter_slot];
    for policy in policies {
        slots.push(registry.policy_records[policy.id].base.base_slot());
        if policy.compound.is_some() {
            slots.push(registry.policy_records[policy.id].compound.base_slot());
        }
        slots.extend(
            policy
                .members
                .iter()
                .map(|(account, _)| registry.policy_set[policy.id][*account].slot()),
        );
    }

    StorageCtx::enter(&mut storage, || -> tempo_precompiles::Result<()> {
        let mut registry = TIP403Registry::new();
        let next_policy_id = policies
            .iter()
            .map(|policy| policy.id + 1)
            .max()
            .unwrap_or(2)
            .max(existing_next_policy_id);
        registry.policy_id_counter.write(next_policy_id)?;
        for policy in policies {
            registry.policy_records[policy.id].base.write(PolicyData {
                policy_type: policy.ty as u8,
                admin: Address::ZERO,
            })?;
            if let Some((sender, recipient, mint_recipient)) = policy.compound {
                registry.policy_records[policy.id]
                    .compound
                    .write(RawCompoundPolicyData {
                        sender_policy_id: sender,
                        recipient_policy_id: recipient,
                        mint_recipient_policy_id: mint_recipient,
                    })?;
            }
            for &(account, in_set) in policy.members {
                registry.policy_set[policy.id][account].write(in_set)?;
            }
        }
        Ok(())
    })?;

    let mut cache = cache.lock();
    for slot in slots {
        let value = storage.sload(TIP403_REGISTRY_ADDRESS, slot)?;
        cache.set(
            TIP403_REGISTRY_ADDRESS,
            slot.into(),
            block_number,
            value.into(),
        );
    }
    Ok(())
}

/// A Tempo L1 node running in dev mode for integration testing.
///
/// Starts an in-process Tempo node that produces blocks automatically
/// (500ms block time), providing both HTTP and WebSocket endpoints.
///
/// # Usage
///
/// ```ignore
/// let l1 = L1TestNode::start().await?;
/// let provider = ProviderBuilder::new().connect_http(l1.http_url().clone());
/// let zone = ZoneTestNode::start_from_l1(l1.http_url(), l1.ws_url(), Address::ZERO).await?;
/// ```
pub(crate) struct L1TestNode {
    http_url: url::Url,
    ws_url: url::Url,
    _node_handle: Box<dyn TestNodeHandle>,
    _tasks: Runtime,
}

/// Explicit account-access and callback-gateway configuration for a test zone.
#[derive(Clone, Debug)]
pub(crate) struct ZoneCreationConfig {
    pub(crate) access_mode: bool,
    pub(crate) gateway_mode: bool,
    pub(crate) allowed_accounts: Vec<Address>,
    pub(crate) zone_gateways: Vec<Address>,
}

impl ZoneCreationConfig {
    pub(crate) fn closed(mut allowed_accounts: Vec<Address>) -> Self {
        allowed_accounts.sort_unstable();
        allowed_accounts.dedup();
        Self {
            access_mode: true,
            gateway_mode: true,
            allowed_accounts,
            zone_gateways: Vec::new(),
        }
    }

    pub(crate) fn open() -> Self {
        Self {
            access_mode: false,
            gateway_mode: false,
            allowed_accounts: Vec::new(),
            zone_gateways: Vec::new(),
        }
    }

    pub(crate) fn open_with_enforced_gateways() -> Self {
        Self {
            gateway_mode: true,
            ..Self::open()
        }
    }
}

impl L1TestNode {
    /// Returns the HTTP RPC URL for this L1 node.
    pub(crate) fn http_url(&self) -> &url::Url {
        &self.http_url
    }

    /// Returns the WebSocket RPC URL for this L1 node.
    pub(crate) fn ws_url(&self) -> &url::Url {
        &self.ws_url
    }

    /// Returns an unsigned HTTP provider connected to this L1 node.
    pub(crate) fn provider(&self) -> alloy_provider::DynProvider {
        ProviderBuilder::new()
            .connect_http(self.http_url.clone())
            .erased()
    }

    /// Returns a signer for the pre-funded dev account.
    ///
    /// This is the first key derived from [`TEST_MNEMONIC`] (`test test … junk`),
    /// corresponding to address `0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`.
    /// The account is pre-funded with pathUSD in `test-genesis.json`.
    pub(crate) fn dev_signer(&self) -> alloy_signer_local::PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .build()
            .expect("valid test mnemonic")
    }

    /// Returns the address of the pre-funded dev account.
    pub(crate) fn dev_address(&self) -> Address {
        self.dev_signer().address()
    }

    /// Returns the signer used as the ZonePortal admin (mnemonic index 2).
    ///
    /// Distinct from the dev account (which acts as the sequencer) so the test
    /// suite exercises the admin/sequencer role separation. This account is NOT
    /// pre-funded; [`create_zone`](Self::create_zone) funds it with pathUSD for
    /// gas so it can make admin-only portal calls.
    pub(crate) fn admin_signer(&self) -> alloy_signer_local::PrivateKeySigner {
        self.signer_at(2)
    }

    /// Returns the address of the ZonePortal admin account.
    pub(crate) fn admin_address(&self) -> Address {
        self.admin_signer().address()
    }

    /// Returns a signer for the second test account (mnemonic index 1).
    ///
    /// This account is NOT pre-funded — use [`fund_user`](Self::fund_user) to
    /// transfer pathUSD from the dev account before depositing.
    pub(crate) fn user_signer(&self) -> alloy_signer_local::PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .index(1)
            .expect("valid derivation index")
            .build()
            .expect("valid test mnemonic")
    }

    /// Returns a signer derived from [`TEST_MNEMONIC`] at the given BIP-44 index.
    pub(crate) fn signer_at(&self, index: u32) -> alloy_signer_local::PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .index(index)
            .expect("valid derivation index")
            .build()
            .expect("valid test mnemonic")
    }

    /// Transfer pathUSD from the dev account to a recipient on L1.
    pub(crate) async fn fund_user(&self, to: Address, amount: u128) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let receipt = ITIP20::new(PATH_USD_ADDRESS, &provider)
            .transfer(to, U256::from(amount))
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "fund_user transfer failed");
        Ok(())
    }

    /// Read a TIP-20 token balance on L1 (single-shot, no polling).
    pub(crate) async fn balance_of(&self, token: Address, account: Address) -> eyre::Result<U256> {
        Ok(ITIP20::new(token, self.provider())
            .balanceOf(account)
            .call()
            .await?)
    }

    /// Wait for a TIP-20 token balance to reach at least `min_balance` on L1.
    pub(crate) async fn wait_for_balance(
        &self,
        token: Address,
        account: Address,
        min_balance: U256,
        timeout: Duration,
    ) -> eyre::Result<U256> {
        let tip20 = ITIP20::new(token, self.provider());
        poll_until(timeout, DEFAULT_POLL, "L1 token balance", || {
            let tip20 = &tip20;
            async move {
                let balance = tip20.balanceOf(account).call().await?;
                if balance >= min_balance {
                    Ok(Some(balance))
                } else {
                    Ok(None)
                }
            }
        })
        .await
    }

    /// Assert that a `BatchSubmitted` event exists on the portal.
    pub(crate) async fn assert_batch_submitted(&self, portal_address: Address) -> eyre::Result<()> {
        let portal = ZonePortal::new(portal_address, self.provider());
        let events = portal.BatchSubmitted_filter().from_block(0).query().await?;
        eyre::ensure!(
            !events.is_empty(),
            "expected at least one BatchSubmitted event on L1"
        );
        Ok(())
    }

    /// Assert that a `WithdrawalProcessed` event exists on the portal matching `to` and `amount`.
    pub(crate) async fn assert_withdrawal_processed(
        &self,
        portal_address: Address,
        to: Address,
        amount: u128,
    ) -> eyre::Result<()> {
        let portal = ZonePortal::new(portal_address, self.provider());
        let events = portal
            .WithdrawalProcessed_filter()
            .from_block(0)
            .query()
            .await?;
        let found = events.iter().any(|(e, _)| e.to == to && e.amount == amount);
        eyre::ensure!(
            found,
            "expected WithdrawalProcessed event for {to} with amount {amount}"
        );
        Ok(())
    }

    /// Assert that a `WithdrawalProcessed` event exists with the expected callback result.
    pub(crate) async fn assert_withdrawal_processed_with_status(
        &self,
        portal_address: Address,
        to: Address,
        token: Address,
        amount: u128,
        callback_success: bool,
    ) -> eyre::Result<()> {
        let portal = ZonePortal::new(portal_address, self.provider());
        let events = portal
            .WithdrawalProcessed_filter()
            .from_block(0)
            .query()
            .await?;
        let found = events.iter().any(|(e, _)| {
            e.to == to
                && e.token == token
                && e.amount == amount
                && e.callbackSuccess == callback_success
        });
        eyre::ensure!(
            found,
            "expected WithdrawalProcessed event for {to} with token {token} amount {amount} and callbackSuccess={callback_success}"
        );
        Ok(())
    }

    /// Wait for a matching withdrawal result and return its callback status.
    pub(crate) async fn wait_for_withdrawal_processed_status(
        &self,
        portal_address: Address,
        to: Address,
        token: Address,
        amount: u128,
        timeout: Duration,
    ) -> eyre::Result<bool> {
        let portal = ZonePortal::new(portal_address, self.provider());
        poll_until(timeout, DEFAULT_POLL, "WithdrawalProcessed event", || {
            let portal = &portal;
            async move {
                let events = portal
                    .WithdrawalProcessed_filter()
                    .from_block(0)
                    .query()
                    .await?;
                Ok(events
                    .iter()
                    .find(|(event, _)| {
                        event.to == to && event.token == token && event.amount == amount
                    })
                    .map(|(event, _)| event.callbackSuccess))
            }
        })
        .await
    }

    /// Assert that matching withdrawal results were emitted in FIFO order.
    pub(crate) async fn assert_withdrawals_processed_in_order(
        &self,
        portal_address: Address,
        expected: &[(Address, Address, u128, bool)],
    ) -> eyre::Result<()> {
        let portal = ZonePortal::new(portal_address, self.provider());
        let events = portal
            .WithdrawalProcessed_filter()
            .from_block(0)
            .query()
            .await?;
        let mut expected_index = 0;
        for (event, _) in events {
            if let Some((to, token, amount, success)) = expected.get(expected_index)
                && event.to == *to
                && event.token == *token
                && event.amount == *amount
                && event.callbackSuccess == *success
            {
                expected_index += 1;
            }
        }
        eyre::ensure!(
            expected_index == expected.len(),
            "expected ordered withdrawal results {expected:?}, matched only {expected_index}"
        );
        Ok(())
    }

    /// Returns an HTTP provider with the dev account wallet attached.
    pub(crate) fn dev_provider(&self) -> alloy_provider::DynProvider {
        ProviderBuilder::new()
            .wallet(self.dev_signer())
            .connect_http(self.http_url.clone())
            .erased()
    }

    /// Returns an HTTP provider with the admin account wallet attached.
    ///
    /// Used for `onlyAdmin` portal calls so they are signed by the admin key
    /// rather than the dev (sequencer) key.
    pub(crate) fn admin_provider(&self) -> alloy_provider::DynProvider {
        ProviderBuilder::new()
            .wallet(self.admin_signer())
            .connect_http(self.http_url.clone())
            .erased()
    }

    /// Returns an HTTP provider with an explicit signer attached.
    pub(crate) fn provider_with_signer(
        &self,
        signer: alloy_signer_local::PrivateKeySigner,
    ) -> alloy_provider::DynProvider {
        ProviderBuilder::new()
            .wallet(signer)
            .connect_http(self.http_url.clone())
            .erased()
    }

    /// Create a zone through the native ZoneFactory.
    ///
    /// Combines [`native_zone_factory`](Self::native_zone_factory) and
    /// [`create_zone`](Self::create_zone). Returns the portal address.
    pub(crate) async fn deploy_zone(&self) -> eyre::Result<Address> {
        let factory = self.native_zone_factory().await?;
        self.create_zone(factory).await
    }

    /// Wait for a withdrawal to be fully processed on L1 (pathUSD).
    ///
    /// Polls the account's L1 token balance until it increases by at least
    /// `amount`, then asserts both `BatchSubmitted` and `WithdrawalProcessed`
    /// events exist on the portal.
    pub(crate) async fn wait_for_withdrawal_on_l1(
        &self,
        portal_address: Address,
        account: Address,
        amount: u128,
        timeout: Duration,
    ) -> eyre::Result<()> {
        self.wait_for_withdrawal_on_l1_token(
            portal_address,
            PATH_USD_ADDRESS,
            account,
            amount,
            timeout,
        )
        .await
    }

    /// Wait for a withdrawal of a specific token to be fully processed on L1.
    pub(crate) async fn wait_for_withdrawal_on_l1_token(
        &self,
        portal_address: Address,
        token: Address,
        account: Address,
        amount: u128,
        timeout: Duration,
    ) -> eyre::Result<()> {
        let balance_before = self.balance_of(token, account).await?;
        let expected = balance_before + U256::from(amount);
        self.wait_for_balance(token, account, expected, timeout)
            .await?;
        self.assert_batch_submitted(portal_address).await?;
        self.assert_withdrawal_processed(portal_address, account, amount)
            .await
    }

    /// Create a StablecoinDEX pair for a base token.
    pub(crate) async fn create_dex_pair(&self, base_token: Address) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let dex = TestStablecoinDEX::new(STABLECOIN_DEX_ADDRESS, &provider);
        let receipt = dex
            .createPair(base_token)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createPair failed for {base_token}");
        Ok(())
    }

    /// Place a bid order on the StablecoinDEX using the dev account.
    pub(crate) async fn place_dex_bid_order(
        &self,
        base_token: Address,
        amount: u128,
        tick: i16,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let quote_token = ITIP20::new(base_token, &provider)
            .quoteToken()
            .call()
            .await?;

        ITIP20::new(quote_token, &provider)
            .approve(STABLECOIN_DEX_ADDRESS, U256::MAX)
            .send()
            .await?
            .get_receipt()
            .await?;

        let dex = TestStablecoinDEX::new(STABLECOIN_DEX_ADDRESS, &provider);
        let receipt = dex
            .place(base_token, amount, true, tick)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(
            receipt.status(),
            "place bid order failed for {base_token} amount {amount} at tick {tick}"
        );
        Ok(())
    }

    /// Place an ask order on the StablecoinDEX using the dev account.
    pub(crate) async fn place_dex_ask_order(
        &self,
        base_token: Address,
        amount: u128,
        tick: i16,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();
        ITIP20::new(base_token, &provider)
            .approve(STABLECOIN_DEX_ADDRESS, U256::MAX)
            .send()
            .await?
            .get_receipt()
            .await?;

        let dex = TestStablecoinDEX::new(STABLECOIN_DEX_ADDRESS, &provider);
        let receipt = dex
            .place(base_token, amount, false, tick)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(
            receipt.status(),
            "place ask order failed for {base_token} amount {amount} at tick {tick}"
        );
        Ok(())
    }

    /// Quote a StablecoinDEX swap without executing it.
    pub(crate) async fn quote_dex_swap_exact_amount_in(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in: u128,
    ) -> eyre::Result<u128> {
        let provider = self.provider();
        let dex = TestStablecoinDEX::new(STABLECOIN_DEX_ADDRESS, &provider);
        Ok(dex
            .quoteSwapExactAmountIn(token_in, token_out, amount_in)
            .call()
            .await?)
    }

    /// Verify and return the native ZoneFactory at TIP-1091's fixed address.
    pub(crate) async fn native_zone_factory(&self) -> eyre::Result<Address> {
        zone_node::dev::native_zone_factory(
            self.http_url.as_str(),
            alloy_network::EthereumWallet::from(self.dev_signer()),
        )
        .await
    }

    /// Create a zone on an existing ZoneFactory and return the portal address.
    ///
    /// Captures the current L1 header as the genesis anchor, then calls
    /// `createZone()` with pathUSD as the token, a distinct [`admin_address`] as
    /// the portal admin, and the dev account as the sequencer. This exercises the
    /// admin/sequencer role separation. The admin account is funded with pathUSD
    /// for gas so admin-only portal calls (e.g. `enableToken`) can be made.
    ///
    /// [`admin_address`]: Self::admin_address
    pub(crate) async fn create_zone(&self, factory_address: Address) -> eyre::Result<Address> {
        let config = ZoneCreationConfig::closed(vec![
            self.admin_address(),
            self.dev_address(),
            self.user_signer().address(),
        ]);
        let portal = self
            .create_zone_with_admin_sequencer_and_config(
                factory_address,
                self.admin_address(),
                self.dev_address(),
                config,
            )
            .await?;
        // The admin is not pre-funded; give it pathUSD to pay for gas on
        // admin-only portal calls.
        self.fund_user(self.admin_address(), 10_000_000).await?;
        Ok(portal)
    }

    /// Create a zone with an exact access-mode, membership, and gateway configuration.
    pub(crate) async fn create_zone_with_admin_sequencer_and_config(
        &self,
        factory_address: Address,
        admin: Address,
        sequencer: Address,
        config: ZoneCreationConfig,
    ) -> eyre::Result<Address> {
        let l1_provider = self.dev_provider();
        let create_zone = ZoneFactory::createZoneCall {
            params: ZoneFactory::CreateZoneParams {
                admin,
                initialToken: PATH_USD_ADDRESS,
                accessMode: config.access_mode,
                gatewayMode: config.gateway_mode,
                allowedAccounts: config.allowed_accounts,
                zoneGateways: config.zone_gateways,
                sequencers: vec![sequencer],
                threshold: 1,
                rpcUrl: String::new(),
            },
        };
        let receipt = l1_provider
            .send_transaction(
                TransactionRequest::default()
                    .to(factory_address)
                    .input(create_zone.abi_encode().into()),
            )
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createZone failed");

        let zone_created = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ZoneFactory::ZoneCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre::eyre!("ZoneCreated event not found"))?;

        Ok(zone_created.portal)
    }

    /// Deploy the SwapAndDepositRouter contract on L1 from the Foundry artifact.
    ///
    /// The constructor takes `(address stablecoinDEX, address zoneFactory)`.
    /// We pass `Address::ZERO` for the DEX since both zones use the same token.
    pub(crate) async fn deploy_router(&self, factory_address: Address) -> eyre::Result<Address> {
        self.deploy_router_with_dex(factory_address, Address::ZERO)
            .await
    }

    /// Deploy the SwapAndDepositRouter with a specific DEX address.
    ///
    /// Use this when the test requires actual token swaps via the StablecoinDEX.
    pub(crate) async fn deploy_router_with_dex(
        &self,
        factory_address: Address,
        dex_address: Address,
    ) -> eyre::Result<Address> {
        let l1_provider = self.dev_provider();

        // Constructor: constructor(address _stablecoinDEX, address _zoneFactory)
        let mut deploy_bytes = forge_bytecode("SwapAndDepositRouter")?.to_vec();
        deploy_bytes.extend_from_slice(&(dex_address, factory_address).abi_encode());
        let bytecode = Bytes::from(deploy_bytes);

        let mut deploy_tx = TransactionRequest::default().input(bytecode.into());
        deploy_tx.to = Some(TxKind::Create);
        let receipt = l1_provider
            .send_transaction(deploy_tx)
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "SwapAndDepositRouter deployment failed");

        receipt
            .contract_address
            .ok_or_else(|| eyre::eyre!("SwapAndDepositRouter deployment missing contract address"))
    }

    /// Deploy two open zones for cross-zone routing, with separate sequencers.
    pub(crate) async fn deploy_two_open_zones_with_sequencers(
        &self,
        sequencer_a: alloy_signer_local::PrivateKeySigner,
        sequencer_b: alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<(Address, Address, Address)> {
        let factory = self.native_zone_factory().await?;
        let portal_a = self
            .create_zone_with_admin_sequencer_and_config(
                factory,
                self.dev_address(),
                sequencer_a.address(),
                ZoneCreationConfig::open(),
            )
            .await?;
        let portal_b = self
            .create_zone_with_admin_sequencer_and_config(
                factory,
                self.dev_address(),
                sequencer_b.address(),
                ZoneCreationConfig::open(),
            )
            .await?;
        let router = self.deploy_router(factory).await?;

        Ok((portal_a, portal_b, router))
    }

    /// Create a new TIP-20 token on L1 via the factory precompile.
    ///
    /// Returns the new token's address.
    pub(crate) async fn create_tip20(
        &self,
        name: &str,
        symbol: &str,
        salt: B256,
    ) -> eyre::Result<Address> {
        let provider = self.dev_provider();
        let factory = ITIP20Factory::new(TIP20_FACTORY_ADDRESS, &provider);
        let receipt = factory
            .createToken_0(
                name.to_string(),
                symbol.to_string(),
                "USD".to_string(),
                PATH_USD_ADDRESS,
                self.dev_address(),
                salt,
            )
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createToken failed");

        let event = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ITIP20Factory::TokenCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre::eyre!("TokenCreated event not found"))?;

        Ok(event.token)
    }

    /// Enable a token on a ZonePortal (must be called by the admin).
    pub(crate) async fn enable_token_on_portal(
        &self,
        portal_address: Address,
        token: Address,
    ) -> eyre::Result<()> {
        let provider = self.admin_provider();
        let portal = ZonePortal::new(portal_address, &provider);
        let receipt = portal
            .enableToken(token)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "enableToken failed");
        Ok(())
    }

    /// Update a portal callback gateway with the default distinct admin signer.
    pub(crate) async fn set_zone_gateway_on_portal(
        &self,
        portal_address: Address,
        gateway: Address,
        enabled: bool,
    ) -> eyre::Result<u64> {
        self.set_zone_gateway_on_portal_with_signer(
            portal_address,
            gateway,
            enabled,
            self.admin_signer(),
        )
        .await
    }

    /// Update account allowlist enforcement with the default portal admin.
    pub(crate) async fn set_access_mode_on_portal(
        &self,
        portal_address: Address,
        mode: bool,
    ) -> eyre::Result<u64> {
        let provider = self.admin_provider();
        let portal = ZonePortal::new(portal_address, &provider);
        let receipt = portal
            .setAccessMode(mode)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "setAccessMode failed");
        eyre::ensure!(
            portal.isAccessEnforced().call().await? == mode,
            "L1 ZonePortal access mode did not update"
        );
        Ok(provider.get_block_number().await?)
    }

    /// Update callback gateway registration enforcement with the default portal admin.
    pub(crate) async fn set_gateway_mode_on_portal(
        &self,
        portal_address: Address,
        mode: bool,
    ) -> eyre::Result<u64> {
        let provider = self.admin_provider();
        let portal = ZonePortal::new(portal_address, &provider);
        let receipt = portal
            .setGatewayMode(mode)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "setGatewayMode failed");
        eyre::ensure!(
            portal.isGatewayOpen().call().await? != mode,
            "L1 ZonePortal gateway mode did not update"
        );
        Ok(provider.get_block_number().await?)
    }

    /// Update a portal callback gateway with the signer that owns that portal's admin role.
    pub(crate) async fn set_zone_gateway_on_portal_with_signer(
        &self,
        portal_address: Address,
        gateway: Address,
        enabled: bool,
        admin_signer: alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<u64> {
        let provider = self.provider_with_signer(admin_signer);
        let portal = ZonePortal::new(portal_address, &provider);
        let role = if enabled {
            PortalRole::CallbackGateway
        } else {
            PortalRole::None
        };
        let receipt = portal
            .setRole(gateway, role)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "setRole for gateway failed");
        eyre::ensure!(
            portal.role(gateway).call().await? as u8 == role as u8,
            "L1 ZonePortal gateway role for {gateway} did not equal {role:?}"
        );
        Ok(provider.get_block_number().await?)
    }

    /// Update closed-mode account membership with the default distinct admin signer.
    pub(crate) async fn set_allowed_account_on_portal(
        &self,
        portal_address: Address,
        account: Address,
        enabled: bool,
    ) -> eyre::Result<u64> {
        self.set_allowed_account_on_portal_with_signer(
            portal_address,
            account,
            enabled,
            self.admin_signer(),
        )
        .await
    }

    /// Update closed-mode account membership with an explicit portal admin signer.
    pub(crate) async fn set_allowed_account_on_portal_with_signer(
        &self,
        portal_address: Address,
        account: Address,
        enabled: bool,
        admin_signer: alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<u64> {
        let provider = self.provider_with_signer(admin_signer);
        let portal = ZonePortal::new(portal_address, &provider);
        let role = if enabled {
            PortalRole::Account
        } else {
            PortalRole::None
        };
        let receipt = portal
            .setRole(account, role)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "setRole for account failed");
        eyre::ensure!(
            portal.role(account).call().await? as u8 == role as u8,
            "L1 ZonePortal account role for {account} did not equal {role:?}"
        );
        Ok(provider.get_block_number().await?)
    }

    /// Pause deposits for a token on the ZonePortal.
    pub(crate) async fn pause_deposits_on_portal(
        &self,
        portal_address: Address,
        token: Address,
    ) -> eyre::Result<()> {
        let provider = self.admin_provider();
        let portal = TestZonePortalAdmin::new(portal_address, &provider);
        let receipt = portal
            .pauseDeposits(token)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "pauseDeposits failed");
        eyre::ensure!(
            !portal.areDepositsActive(token).call().await?,
            "deposits should be paused for {token}"
        );
        Ok(())
    }

    /// Set the sequencer encryption key on the ZonePortal.
    ///
    /// The sequencer must sign a proof-of-possession with the encryption key's
    /// private key. The POP message is `keccak256(abi.encode(portalAddress, x, yParity))`.
    pub(crate) async fn set_sequencer_encryption_key(
        &self,
        portal_address: Address,
        encryption_key: &k256::SecretKey,
    ) -> eyre::Result<()> {
        self.set_sequencer_encryption_key_with_signer(
            portal_address,
            encryption_key,
            self.dev_signer(),
        )
        .await
    }

    /// Set the sequencer encryption key using an explicit portal sequencer signer.
    pub(crate) async fn set_sequencer_encryption_key_with_signer(
        &self,
        portal_address: Address,
        encryption_key: &k256::SecretKey,
        sequencer_signer: alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<()> {
        // Derive public key coordinates
        let scalar: Scalar = *encryption_key.to_nonzero_scalar();
        let pub_point = AffinePoint::from(ProjectivePoint::GENERATOR * scalar);
        let encoded = pub_point.to_encoded_point(true);
        let x = B256::from_slice(encoded.x().unwrap().as_slice());
        let y_parity: u8 = encoded.as_bytes()[0]; // 0x02 or 0x03

        // Build POP message matching Solidity: keccak256(abi.encode(address(this), x, yParity))
        // yParity is uint8 in Solidity, which abi.encode pads to 32 bytes — use U256
        let message = keccak256((portal_address, x, U256::from(y_parity)).abi_encode());

        // Sign with the encryption key (not the sequencer's Ethereum key)
        let enc_key_bytes = B256::from_slice(&encryption_key.to_bytes());
        let pop_signer = alloy_signer_local::PrivateKeySigner::from_bytes(&enc_key_bytes)?;
        let sig = pop_signer.sign_hash_sync(&message)?;

        // ecrecover expects v = 27 or 28
        let pop_v = sig.v() as u8 + 27;
        let pop_r = B256::from(sig.r().to_be_bytes::<32>());
        let pop_s = B256::from(sig.s().to_be_bytes::<32>());

        let sequencer_provider = ProviderBuilder::new()
            .wallet(sequencer_signer)
            .connect_http(self.http_url.clone());
        let portal = ZonePortal::new(portal_address, &sequencer_provider);
        let receipt = portal
            .setSequencerEncryptionKey(x, y_parity, pop_v, pop_r, pop_s)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "setSequencerEncryptionKey failed");
        Ok(())
    }

    /// Build a valid encrypted deposit payload for the current portal key.
    pub(crate) async fn encrypt_deposit_for_portal(
        &self,
        portal_address: Address,
        recipient: Address,
        memo: B256,
    ) -> eyre::Result<(U256, tempo_zone_contracts::EncryptedDepositPayload)> {
        let portal = ZonePortal::new(portal_address, self.provider());
        let key_result = portal.sequencerEncryptionKey().call().await?;
        let key_count = portal.encryptionKeyCount().call().await?;
        eyre::ensure!(
            key_count > U256::ZERO,
            "no encryption key registered on portal"
        );
        let key_index = key_count - U256::from(1);

        let enc = ecies::encrypt_deposit(
            &key_result.x,
            key_result.yParity,
            recipient,
            memo,
            portal_address,
            key_index,
        )
        .ok_or_else(|| eyre::eyre!("ECIES encryption failed"))?;

        Ok((
            key_index,
            tempo_zone_contracts::EncryptedDepositPayload {
                ephemeralPubkeyX: enc.eph_pub_x,
                ephemeralPubkeyYParity: enc.eph_pub_y_parity,
                ciphertext: enc.ciphertext.into(),
                nonce: alloy_primitives::FixedBytes(enc.nonce),
                tag: alloy_primitives::FixedBytes(enc.tag),
            },
        ))
    }

    /// Transfer a specific TIP-20 token from the dev account to a recipient on L1.
    pub(crate) async fn fund_user_token(
        &self,
        token: Address,
        to: Address,
        amount: u128,
    ) -> eyre::Result<()> {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(EthereumWallet::from(self.dev_signer()))
            .connect_http(self.http_url.clone());
        let receipt = ITIP20::new(token, &provider)
            .transfer(to, U256::from(amount))
            // A transfer call would otherwise infer `token` as its L1 fee token. Newly created
            // test tokens intentionally have no FeeAMM pool, so pay gas explicitly in pathUSD.
            .fee_token(PATH_USD_ADDRESS)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "fund_user_token transfer failed");
        Ok(())
    }

    /// Mint tokens on L1.
    ///
    /// The dev account must be the admin of the token (set during `createToken`).
    /// Grants `ISSUER_ROLE` to self first (admin can grant roles), then mints.
    pub(crate) async fn mint_tip20(
        &self,
        token: Address,
        to: Address,
        amount: u128,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();

        // Admin can grant ISSUER_ROLE to self
        let receipt = IRolesAuth::new(token, &provider)
            .grantRole(*ISSUER_ROLE, self.dev_address())
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "grantRole ISSUER failed on L1");

        let receipt = ITIP20::new(token, &provider)
            .mint(to, U256::from(amount))
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "mint_tip20 failed");
        Ok(())
    }

    /// Create a new BLACKLIST policy on L1. Returns the policy ID.
    pub(crate) async fn create_blacklist_policy(&self) -> eyre::Result<u64> {
        let provider = self.dev_provider();
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
        let receipt = registry
            .createPolicy(self.dev_address(), ITIP403Registry::PolicyType::BLACKLIST)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createPolicy (BLACKLIST) failed");

        let event = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ITIP403Registry::PolicyCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre::eyre!("PolicyCreated event not found"))?;

        Ok(event.policyId)
    }

    /// Create a new WHITELIST policy on L1. Returns the policy ID.
    #[allow(dead_code)]
    pub(crate) async fn create_whitelist_policy(&self) -> eyre::Result<u64> {
        let provider = self.dev_provider();
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
        let receipt = registry
            .createPolicy(self.dev_address(), ITIP403Registry::PolicyType::WHITELIST)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createPolicy (WHITELIST) failed");

        let event = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ITIP403Registry::PolicyCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre::eyre!("PolicyCreated event not found"))?;

        Ok(event.policyId)
    }

    /// Add an address to a blacklist policy.
    pub(crate) async fn blacklist_address(
        &self,
        policy_id: u64,
        account: Address,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
        let receipt = registry
            .modifyPolicyBlacklist(policy_id, account, true)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "modifyPolicyBlacklist failed");
        Ok(())
    }

    /// Add an address to a whitelist policy.
    #[allow(dead_code)]
    pub(crate) async fn whitelist_address(
        &self,
        policy_id: u64,
        account: Address,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
        let receipt = registry
            .modifyPolicyWhitelist(policy_id, account, true)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "modifyPolicyWhitelist failed");
        Ok(())
    }

    /// Change a token's transfer policy on L1.
    ///
    /// The dev account must hold `DEFAULT_ADMIN_ROLE` on the token.
    pub(crate) async fn change_transfer_policy_id(
        &self,
        token: Address,
        policy_id: u64,
    ) -> eyre::Result<()> {
        let provider = self.dev_provider();
        let receipt = ITIP20::new(token, &provider)
            .changeTransferPolicyId(policy_id)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "changeTransferPolicyId failed");
        Ok(())
    }

    /// Create a COMPOUND policy on L1 that delegates to sub-policies by role.
    ///
    /// Returns the compound policy ID.
    pub(crate) async fn create_compound_policy(
        &self,
        sender_policy_id: u64,
        recipient_policy_id: u64,
        mint_recipient_policy_id: u64,
    ) -> eyre::Result<u64> {
        let provider = self.dev_provider();
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
        let receipt = registry
            .createCompoundPolicy(
                sender_policy_id,
                recipient_policy_id,
                mint_recipient_policy_id,
            )
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "createCompoundPolicy failed");

        let event = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ITIP403Registry::CompoundPolicyCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre::eyre!("CompoundPolicyCreated event not found"))?;

        Ok(event.policyId)
    }

    /// Check if a user is authorized under a policy on L1.
    pub(crate) async fn is_authorized(&self, policy_id: u64, user: Address) -> eyre::Result<bool> {
        let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, self.provider());
        Ok(registry.isAuthorized(policy_id, user).call().await?)
    }

    /// Start an L1 dev node with the default configuration (500ms block time).
    pub(crate) async fn start() -> eyre::Result<Self> {
        Self::start_with(|_| {}).await
    }

    /// Start an L1 dev node, applying a closure to customise the [`NodeConfig`]
    /// before launch.
    ///
    /// The base config already has dev mode enabled, random ports, and full
    /// HTTP + WS RPC. The closure receives a `&mut NodeConfig` for last-mile
    /// tweaks (e.g. changing block time):
    ///
    /// ```ignore
    /// let l1 = L1TestNode::start_with(|cfg| {
    ///     cfg.dev.block_time = Some(Duration::from_secs(1));
    /// }).await?;
    /// ```
    pub(crate) async fn start_with(
        f: impl FnOnce(&mut NodeConfig<TempoChainSpec>),
    ) -> eyre::Result<Self> {
        let tasks = Runtime::test();

        let genesis: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/test-genesis.json"))?;
        let mut genesis = serde_json::from_value(genesis)?;
        install_native_zone_factory(&mut genesis, l1_dev_signer().address())?;
        let chain_spec = TempoChainSpec::from_genesis(genesis);

        let mut node_config = NodeConfig::new(Arc::new(chain_spec))
            .with_unused_ports()
            .dev()
            .with_rpc(
                RpcServerArgs::default()
                    .with_unused_ports()
                    .with_http()
                    .with_http_api(RpcModuleSelection::All)
                    .with_ws()
                    .with_ws_api(RpcModuleSelection::All),
            )
            .apply(|mut c| {
                c.dev.block_time = Some(Duration::from_millis(500));
                c.dev.finality_depth = std::num::NonZeroUsize::MIN;
                c
            });

        f(&mut node_config);

        let node_handle = NodeBuilder::new(node_config)
            .testing_node(tasks.clone())
            .node(tempo_node::node::TempoNode::default())
            .launch_with_debug_capabilities()
            .await?;

        let http_url = node_handle
            .node
            .rpc_server_handle()
            .http_url()
            .unwrap()
            .parse()
            .unwrap();
        let ws_url = node_handle
            .node
            .rpc_server_handle()
            .ws_url()
            .unwrap()
            .parse()
            .unwrap();

        Ok(Self {
            http_url,
            ws_url,
            _node_handle: Box::new(node_handle),
            _tasks: tasks,
        })
    }
}

/// Build a zone test genesis anchored to a real L1 block.
///
/// Delegates to [`zone_node::genesis::l1_anchored_genesis`] with the latest L1 header.
///
/// Returns `(genesis, genesis_block_number)`.
pub(crate) async fn build_l1_anchored_genesis(
    l1_http_url: &url::Url,
    portal_address: Address,
) -> eyre::Result<(Genesis, u64)> {
    let l1_provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_http(l1_http_url.clone());

    let block = l1_provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await?
        .ok_or_else(|| eyre::eyre!("L1 latest block not found"))?;
    let l1_header: &TempoHeader = block.header.as_ref();
    let default_fee_token = if portal_address.is_zero() {
        PATH_USD_ADDRESS
    } else {
        ZonePortal::new(portal_address, &l1_provider)
            .enabledTokenAt(U256::ZERO)
            .call()
            .await?
    };
    zone_node::genesis::l1_anchored_genesis(l1_header, portal_address, default_fee_token)
}

/// Build a zone test genesis anchored to a specific L1 block number.
pub(crate) async fn build_l1_anchored_genesis_at_block(
    l1_http_url: &url::Url,
    portal_address: Address,
    block_number: u64,
) -> eyre::Result<(Genesis, u64)> {
    let l1_provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_http(l1_http_url.clone());

    let block = l1_provider
        .get_block_by_number(block_number.into())
        .await?
        .ok_or_else(|| eyre::eyre!("L1 block {block_number} not found"))?;
    let l1_header: &TempoHeader = block.header.as_ref();
    let default_fee_token = if portal_address.is_zero() {
        PATH_USD_ADDRESS
    } else {
        ZonePortal::new(portal_address, &l1_provider)
            .enabledTokenAt(U256::ZERO)
            .call()
            .await?
    };
    zone_node::genesis::l1_anchored_genesis(l1_header, portal_address, default_fee_token)
}
