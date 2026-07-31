//! Zone account helpers: funded accounts, deposits, withdrawals, router callbacks.

use super::*;

use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_signer_local::{MnemonicBuilder, coins_bip39::English};
use std::{collections::BTreeSet, time::Duration};
use tempo_alloy::TempoNetwork;
use tempo_chainspec::spec::TEMPO_T0_BASE_FEE;
use tempo_contracts::precompiles::ITIP20;
use tempo_precompiles::PATH_USD_ADDRESS;
use tempo_zone_contracts::{
    EncryptedDepositPayload, IZoneOutbox, SwapAndDepositRouterPlaintextCallback,
    ZONE_OUTBOX_ADDRESS, ZONE_TOKEN_ADDRESS, ZonePortal,
};
use zone_precompiles::ecies;

pub(crate) fn local_dev_zone_account(zone: &ZoneTestNode) -> eyre::Result<(DynProvider, Address)> {
    let dev_signer = MnemonicBuilder::<English>::default()
        .phrase(TEST_MNEMONIC)
        .build()?;
    let dev_address = dev_signer.address();
    let provider = ProviderBuilder::new()
        .wallet(dev_signer)
        .connect_http(zone.http_url().clone())
        .erased();
    Ok((provider, dev_address))
}

pub(crate) fn local_dev_tempo_zone_account(
    zone: &ZoneTestNode,
) -> eyre::Result<(DynProvider<TempoNetwork>, Address)> {
    let dev_signer = MnemonicBuilder::<English>::default()
        .phrase(TEST_MNEMONIC)
        .build()?;
    let dev_address = dev_signer.address();
    let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
        .wallet(EthereumWallet::from(dev_signer))
        .connect_http(zone.http_url().clone())
        .erased();
    Ok((provider, dev_address))
}

pub(crate) async fn approve_outbox<P>(
    fixture: &mut L1Fixture,
    zone: &ZoneTestNode,
    provider: P,
) -> eyre::Result<()>
where
    P: Provider + Clone,
{
    let zone_token = ITIP20::new(PATH_USD_ADDRESS, provider);
    let approve_pending = zone_token
        .approve(ZONE_OUTBOX_ADDRESS, U256::MAX)
        .gas_price(TEMPO_T0_BASE_FEE as u128)
        .gas(TIP20_TX_GAS)
        .send()
        .await?;
    fixture.inject_empty_block(zone.deposit_queue());
    let approve_receipt = approve_pending.get_receipt().await?;
    assert!(approve_receipt.status(), "approve should succeed");
    Ok(())
}

/// Arguments for [`ZoneAccount::withdraw_with`].
///
/// Use [`WithdrawalArgs::new`] for the common case (amount only, self-withdrawal),
/// then override individual fields as needed.
#[derive(Clone)]
pub(crate) struct WithdrawalArgs {
    pub amount: u128,
    pub to: Option<Address>,
    pub memo: B256,
    pub gas_limit: u64,
    pub zone_fallback_recipient: Option<Address>,
    pub data: alloy_primitives::Bytes,
    pub reveal_to: alloy_primitives::Bytes,
}

pub(crate) struct PlaintextRouterCallbackArgs {
    pub amount: u128,
    pub router: Address,
    pub token_out: Address,
    pub target_portal: Address,
    pub recipient: Address,
    pub tempo_refund_recipient: Address,
    pub memo: B256,
    pub min_amount_out: u128,
}

pub(crate) struct EncryptedRouterCallbackArgs {
    pub amount: u128,
    pub router: Address,
    pub token_out: Address,
    pub target_portal: Address,
    pub key_index: U256,
    pub encrypted: tempo_zone_contracts::EncryptedDepositPayload,
    pub tempo_refund_recipient: Address,
    pub min_amount_out: u128,
}

impl WithdrawalArgs {
    /// Simple withdrawal: send `amount` back to self with no callback.
    pub(crate) fn new(amount: u128) -> Self {
        Self {
            amount,
            to: None,
            memo: B256::ZERO,
            gas_limit: 0,
            zone_fallback_recipient: None,
            data: alloy_primitives::Bytes::new(),
            reveal_to: alloy_primitives::Bytes::new(),
        }
    }

