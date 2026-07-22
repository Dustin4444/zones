//! Privacy policy enforcement helpers.
//!
//! Shared by [`ZoneRpcApi`] implementations.

use std::future::Future;

use alloy_consensus::transaction::SignerRecoverable;
use alloy_eips::eip2718::Decodable2718;
use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, Bytes, TxKind};
use alloy_sol_types::SolCall;
use tempo_alloy::rpc::TempoTransactionRequest;
use tempo_contracts::precompiles::ITIP20;
use tempo_primitives::{TempoTxEnvelope, is_tip20_prefix};
use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZoneInbox, ZoneOutbox};
use zone_primitives::constants::CONTRACT_DEPLOYER_ALLOWLIST;

use crate::{auth::AuthContext, types::JsonRpcError};

alloy_sol_types::sol! {
    interface LegacyZoneOutboxWithdrawal {
        function requestWithdrawal(
            address token,
            address to,
            uint128 amount,
            bytes32 memo,
            uint64 gasLimit,
            address fallbackRecipient,
            bytes data
        ) external;
    }
}

/// Enforce all private RPC authorization rules for simulation-style requests.
///
/// The sequencer check is lazy: it is awaited only for calls that try to read
/// another account's `ZoneInbox.refunds(token, owner)` entry.
pub async fn enforce_authorized<F>(
    request: &mut TempoTransactionRequest,
    auth: &AuthContext,
    is_sequencer: F,
) -> Result<(), JsonRpcError>
where
    F: Future<Output = Result<bool, JsonRpcError>>,
{
    enforce_from(request, auth)?;
    enforce_contract_creation(request, auth.caller)?;
    enforce_zone_inbox_refund_call_privacy(request, auth, is_sequencer).await
}

