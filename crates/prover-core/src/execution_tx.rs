use alloc::{boxed::Box, vec::Vec};
use core::{fmt, num::NonZeroU64};

use alloy_consensus::{Typed2718, crypto::secp256k1};
use alloy_evm::{FromRecoveredTx, FromTxWithEncoded, IntoTxEnv, TransactionEnvMut};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use revm::context::{
    Transaction, TxEnv,
    either::Either,
    result::InvalidTransaction,
    transaction::{
        AccessList, AccessListItem, RecoveredAuthority, RecoveredAuthorization, SignedAuthorization,
    },
};
use revm::handler::SystemCallTx;
use tempo_primitives::{
    AASigned, TempoSignature, TempoTransaction, TempoTxEnvelope,
    transaction::{
        Call, RecoveredTempoAuthorization, SignedKeyAuthorization, calc_gas_balance_spending,
    },
};

/// Tempo AA transaction metadata carried alongside the base revm [`TxEnv`].
///
/// This is a no_std prover-core adaptation of upstream `tempo-revm::TempoBatchCallEnv`.
#[derive(Debug, Clone, Default)]
pub struct ZoneBatchCallEnv {
    pub signature: TempoSignature,
    pub valid_before: Option<u64>,
    pub valid_after: Option<u64>,
    pub aa_calls: Vec<Call>,
    pub tempo_authorization_list: Vec<RecoveredTempoAuthorization>,
    pub nonce_key: U256,
    pub subblock_transaction: bool,
    pub key_authorization: Option<SignedKeyAuthorization>,
    pub signature_hash: B256,
    pub tx_hash: B256,
    pub override_key_id: Option<Address>,
    pub expiring_nonce_idx: Option<usize>,
}

/// Tempo transaction environment accepted by the future revm-backed Zone executor.
///
/// This mirrors upstream `tempo-revm::TempoTxEnv` closely enough for recovered
/// `TempoTxEnvelope` values from the witness execution plan to cross the
/// alloy/revm transaction boundary without pulling the std-bound `tempo-revm`
/// crate into prover-core.
#[derive(Debug, Clone, Default)]
pub struct ZoneTxEnv {
    pub inner: TxEnv,
    pub fee_token: Option<Address>,
    pub is_system_tx: bool,
    pub unique_tx_identifier: Option<B256>,
    pub fee_payer: Option<Option<Address>>,
    pub tempo_tx_env: Option<Box<ZoneBatchCallEnv>>,
}

impl ZoneTxEnv {
    pub fn fee_payer(&self) -> Result<Address, ZoneInvalidTransaction> {
        if let Some(fee_payer) = self.fee_payer {
            fee_payer.ok_or(ZoneInvalidTransaction::InvalidFeePayerSignature)
        } else {
            Ok(self.caller())
        }
    }

    pub fn has_fee_payer_signature(&self) -> bool {
        self.fee_payer.is_some()
    }

    pub fn is_subblock_transaction(&self) -> bool {
        self.tempo_tx_env
            .as_ref()
            .is_some_and(|aa| aa.subblock_transaction)
    }

    pub fn unique_tx_identifier(&self) -> Option<B256> {
        self.unique_tx_identifier
    }

    pub fn channel_open_context_hash(&self) -> Option<B256> {
        self.unique_tx_identifier()
    }

    pub fn first_call(&self) -> Option<(&TxKind, &[u8])> {
        if let Some(aa) = self.tempo_tx_env.as_ref() {
            aa.aa_calls
                .first()
                .map(|call| (&call.to, call.input.as_ref()))
        } else {
            Some((&self.inner.kind, &self.inner.data))
        }
    }

    pub fn calls(&self) -> impl Iterator<Item = (&TxKind, &[u8])> {
        if let Some(aa) = self.tempo_tx_env.as_ref() {
            Either::Left(
                aa.aa_calls
                    .iter()
                    .map(|call| (&call.to, call.input.as_ref())),
            )
        } else {
            Either::Right(core::iter::once((
                &self.inner.kind,
                self.inner.input().as_ref(),
            )))
        }
    }
}

impl From<TxEnv> for ZoneTxEnv {
    fn from(inner: TxEnv) -> Self {
        Self {
            inner,
            ..Default::default()
        }
    }
}

impl Transaction for ZoneTxEnv {
    type AccessListItem<'a> = &'a AccessListItem;
    type Authorization<'a> = &'a Either<SignedAuthorization, RecoveredAuthorization>;