    /// Plaintext router callback: optionally swap, then deposit into `target_portal`.
    pub(crate) fn swap_and_deposit_via_router(args: PlaintextRouterCallbackArgs) -> Self {
        let callback_data = SwapAndDepositRouterPlaintextCallback {
            token_out: args.token_out,
            target_portal: args.target_portal,
            recipient: args.recipient,
            tempo_refund_recipient: args.tempo_refund_recipient,
            memo: args.memo,
            min_amount_out: args.min_amount_out,
        }
        .abi_encode();

        Self {
            amount: args.amount,
            to: Some(args.router),
            memo: args.memo,
            gas_limit: 2_000_000,
            zone_fallback_recipient: None, // defaults to self
            data: alloy_primitives::Bytes::from(callback_data),
            reveal_to: alloy_primitives::Bytes::new(),
        }
    }

    /// Encrypted router callback: optionally swap, then deposit encrypted into `target_portal`.
    pub(crate) fn swap_and_deposit_encrypted_via_router(args: EncryptedRouterCallbackArgs) -> Self {
        let callback_data = tempo_zone_contracts::SwapAndDepositRouterEncryptedCallback {
            token_out: args.token_out,
            target_portal: args.target_portal,
            key_index: args.key_index,
            encrypted: args.encrypted,
            tempo_refund_recipient: args.tempo_refund_recipient,
            min_amount_out: args.min_amount_out,
        }
        .abi_encode();

        Self {
            amount: args.amount,
            to: Some(args.router),
            memo: B256::ZERO,
            gas_limit: 2_000_000,
            zone_fallback_recipient: None, // defaults to self
            data: alloy_primitives::Bytes::from(callback_data),
            reveal_to: alloy_primitives::Bytes::new(),
        }
    }

    /// Cross-zone withdrawal via the [`SwapAndDepositRouter`].
    ///
    /// The withdrawal callback sends tokens to the router, which deposits them
    /// into `target_portal` for `recipient`. Both zones must use the same token
    /// (no swap needed — `tokenOut == tokenIn`).
    pub(crate) fn cross_zone_via_router(
        amount: u128,
        router: Address,
        target_portal: Address,
        token: Address,
        recipient: Address,
        tempo_refund_recipient: Address,
    ) -> Self {
        Self::swap_and_deposit_via_router(PlaintextRouterCallbackArgs {
            amount,
            router,
            token_out: token,
            target_portal,
            recipient,
            tempo_refund_recipient,
            memo: B256::ZERO,
            min_amount_out: 0,
        })
    }
}

/// A test account that can interact with both L1 and L2 (zone) nodes.
///
/// Wraps a signing key and provides high-level helpers for the common
/// deposit/withdrawal flow, tracking approvals to avoid redundant transactions.
pub(crate) struct ZoneAccount {
    /// The account's on-chain address (derived from `signer`).
    address: Address,
    /// Wallet-attached provider for Tempo L1 (deposits, approvals).
    l1_provider: alloy_provider::DynProvider,
    /// Wallet-attached provider for the Zone L2 (withdrawals, approvals).
    l2_provider: alloy_provider::DynProvider,
    /// The ZonePortal contract address on L1 for this zone.
    portal_address: Address,
    /// Whether we've already approved the portal to spend pathUSD on L1.
    l1_portal_approved: bool,
    /// Tokens already approved for the ZoneOutbox on L2.
    l2_outbox_approved_tokens: BTreeSet<Address>,
}

impl ZoneAccount {
    /// Create a new `ZoneAccount` from an [`L1TestNode`] and [`ZoneTestNode`].
    ///
    /// Uses the L1's **user** signer (mnemonic index 1) as the account key,
    /// separate from the dev/sequencer account (index 0). The same key signs
    /// both L1 and L2 transactions.
    ///
    /// The user account must be funded on L1 before depositing — call
    /// [`L1TestNode::fund_user`] first.
    pub(crate) fn from_l1_and_zone(
        l1: &L1TestNode,
        zone: &ZoneTestNode,
        portal_address: Address,
    ) -> Self {
        let signer = l1.user_signer();
        let address = signer.address();

        let l1_provider = ProviderBuilder::new()
            .wallet(signer.clone())
            .connect_http(l1.http_url().clone())
            .erased();

        let l2_provider = ProviderBuilder::new()
            .wallet(signer)
            .connect_http(zone.http_url().clone())
            .erased();

        Self {
            address,
            l1_provider,
            l2_provider,
            portal_address,
            l1_portal_approved: false,
            l2_outbox_approved_tokens: BTreeSet::new(),
        }
    }