/// Enforce that `from` matches the authenticated caller.
///
/// - If `from` is omitted, sets it to `auth.caller`.
/// - If present and mismatched, returns `-32004 Account mismatch`.
pub fn enforce_from(
    request: &mut TempoTransactionRequest,
    auth: &AuthContext,
) -> Result<(), JsonRpcError> {
    match TransactionBuilder::from(request as &TempoTransactionRequest) {
        Some(from) if from != auth.caller => Err(JsonRpcError::account_mismatch()),
        None => {
            request.set_from(auth.caller);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Apply the protocol contract-deployer allowlist to create-style transaction requests.
///
/// Plain Ethereum-style create requests (`to = null`) and Tempo AA calls to `TxKind::Create`
/// are rejected with `-32602 Invalid params` unless the caller is a protocol-allowed deployer.
pub fn enforce_contract_creation(
    request: &TempoTransactionRequest,
    caller: Address,
) -> Result<(), JsonRpcError> {
    enforce_contract_creation_with_allowlist(request, caller, CONTRACT_DEPLOYER_ALLOWLIST)
}

fn enforce_contract_creation_with_allowlist(
    request: &TempoTransactionRequest,
    caller: Address,
    allowlist: &[Address],
) -> Result<(), JsonRpcError> {
    if allowlist.contains(&caller) {
        return Ok(());
    }

    let outer_create = request.inner.to.is_some_and(|to| to.is_create());
    let implicit_plain_create = request.calls.is_empty() && request.inner.to.is_none();
    let tempo_create = request.calls.iter().any(|call| call.to.is_create());
    if outer_create || implicit_plain_create || tempo_create {
        return Err(JsonRpcError::invalid_params(
            "contract creation not supported on zones",
        ));
    }

    Ok(())
}

async fn enforce_zone_inbox_refund_call_privacy<F>(
    request: &TempoTransactionRequest,
    auth: &AuthContext,
    is_sequencer: F,
) -> Result<(), JsonRpcError>
where
    F: Future<Output = Result<bool, JsonRpcError>>,
{
    if zone_inbox_refunds_mismatched_owner(request, auth.caller).is_none() {
        return Ok(());
    }

    if is_sequencer.await? {
        return Ok(());
    }

    Err(JsonRpcError::account_mismatch())
}

/// Finds a direct or nested `ZoneInbox.refunds(token, owner)` read where
/// `owner` is not the authenticated caller.
///
/// Other calls, contract creations, and malformed calldata are ignored here.
fn zone_inbox_refunds_mismatched_owner(
    request: &TempoTransactionRequest,
    caller: Address,
) -> Option<Address> {
    let refunds_owner_mismatch = |to: Option<Address>, input: Option<&Bytes>| {
        if to != Some(ZONE_INBOX_ADDRESS) {
            return None;
        }

        let input = input?;
        if !input.starts_with(&ZoneInbox::refundsCall::SELECTOR) {
            return None;
        }

        let owner = ZoneInbox::refundsCall::abi_decode(input).ok()?.owner;
        (owner != caller).then_some(owner)
    };

    if let Some(owner) = refunds_owner_mismatch(
        TransactionBuilder::to(request),
        TransactionBuilder::input(request),
    ) {
        return Some(owner);
    }

    request.calls.iter().find_map(|call| {
        let to = match call.to {
            TxKind::Call(to) => Some(to),
            TxKind::Create => None,
        };
        refunds_owner_mismatch(to, Some(&call.input))
    })
}

/// Raw transaction bytes that have passed private zone RPC authorization and call policy.
///
/// Construct with [`parse_authorized_raw_transaction`] so submission code cannot accidentally
/// forward an unchecked transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedRawTransaction(Bytes);

impl AuthorizedRawTransaction {
    /// Recover the original encoded transaction after all RPC policy checks have passed.
    pub fn into_inner(self) -> Bytes {
        self.0
    }
}

/// Decode and authorize a raw transaction for submission to a private zone.
///
/// The recovered sender must match the authenticated caller, every call in a Tempo AA batch is
/// checked, and user transactions may not directly invoke protocol-only Inbox or Outbox methods.
/// Direct TIP-20 calls are limited to `transferFrom` and `approve`; withdrawals remain available
/// through both `ZoneOutbox.requestWithdrawal` overloads.
pub fn parse_authorized_raw_transaction(
    data: Bytes,
    auth: &AuthContext,
) -> Result<AuthorizedRawTransaction, JsonRpcError> {
    let tx = TempoTxEnvelope::decode_2718_exact(&data)
        .map_err(|_| JsonRpcError::invalid_params("failed to decode transaction"))?;

    let sender = tx
        .recover_signer()
        .map_err(|_| JsonRpcError::invalid_params("invalid transaction signature"))?;

    if sender != auth.caller {
        return Err(JsonRpcError::transaction_rejected());
    }

    parse_zone_user_transaction(&tx)?;

    Ok(AuthorizedRawTransaction(data))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedZoneUserCall {
    TransferFrom,
    Approve,
    RequestWithdrawal,
    Other,
}

fn parse_zone_user_transaction(tx: &TempoTxEnvelope) -> Result<(), JsonRpcError> {
    tx.calls()
        .try_for_each(|(target, input)| parse_zone_user_call(target, input).map(drop))
}

fn parse_zone_user_call(target: TxKind, input: &Bytes) -> Result<ParsedZoneUserCall, JsonRpcError> {
    let TxKind::Call(address) = target else {
        return Ok(ParsedZoneUserCall::Other);
    };

    if address == ZONE_INBOX_ADDRESS && input.starts_with(&ZoneInbox::advanceTempoCall::SELECTOR) {
        return Err(JsonRpcError::transaction_rejected());
    }

    if address == ZONE_OUTBOX_ADDRESS {
        if input.starts_with(&LegacyZoneOutboxWithdrawal::requestWithdrawalCall::SELECTOR) {
            LegacyZoneOutboxWithdrawal::requestWithdrawalCall::abi_decode(input)
                .map_err(|_| JsonRpcError::transaction_rejected())?;
            return Ok(ParsedZoneUserCall::RequestWithdrawal);
        }

        if input.starts_with(&ZoneOutbox::requestWithdrawalCall::SELECTOR) {
            ZoneOutbox::requestWithdrawalCall::abi_decode(input)
                .map_err(|_| JsonRpcError::transaction_rejected())?;
            return Ok(ParsedZoneUserCall::RequestWithdrawal);
        }

        if input.starts_with(&ZoneOutbox::finalizeWithdrawalBatchCall::SELECTOR)
            || input.starts_with(&ZoneOutbox::enqueueDepositBounceBackCall::SELECTOR)
            || input.starts_with(&ZoneOutbox::consumeFallbackRecipientCall::SELECTOR)
        {
            return Err(JsonRpcError::transaction_rejected());
        }

        return Ok(ParsedZoneUserCall::Other);
    }

    if !is_tip20_prefix(address) {
        return Ok(ParsedZoneUserCall::Other);
    }

    if input.starts_with(&ITIP20::transferFromCall::SELECTOR) {
        ITIP20::transferFromCall::abi_decode(input)
            .map_err(|_| JsonRpcError::transaction_rejected())?;
        Ok(ParsedZoneUserCall::TransferFrom)
    } else if input.starts_with(&ITIP20::approveCall::SELECTOR) {
        ITIP20::approveCall::abi_decode(input).map_err(|_| JsonRpcError::transaction_rejected())?;
        Ok(ParsedZoneUserCall::Approve)
    } else {
        Err(JsonRpcError::transaction_rejected())
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256, address};
    use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::SolCall;
    use tempo_alloy::rpc::TempoTransactionRequest;
    use tempo_contracts::precompiles::ITIP20;
    use tempo_primitives::transaction::{
        AASigned, Call, PrimitiveSignature, TempoSignature, TempoTransaction,
    };
    use tempo_zone_contracts::{
        ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZONE_TOKEN_ADDRESS, ZoneInbox, ZoneOutbox,
    };

    use super::{
        AuthContext, LegacyZoneOutboxWithdrawal, ParsedZoneUserCall, enforce_contract_creation,
        enforce_contract_creation_with_allowlist, parse_authorized_raw_transaction,
        parse_zone_user_call, parse_zone_user_transaction, zone_inbox_refunds_mismatched_owner,
    };

    const TOKEN: Address = address!("0x20C0000000000000000000000000000000000001");

    fn call_target(byte: u8) -> TxKind {
        TxKind::Call(Address::repeat_byte(byte))
    }

    fn call_request(to: Option<TxKind>) -> TempoTransactionRequest {
        TempoTransactionRequest {
            inner: TransactionRequest {
                to,
                input: TransactionInput::new(Bytes::default()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn zone_inbox_refunds_request(owner: Address) -> TempoTransactionRequest {
        TempoTransactionRequest {
            inner: TransactionRequest {
                to: Some(TxKind::Call(ZONE_INBOX_ADDRESS)),
                input: TransactionInput::new(
                    ZoneInbox::refundsCall {
                        token: ZONE_TOKEN_ADDRESS,
                        owner,
                    }
                    .abi_encode()
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn signed_raw_call(signer: &PrivateKeySigner, target: Address, input: Bytes) -> Bytes {
        let mut tx = TxEip1559 {
            chain_id: 1,
            gas_limit: 500_000,
            max_fee_per_gas: 1,
            to: target.into(),
            input,
            ..Default::default()
        };
        let signature = signer.sign_transaction_sync(&mut tx).unwrap();
        TxEnvelope::Eip1559(tx.into_signed(signature))
            .encoded_2718()
            .into()
    }

    fn auth(caller: Address) -> AuthContext {
        AuthContext {
            caller,
            expires_at: u64::MAX,
            keychain_key_id: None,
        }
    }

    #[test]
    fn contract_creation_policy_allows_standard_call_request() {
        let request = call_request(Some(call_target(0x11)));
        assert!(enforce_contract_creation(&request, Address::repeat_byte(0x01)).is_ok());
    }

    #[test]
    fn contract_creation_policy_rejects_plain_create_request() {
        let request = call_request(None);
        let err = enforce_contract_creation(&request, Address::repeat_byte(0x01)).unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "contract creation not supported on zones");
    }

    #[test]
    fn contract_creation_policy_rejects_explicit_outer_create_request() {
        let request = call_request(Some(TxKind::Create));
        let err = enforce_contract_creation(&request, Address::repeat_byte(0x01)).unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "contract creation not supported on zones");
    }

    #[test]
    fn contract_creation_policy_allows_tempo_calls_without_outer_to() {
        let mut request = call_request(None);
        request.calls = vec![Call {
            to: call_target(0x22),
            value: U256::ZERO,
            input: Bytes::default(),
        }];

        assert!(enforce_contract_creation(&request, Address::repeat_byte(0x01)).is_ok());
    }

    #[test]
    fn contract_creation_policy_rejects_tempo_create_call() {
        let mut request = call_request(None);
        request.calls = vec![Call {
            to: TxKind::Create,
            value: U256::ZERO,
            input: Bytes::default(),
        }];

        let err = enforce_contract_creation(&request, Address::repeat_byte(0x01)).unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "contract creation not supported on zones");
    }

    #[test]
    fn contract_creation_policy_allows_designated_deployer() {
        let caller = Address::repeat_byte(0x11);
        let request = call_request(None);

        assert!(enforce_contract_creation_with_allowlist(&request, caller, &[]).is_err());
        assert!(enforce_contract_creation_with_allowlist(&request, caller, &[caller]).is_ok());
    }

    #[test]
    fn zone_inbox_refunds_mismatched_owner_detects_outer_call() {
        let caller = Address::repeat_byte(0x11);
        let owner = Address::repeat_byte(0x22);
        let request = zone_inbox_refunds_request(owner);

        assert_eq!(
            zone_inbox_refunds_mismatched_owner(&request, caller),
            Some(owner)
        );
    }

    #[test]
    fn zone_inbox_refunds_mismatched_owner_allows_own_outer_call() {
        let caller = Address::repeat_byte(0x11);
        let request = zone_inbox_refunds_request(caller);

        assert_eq!(zone_inbox_refunds_mismatched_owner(&request, caller), None);
    }

    #[test]
    fn zone_inbox_refunds_mismatched_owner_detects_nested_tempo_call() {
        let caller = Address::repeat_byte(0x11);
        let owner = Address::repeat_byte(0x22);
        let mut request = TempoTransactionRequest {
            inner: TransactionRequest {
                to: Some(TxKind::Call(Address::repeat_byte(0x33))),
                ..Default::default()
            },
            ..Default::default()
        };
        request.calls.push(Call {
            to: TxKind::Call(ZONE_INBOX_ADDRESS),
            value: U256::ZERO,
            input: ZoneInbox::refundsCall {
                token: ZONE_TOKEN_ADDRESS,
                owner,
            }
            .abi_encode()
            .into(),
        });

        assert_eq!(
            zone_inbox_refunds_mismatched_owner(&request, caller),
            Some(owner)
        );
    }

    #[test]
    fn zone_inbox_refunds_mismatched_owner_ignores_other_calls() {
        let caller = Address::repeat_byte(0x11);
        let mut request = zone_inbox_refunds_request(Address::repeat_byte(0x22));
        request.inner.to = Some(TxKind::Call(Address::repeat_byte(0x33)));

        assert_eq!(zone_inbox_refunds_mismatched_owner(&request, caller), None);
    }

    #[test]
    fn parses_allowed_tip20_calls() {
        let transfer = ITIP20::transferFromCall {
            from: Address::repeat_byte(0x11),
            to: Address::repeat_byte(0x22),
            amount: U256::from(7),
        };
        let approve = ITIP20::approveCall {
            spender: Address::repeat_byte(0x33),
            amount: U256::from(9),
        };

        for (input, expected) in [
            (
                Bytes::from(transfer.abi_encode()),
                ParsedZoneUserCall::TransferFrom,
            ),
            (
                Bytes::from(approve.abi_encode()),
                ParsedZoneUserCall::Approve,
            ),
        ] {
            assert_eq!(
                parse_zone_user_call(TxKind::Call(TOKEN), &input).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn rejects_disallowed_and_malformed_tip20_calls() {
        let transfer = ITIP20::transferCall {
            to: Address::repeat_byte(0x22),
            amount: U256::from(7),
        };

        assert!(parse_zone_user_call(TxKind::Call(TOKEN), &transfer.abi_encode().into()).is_err());
        assert!(
            parse_zone_user_call(
                TxKind::Call(TOKEN),
                &ITIP20::approveCall::SELECTOR.to_vec().into(),
            )
            .is_err()
        );
    }

    #[test]
    fn parses_both_withdrawal_overloads() {
        let seven_args = LegacyZoneOutboxWithdrawal::requestWithdrawalCall {
            token: TOKEN,
            to: Address::repeat_byte(0x22),
            amount: 7,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackRecipient: Address::repeat_byte(0x33),
            data: Bytes::new(),
        };
        let eight_args = ZoneOutbox::requestWithdrawalCall {
            token: TOKEN,
            to: Address::repeat_byte(0x22),
            amount: 7,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackRecipient: Address::repeat_byte(0x33),
            data: Bytes::new(),
            revealTo: Bytes::new(),
        };

        for input in [seven_args.abi_encode(), eight_args.abi_encode()] {
            assert_eq!(
                parse_zone_user_call(TxKind::Call(ZONE_OUTBOX_ADDRESS), &input.into()).unwrap(),
                ParsedZoneUserCall::RequestWithdrawal
            );
        }
    }

    #[test]
    fn rejects_protocol_only_inbox_and_outbox_calls() {
        for (target, input) in [
            (
                ZONE_INBOX_ADDRESS,
                ZoneInbox::advanceTempoCall::SELECTOR.to_vec(),
            ),
            (
                ZONE_OUTBOX_ADDRESS,
                ZoneOutbox::finalizeWithdrawalBatchCall::SELECTOR.to_vec(),
            ),
            (
                ZONE_OUTBOX_ADDRESS,
                ZoneOutbox::enqueueDepositBounceBackCall::SELECTOR.to_vec(),
            ),
            (
                ZONE_OUTBOX_ADDRESS,
                ZoneOutbox::consumeFallbackRecipientCall::SELECTOR.to_vec(),
            ),
        ] {
            assert!(parse_zone_user_call(TxKind::Call(target), &input.into()).is_err());
        }
    }

    #[test]
    fn allows_other_outbox_and_non_tip20_calls() {
        assert_eq!(
            parse_zone_user_call(
                TxKind::Call(ZONE_OUTBOX_ADDRESS),
                &ZoneOutbox::lastBatchCall::SELECTOR.to_vec().into(),
            )
            .unwrap(),
            ParsedZoneUserCall::Other
        );
        assert_eq!(
            parse_zone_user_call(TxKind::Call(Address::repeat_byte(0x1c)), &Bytes::new(),).unwrap(),
            ParsedZoneUserCall::Other
        );
    }

    #[test]
    fn parses_every_call_in_an_aa_batch() {
        let allowed = ITIP20::approveCall {
            spender: Address::repeat_byte(0x33),
            amount: U256::from(9),
        };
        let forbidden = ITIP20::mintCall {
            to: Address::repeat_byte(0x44),
            amount: U256::from(1),
        };
        let transaction = TempoTransaction {
            calls: vec![
                Call {
                    to: TxKind::Call(TOKEN),
                    value: U256::ZERO,
                    input: allowed.abi_encode().into(),
                },
                Call {
                    to: TxKind::Call(TOKEN),
                    value: U256::ZERO,
                    input: forbidden.abi_encode().into(),
                },
            ],
            ..Default::default()
        };
        let signature =
            TempoSignature::Primitive(PrimitiveSignature::Secp256k1(Signature::test_signature()));
        let envelope = AASigned::new_unhashed(transaction, signature).into();

        assert!(parse_zone_user_transaction(&envelope).is_err());
    }

    #[test]
    fn parses_authorized_raw_transaction_once_for_submission() {
        let signer = PrivateKeySigner::random();
        let approve = ITIP20::approveCall {
            spender: Address::repeat_byte(0x33),
            amount: U256::from(9),
        };
        let raw = signed_raw_call(&signer, TOKEN, approve.abi_encode().into());

        let authorized =
            parse_authorized_raw_transaction(raw.clone(), &auth(signer.address())).unwrap();
        assert_eq!(authorized.into_inner(), raw);
    }

    #[test]
    fn raw_transaction_policy_rejects_sender_mismatch_and_disallowed_call() {
        let signer = PrivateKeySigner::random();
        let approve = ITIP20::approveCall {
            spender: Address::repeat_byte(0x33),
            amount: U256::from(9),
        };
        let raw = signed_raw_call(&signer, TOKEN, approve.abi_encode().into());
        let error =
            parse_authorized_raw_transaction(raw, &auth(PrivateKeySigner::random().address()))
                .unwrap_err();
        assert_eq!(error.code, -32003);

        let transfer = ITIP20::transferCall {
            to: Address::repeat_byte(0x22),
            amount: U256::from(7),
        };
        let raw = signed_raw_call(&signer, TOKEN, transfer.abi_encode().into());
        let error = parse_authorized_raw_transaction(raw, &auth(signer.address())).unwrap_err();
        assert_eq!(error.code, -32003);
    }
}