    fn tx_type(&self) -> u8 {
        self.inner.tx_type()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn caller(&self) -> Address {
        self.inner.caller()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn gas_price(&self) -> u128 {
        self.inner.gas_price()
    }

    fn value(&self) -> U256 {
        self.inner.value()
    }

    fn nonce(&self) -> u64 {
        Transaction::nonce(&self.inner)
    }

    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }

    fn access_list(&self) -> Option<impl Iterator<Item = Self::AccessListItem<'_>>> {
        self.inner.access_list()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> u128 {
        self.inner.max_fee_per_blob_gas()
    }

    fn authorization_list_len(&self) -> usize {
        self.inner.authorization_list_len()
    }

    fn authorization_list(&self) -> impl Iterator<Item = Self::Authorization<'_>> {
        self.inner.authorization_list()
    }

    fn input(&self) -> &Bytes {
        self.inner.input()
    }

    fn blob_versioned_hashes(&self) -> &[B256] {
        self.inner.blob_versioned_hashes()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_balance_spending(&self) -> Result<U256, InvalidTransaction> {
        calc_gas_balance_spending(self.gas_limit(), self.max_fee_per_gas())
            .checked_add(self.value())
            .ok_or(InvalidTransaction::OverflowPaymentInTransaction)
    }

    fn effective_balance_spending(
        &self,
        base_fee: u128,
        _blob_price: u128,
    ) -> Result<U256, InvalidTransaction> {
        calc_gas_balance_spending(self.gas_limit(), self.effective_gas_price(base_fee))
            .checked_add(self.value())
            .ok_or(InvalidTransaction::OverflowPaymentInTransaction)
    }
}

impl TransactionEnvMut for ZoneTxEnv {
    fn set_gas_limit(&mut self, gas_limit: u64) {
        self.inner.set_gas_limit(gas_limit);
    }

    fn set_nonce(&mut self, nonce: u64) {
        self.inner.set_nonce(nonce);
    }