    /// Create a `ZoneAccount` with a custom signer.
    ///
    /// Unlike [`from_l1_and_zone`](Self::from_l1_and_zone) which uses the L1's
    /// user signer, this allows creating an account with any private key —
    /// useful when the account was funded via encrypted deposit to a specific
    /// recipient.
    pub(crate) fn with_signer(
        signer: alloy_signer_local::PrivateKeySigner,
        l1: &L1TestNode,
        zone: &ZoneTestNode,
        portal_address: Address,
    ) -> Self {
        let address = signer.address();

        let l1_provider = ProviderBuilder::new()
            .wallet(signer.clone())
            .connect_http(l1.http_url().clone())
            .erased();

        let l2_provider = ProviderBuilder::new()
            .wallet(signer)
            .connect_http(zone.http_url().clone())
            .erased();

        Self {
            address,
            l1_provider,
            l2_provider,
            portal_address,
            l1_portal_approved: false,
            l2_outbox_approved_tokens: BTreeSet::new(),
        }
    }

    /// The account's address.
    pub(crate) fn address(&self) -> Address {
        self.address
    }

    /// The account's L1 provider.
    pub(crate) fn l1_provider(&self) -> &alloy_provider::DynProvider {
        &self.l1_provider
    }

    /// Approve the ZonePortal to spend pathUSD on L1, then deposit.
    ///
    /// Skips approval if already approved in this session.
    /// Waits for the expected post-deposit balance on L2 and returns it.
    pub(crate) async fn deposit(
        &mut self,
        amount: u128,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<U256> {
        self.deposit_to(self.address, amount, timeout, zone).await
    }

    /// Simulate a plaintext deposit without submitting a transaction.
    pub(crate) async fn simulate_deposit(
        &self,
        amount: u128,
        recipient: Address,
        tempo_refund_recipient: Address,
    ) -> eyre::Result<()> {
        ZonePortal::new(self.portal_address, &self.l1_provider)
            .deposit(
                PATH_USD_ADDRESS,
                recipient,
                amount,
                B256::ZERO,
                tempo_refund_recipient,
            )
            .call()
            .await?;
        Ok(())
    }

    /// Approve the portal for `token` and submit a raw `deposit` transaction,
    /// returning the L1 inclusion block WITHOUT waiting for anything on the
    /// zone.
    ///
    /// Negative-path tests use this when the deposit is expected to bounce
    /// (or revert — the revert surfaces as this method's error).
    #[allow(dead_code)] // adopted by the e2e suites in a follow-up
    pub(crate) async fn deposit_raw(
        &mut self,
        token: Address,
        recipient: Address,
        amount: u128,
        tempo_refund_recipient: Address,
    ) -> eyre::Result<u64> {
        ITIP20::new(token, &self.l1_provider)
            .approve(self.portal_address, U256::MAX)
            .send()
            .await?
            .get_receipt()
            .await?;

        let receipt = ZonePortal::new(self.portal_address, &self.l1_provider)
            .deposit(token, recipient, amount, B256::ZERO, tempo_refund_recipient)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "raw deposit transaction failed on L1");
        receipt
            .block_number
            .ok_or_else(|| eyre::eyre!("deposit receipt missing block number"))
    }

