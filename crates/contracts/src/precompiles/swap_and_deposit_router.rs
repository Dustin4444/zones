//! `SwapAndDepositRouter` — deployed on Tempo L1.

use crate::DepositPayload;
use alloc::vec::Vec;
use alloy_primitives::{Address, B256, FixedBytes, U256, keccak256};
use alloy_sol_types::SolValue;

crate::sol! {
    #[derive(Debug)]
    contract SwapAndDepositRouter {
        function onWithdrawalReceived(
            uint32 sourceZoneId,
            address sourcePortal,
            bytes32 senderTag,
            address tokenIn,
            uint128 amount,
            bytes calldata data
        ) external returns (bytes4);
    }
}

/// Callback payload for `SwapAndDepositRouter.onWithdrawalReceived`.
///
/// This payload tells the router to optionally swap the withdrawn token on L1 and then call
/// `ZonePortal.deposit(...)` with an ECIES-encrypted `(recipient, memo)` payload. The router
/// consumes a nullifier derived from the authenticated source portal and sender tag and enforces
/// a withdrawal-derived GCM nonce so copied ciphertext cannot authenticate for another withdrawal.
#[derive(Debug, Clone)]
pub struct SwapAndDepositRouterCallback {
    /// Token that should be deposited after the optional L1 swap.
    pub token_out: Address,
    /// Target zone portal that receives the downstream deposit.
    pub target_portal: Address,
    /// Portal encryption key index used to build [`Self::encrypted`].
    pub key_index: U256,
    /// ECIES-encrypted `(recipient, memo)` payload for `deposit`.
    pub encrypted: DepositPayload,
    /// Tempo refund recipient if the downstream deposit later bounces.
    pub tempo_refund_recipient: Address,
    /// Minimum acceptable output from the optional swap.
    ///
    /// Ignored when `tokenIn == token_out` and the router can deposit directly.
    pub min_amount_out: u128,
}

/// Derive the 96-bit GCM nonce from the router-scoped identity of an authenticated source
/// withdrawal.
///
/// The full identity is `keccak256(abi.encode(router, source_portal, sender_tag))`; the router
/// uses all 32 bytes as its replay nullifier, while the client uses the first 12 bytes as the
/// routed deposit's GCM nonce. The sender tag is already domain-separated, and the source portal
/// prevents cross-Zone reuse.
pub fn routed_deposit_nonce(
    router: Address,
    source_portal: Address,
    sender_tag: B256,
) -> FixedBytes<12> {
    let withdrawal_id = keccak256((router, source_portal, sender_tag).abi_encode());
    FixedBytes::from_slice(&withdrawal_id[..12])
}

impl SwapAndDepositRouterCallback {
    /// ABI-encode the router callback data expected by the Solidity router.
    pub fn abi_encode(&self) -> Vec<u8> {
        (
            self.token_out,
            self.target_portal,
            self.key_index,
            self.encrypted.clone(),
            self.tempo_refund_recipient,
            self.min_amount_out,
        )
            .abi_encode_params()
    }
}