    fn set_access_list(&mut self, access_list: AccessList) {
        self.inner.set_access_list(access_list);
    }
}

impl IntoTxEnv<Self> for ZoneTxEnv {
    fn into_tx_env(self) -> Self {
        self
    }
}

impl SystemCallTx for ZoneTxEnv {
    fn new_system_tx_with_caller(
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> Self {
        Self {
            inner: TxEnv::new_system_tx_with_caller(caller, system_contract_address, data),
            is_system_tx: true,
            ..Default::default()
        }
    }
}

impl FromRecoveredTx<AASigned> for ZoneTxEnv {
    fn from_recovered_tx(aa_signed: &AASigned, caller: Address) -> Self {
        let tx = aa_signed.tx();
        let signature = aa_signed.signature();

        if let Some(keychain_sig) = signature.as_keychain() {
            let _ = keychain_sig.key_id(&aa_signed.signature_hash());
        }

        let TempoTransaction {
            chain_id,
            fee_token,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            calls,
            access_list,
            nonce_key,
            nonce,
            fee_payer_signature,
            valid_before,
            valid_after,
            key_authorization,
            tempo_authorization_list,
        } = tx;

        let (to, value, input) = if let Some(first_call) = calls.first() {
            (first_call.to, first_call.value, first_call.input.clone())
        } else {
            (TxKind::Create, U256::ZERO, Bytes::new())
        };

        Self {
            inner: TxEnv {
                tx_type: tx.ty(),
                caller,
                gas_limit: *gas_limit,
                gas_price: *max_fee_per_gas,
                kind: to,
                value,
                data: input,
                nonce: *nonce,
                chain_id: Some(*chain_id),
                gas_priority_fee: Some(*max_priority_fee_per_gas),
                access_list: access_list.clone(),
                authorization_list: tempo_authorization_list
                    .iter()
                    .map(|auth| {
                        let authority = auth
                            .recover_authority()
                            .map_or(RecoveredAuthority::Invalid, RecoveredAuthority::Valid);
                        Either::Right(RecoveredAuthorization::new_unchecked(
                            auth.inner().clone(),
                            authority,
                        ))
                    })
                    .collect(),
                ..Default::default()
            },
            fee_token: *fee_token,
            is_system_tx: false,
            unique_tx_identifier: Some(aa_signed.expiring_nonce_hash(caller)),
            fee_payer: fee_payer_signature.map(|sig| {
                secp256k1::recover_signer(&sig, tx.fee_payer_signature_hash(caller)).ok()
            }),
            tempo_tx_env: Some(Box::new(ZoneBatchCallEnv {
                signature: signature.clone(),
                valid_before: valid_before.map(NonZeroU64::get),
                valid_after: valid_after.map(NonZeroU64::get),
                aa_calls: calls.clone(),
                tempo_authorization_list: tempo_authorization_list
                    .iter()
                    .map(|auth| RecoveredTempoAuthorization::recover(auth.clone()))
                    .collect(),
                nonce_key: *nonce_key,
                subblock_transaction: aa_signed.tx().subblock_proposer().is_some(),
                key_authorization: key_authorization.clone(),
                signature_hash: aa_signed.signature_hash(),
                tx_hash: *aa_signed.hash(),
                override_key_id: None,
                expiring_nonce_idx: None,
            })),
        }
    }
}

impl FromRecoveredTx<TempoTxEnvelope> for ZoneTxEnv {
    fn from_recovered_tx(tx: &TempoTxEnvelope, sender: Address) -> Self {
        match tx {
            tx @ TempoTxEnvelope::Legacy(inner) => Self {
                inner: TxEnv::from_recovered_tx(inner.tx(), sender),
                fee_token: None,
                is_system_tx: tx.is_system_tx(),
                unique_tx_identifier: Some(tx.unique_tx_identifier(sender)),
                fee_payer: None,
                tempo_tx_env: None,
            },
            TempoTxEnvelope::Eip2930(inner) => Self {
                inner: TxEnv::from_recovered_tx(inner.tx(), sender),
                unique_tx_identifier: Some(tx.unique_tx_identifier(sender)),
                ..Default::default()
            },
            TempoTxEnvelope::Eip1559(inner) => Self {
                inner: TxEnv::from_recovered_tx(inner.tx(), sender),
                unique_tx_identifier: Some(tx.unique_tx_identifier(sender)),
                ..Default::default()
            },
            TempoTxEnvelope::Eip7702(inner) => Self {
                inner: TxEnv::from_recovered_tx(inner.tx(), sender),
                unique_tx_identifier: Some(tx.unique_tx_identifier(sender)),
                ..Default::default()
            },
            TempoTxEnvelope::AA(tx) => Self::from_recovered_tx(tx, sender),
        }
    }
}

impl FromTxWithEncoded<AASigned> for ZoneTxEnv {
    fn from_encoded_tx(tx: &AASigned, sender: Address, _encoded: Bytes) -> Self {
        Self::from_recovered_tx(tx, sender)
    }
}

impl FromTxWithEncoded<TempoTxEnvelope> for ZoneTxEnv {
    fn from_encoded_tx(tx: &TempoTxEnvelope, sender: Address, _encoded: Bytes) -> Self {
        Self::from_recovered_tx(tx, sender)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneInvalidTransaction {
    InvalidFeePayerSignature,
}

impl fmt::Display for ZoneInvalidTransaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFeePayerSignature => f.write_str("invalid Tempo fee payer signature"),
        }
    }
}

impl core::error::Error for ZoneInvalidTransaction {}

#[cfg(test)]
mod tests {
    use alloy_consensus::{Signed, TxLegacy};
    use alloy_primitives::{Address, Bytes, TxKind, U256, address};
    use revm::context::Transaction;
    use tempo_primitives::transaction::envelope::{
        TEMPO_SYSTEM_TX_SENDER, TEMPO_SYSTEM_TX_SIGNATURE,
    };

    use super::*;

    #[test]
    fn converts_system_legacy_transaction_to_zone_tx_env() {
        let to = address!("0x0000000000000000000000000000000000001000");
        let input = Bytes::from_static(b"system");
        let tx = TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            gas_limit: 0,
            to: to.into(),
            value: U256::ZERO,
            input: input.clone(),
        };
        let envelope = TempoTxEnvelope::Legacy(Signed::new_unhashed(tx, TEMPO_SYSTEM_TX_SIGNATURE));

        let env = ZoneTxEnv::from_recovered_tx(&envelope, TEMPO_SYSTEM_TX_SENDER);

        assert_eq!(env.caller(), TEMPO_SYSTEM_TX_SENDER);
        assert_eq!(env.kind(), TxKind::Call(to));
        assert_eq!(env.input(), &input);
        assert!(env.is_system_tx);
        assert_eq!(env.fee_payer().unwrap(), TEMPO_SYSTEM_TX_SENDER);
        assert!(env.unique_tx_identifier().is_some());
    }

    #[test]
    fn rejects_invalid_fee_payer_cache() {
        let env = ZoneTxEnv {
            inner: TxEnv {
                caller: Address::ZERO,
                ..Default::default()
            },
            fee_payer: Some(None),
            ..Default::default()
        };

        assert_eq!(
            env.fee_payer().unwrap_err(),
            ZoneInvalidTransaction::InvalidFeePayerSignature
        );
    }
}