    /// Approve the ZonePortal to spend pathUSD on L1, then deposit to a specific recipient.
    ///
    /// Waits for the expected post-deposit balance on L2 and returns it.
    pub(crate) async fn deposit_to(
        &mut self,
        recipient: Address,
        amount: u128,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<U256> {
        Ok(self
            .deposit_to_with_block(recipient, amount, timeout, zone)
            .await?
            .1)
    }

    /// Same as [`deposit_to`](Self::deposit_to), but also returns the L1 block number
    /// that included the deposit transaction.
    pub(crate) async fn deposit_to_with_block(
        &mut self,
        recipient: Address,
        amount: u128,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<(u64, U256)> {
        if !self.l1_portal_approved {
            ITIP20::new(PATH_USD_ADDRESS, &self.l1_provider)
                .approve(self.portal_address, U256::MAX)
                .send()
                .await?
                .get_receipt()
                .await?;
            self.l1_portal_approved = true;
        }

        // Snapshot balance before deposit so we wait for the expected increase
        let balance_before = zone.balance_of(ZONE_TOKEN_ADDRESS, recipient).await?;

        let portal = ZonePortal::new(self.portal_address, &self.l1_provider);
        let receipt = portal
            .deposit(
                PATH_USD_ADDRESS,
                recipient,
                amount,
                B256::ZERO,
                self.address,
            )
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "L1 deposit tx failed");

        let balance = zone
            .wait_for_balance(
                ZONE_TOKEN_ADDRESS,
                recipient,
                balance_before + U256::from(amount),
                timeout,
            )
            .await?;

        let block_number = receipt
            .block_number
            .ok_or_else(|| eyre::eyre!("deposit receipt missing block number"))?;

        Ok((block_number, balance))
    }

    /// Approve the ZonePortal to spend `amount` of a specific `token` on L1, then deposit.
    ///
    /// Unlike [`deposit`](Self::deposit), this allows depositing any enabled token.
    /// The caller must ensure:
    /// - The token is enabled on the portal (`enableToken`)
    /// - The account has sufficient balance of `token` on L1
    ///
    /// Waits for the expected post-deposit balance on L2 and returns it.
    pub(crate) async fn deposit_token(
        &mut self,
        token: Address,
        l2_token: Address,
        amount: u128,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<U256> {
        // Approve portal for this specific token
        ITIP20::new(token, &self.l1_provider)
            .approve(self.portal_address, U256::MAX)
            .send()
            .await?
            .get_receipt()
            .await?;

        // Snapshot balance before deposit so we wait for the expected increase
        let balance_before = zone.balance_of(l2_token, self.address).await?;

        let portal = ZonePortal::new(self.portal_address, &self.l1_provider);
        let receipt = portal
            .deposit(token, self.address, amount, B256::ZERO, self.address)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "L1 deposit tx failed");

        zone.wait_for_balance(
            l2_token,
            self.address,
            balance_before + U256::from(amount),
            timeout,
        )
        .await
    }

    /// Approve portal + call `depositEncrypted` on L1 with properly ECIES-encrypted payload.
    ///
    /// Performs ECIES encryption client-side (matching what a real depositor would do):
    /// 1. Read the sequencer's encryption key from the portal
    /// 2. Generate an ephemeral key pair
    /// 3. ECDH → HKDF → AES-256-GCM encrypt (to, memo)
    /// 4. Call `depositEncrypted` on the portal
    /// 5. Wait for the zone to mint tokens to the decrypted recipient
    pub(crate) async fn deposit_encrypted(
        &mut self,
        amount: u128,
        recipient: Address,
        memo: B256,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<U256> {
        Ok(self
            .deposit_encrypted_with_block(amount, recipient, memo, timeout, zone)
            .await?
            .1)
    }

    /// Same as [`deposit_encrypted`](Self::deposit_encrypted), but also returns the
    /// L1 block number that included the encrypted deposit transaction.
    pub(crate) async fn deposit_encrypted_with_block(
        &mut self,
        amount: u128,
        recipient: Address,
        memo: B256,
        timeout: Duration,
        zone: &ZoneTestNode,
    ) -> eyre::Result<(u64, U256)> {
        let portal_address = self.portal_address;

        // Approve portal if needed
        if !self.l1_portal_approved {
            ITIP20::new(PATH_USD_ADDRESS, &self.l1_provider)
                .approve(portal_address, U256::MAX)
                .send()
                .await?
                .get_receipt()
                .await?;
            self.l1_portal_approved = true;
        }

        // Read sequencer encryption key and its index from portal
        let portal = ZonePortal::new(portal_address, &self.l1_provider);
        let key_result = portal.sequencerEncryptionKey().call().await?;
        let key_count = portal.encryptionKeyCount().call().await?;
        eyre::ensure!(
            key_count > U256::ZERO,
            "no encryption key registered on portal"
        );
        let key_index = key_count - U256::from(1);

        // ECIES encrypt (to, memo) to sequencer's public key
        let enc = ecies::encrypt_deposit(
            &key_result.x,
            key_result.yParity,
            recipient,
            memo,
            portal_address,
            key_index,
        )
        .ok_or_else(|| eyre::eyre!("ECIES encryption failed"))?;

        // Snapshot balance before deposit
        let balance_before = zone.balance_of(ZONE_TOKEN_ADDRESS, recipient).await?;

        // Call depositEncrypted on portal
        let receipt = portal
            .depositEncrypted(
                PATH_USD_ADDRESS,
                amount,
                key_index,
                EncryptedDepositPayload {
                    ephemeralPubkeyX: enc.eph_pub_x,
                    ephemeralPubkeyYParity: enc.eph_pub_y_parity,
                    ciphertext: enc.ciphertext.into(),
                    nonce: alloy_primitives::FixedBytes(enc.nonce),
                    tag: alloy_primitives::FixedBytes(enc.tag),
                },
                self.address,
            )
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "L1 depositEncrypted tx failed");

        // Wait for the zone to process the encrypted deposit and mint to recipient
        let balance = zone
            .wait_for_balance(
                ZONE_TOKEN_ADDRESS,
                recipient,
                balance_before + U256::from(amount),
                timeout,
            )
            .await?;

        let block_number = receipt
            .block_number
            .ok_or_else(|| eyre::eyre!("depositEncrypted receipt missing block number"))?;

        Ok((block_number, balance))
    }

    /// Approve the ZoneOutbox, then request a withdrawal on L2.
    ///
    /// Skips approval if already approved in this session.
    pub(crate) async fn withdraw(&mut self, amount: u128) -> eyre::Result<()> {
        self.withdraw_with(WithdrawalArgs::new(amount)).await
    }

    /// Approve the ZoneOutbox, then request a withdrawal on L2 with custom args.
    ///
    /// Skips approval if already approved in this session.
    /// Uses the default zone token (pathUSD / `ZONE_TOKEN_ADDRESS`).
    pub(crate) async fn withdraw_with(&mut self, args: WithdrawalArgs) -> eyre::Result<()> {
        self.withdraw_token_with(ZONE_TOKEN_ADDRESS, args).await
    }

    /// Simulate a withdrawal request without submitting it to the transaction pool.
    /// Useful for asserting deterministic validation reverts.
    pub(crate) async fn simulate_withdraw_with(&self, args: WithdrawalArgs) -> eyre::Result<()> {
        let to = args.to.unwrap_or(self.address);
        let zone_fallback_recipient = args.zone_fallback_recipient.unwrap_or(self.address);
        IZoneOutbox::new(ZONE_OUTBOX_ADDRESS, &self.l2_provider)
            .requestWithdrawal(
                ZONE_TOKEN_ADDRESS,
                to,
                args.amount,
                args.memo,
                args.gas_limit,
                zone_fallback_recipient,
                args.data,
                args.reveal_to,
            )
            .from(self.address)
            .call()
            .await?;
        Ok(())
    }

    /// Approve the ZoneOutbox for a specific token, then request a withdrawal on L2.
    pub(crate) async fn withdraw_token(
        &mut self,
        token: Address,
        amount: u128,
    ) -> eyre::Result<()> {
        self.withdraw_token_with(token, WithdrawalArgs::new(amount))
            .await
    }

    /// Approve the ZoneOutbox for a specific token, then request a withdrawal on L2 with custom args.
    pub(crate) async fn withdraw_token_with(
        &mut self,
        token: Address,
        args: WithdrawalArgs,
    ) -> eyre::Result<()> {
        self.approve_outbox(token).await?;

        let to = args.to.unwrap_or(self.address);
        let zone_fallback_recipient = args.zone_fallback_recipient.unwrap_or(self.address);

        let outbox = IZoneOutbox::new(ZONE_OUTBOX_ADDRESS, &self.l2_provider);
        let receipt = outbox
            .requestWithdrawal(
                token,
                to,
                args.amount,
                args.memo,
                args.gas_limit,
                zone_fallback_recipient,
                args.data,
                args.reveal_to,
            )
            .gas(WITHDRAWAL_TX_GAS)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(
            receipt.status(),
            "L2 withdrawal request failed (gas used: {})",
            receipt.gas_used
        );

        Ok(())
    }

    /// Approve the ZoneOutbox for a token without submitting a withdrawal.
    ///
    /// Reuses a successful max approval for subsequent withdrawals of the same token.
    pub(crate) async fn approve_outbox(&mut self, token: Address) -> eyre::Result<()> {
        if self.l2_outbox_approved_tokens.contains(&token) {
            return Ok(());
        }

        let receipt = ITIP20::new(token, &self.l2_provider)
            .approve(ZONE_OUTBOX_ADDRESS, U256::MAX)
            .gas(TIP20_TX_GAS)
            .send()
            .await?
            .get_receipt()
            .await?;
        eyre::ensure!(receipt.status(), "L2 outbox approval failed");
        self.l2_outbox_approved_tokens.insert(token);
        Ok(())
    }
}
