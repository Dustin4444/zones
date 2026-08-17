//! Canonical protocol calldata decoding.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{B256, Bytes};
use alloy_rlp::{Decodable as _, Encodable as _};
use alloy_sol_types::{SolCall as _, SolValue as _};
use reth_primitives_traits::SealedHeader;
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, ZonePortal};

use tempo_zone_contracts::{
    MAX_DEPOSITS_PER_TEMPO_BLOCK, MAX_SEQUENCERS, MAX_TOKEN_METADATA_BYTES,
    MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK,
};
use zone_precompiles::{
    ecies::{AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE, ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE},
    outbox::MAX_CALLBACK_DATA_SIZE,
};

use super::error::{
    AuthenticatedDataEvidence, AuthenticatedTransaction, DataSource, ObservationError,
    PortalCallFamily,
};

/// A malformed ABI surface before it is attached to an authenticated transaction.
#[derive(Debug)]
struct AbiError {
    source: DataSource,
    evidence: AuthenticatedDataEvidence,
    detail: String,
}

impl AbiError {
    fn into_observation(self, transaction: AuthenticatedTransaction) -> ObservationError {
        ObservationError::malformed(self.source, transaction, self.evidence, self.detail)
    }
}

/// Bytes and their protocol source used to construct consistent malformed-data errors.
#[derive(Clone, Copy)]
struct Surface<'a> {
    source: DataSource,
    bytes: &'a [u8],
}

impl<'a> Surface<'a> {
    const fn new(source: DataSource, bytes: &'a [u8]) -> Self {
        Self { source, bytes }
    }

    fn malformed(self, detail: impl core::fmt::Display) -> AbiError {
        AbiError {
            source: self.source,
            evidence: AuthenticatedDataEvidence::from_bytes(self.bytes),
            detail: detail.to_string(),
        }
    }
}

/// Canonical Tempo header selected at an authenticated observation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedTempoHeader {
    sealed: SealedHeader<TempoHeader>,
}

impl ImportedTempoHeader {
    pub(super) fn new(header: TempoHeader) -> Self {
        Self {
            sealed: SealedHeader::seal_slow(header),
        }
    }

    pub(crate) fn header(&self) -> &TempoHeader {
        self.sealed.header()
    }

    pub(crate) fn hash(&self) -> B256 {
        self.sealed.hash()
    }

    pub(crate) fn number(&self) -> u64 {
        self.sealed.number()
    }
}

/// A nested queue entry after its opaque `depositData` bytes are decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedDeposit {
    kind: ImportedDepositKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportedDepositKind {
    WithdrawalBounceBack(IZoneInbox::WithdrawalBounceBackDeposit),
    Ordinary(ZonePortal::Deposit),
}

impl ImportedDeposit {
    pub(crate) fn as_withdrawal_bounce_back(
        &self,
    ) -> Option<&IZoneInbox::WithdrawalBounceBackDeposit> {
        match &self.kind {
            ImportedDepositKind::WithdrawalBounceBack(deposit) => Some(deposit),
            ImportedDepositKind::Ordinary(_) => None,
        }
    }

    pub(crate) fn as_ordinary(&self) -> Option<&ZonePortal::Deposit> {
        match &self.kind {
            ImportedDepositKind::Ordinary(deposit) => Some(deposit),
            ImportedDepositKind::WithdrawalBounceBack(_) => None,
        }
    }
}

/// Authenticated inputs carried by `advanceTempo` calldata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedAdvanceTempo {
    imported_header: ImportedTempoHeader,
    deposits: Vec<ImportedDeposit>,
    enabled_tokens: Vec<IZoneInbox::EnabledToken>,
}

impl DecodedAdvanceTempo {
    pub(crate) fn imported_header(&self) -> &ImportedTempoHeader {
        &self.imported_header
    }

    pub(crate) fn deposits(&self) -> &[ImportedDeposit] {
        &self.deposits
    }

    pub(crate) fn enabled_tokens(&self) -> &[IZoneInbox::EnabledToken] {
        &self.enabled_tokens
    }
}

/// Authenticated inputs carried by the optional final system transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedFinalization {
    count: usize,
    block_number: u64,
    encrypted_senders: Vec<Bytes>,
}

impl DecodedFinalization {
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(crate) fn encrypted_senders(&self) -> &[Bytes] {
        &self.encrypted_senders
    }
}

/// Decoded top-level Portal calldata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedPortalCall {
    kind: DecodedPortalCallKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DecodedPortalCallKind {
    SubmitBatch(Box<ZonePortal::submitBatchCall>),
    ProcessWithdrawals(ZonePortal::processWithdrawalsCall),
    SetBouncebackGas(ZonePortal::setBouncebackGasCall),
    EnableToken(ZonePortal::enableTokenCall),
    Deposit,
    ClaimRefund,
}

impl DecodedPortalCall {
    pub(crate) fn as_submit_batch(&self) -> Option<&ZonePortal::submitBatchCall> {
        match &self.kind {
            DecodedPortalCallKind::SubmitBatch(call) => Some(call),
            DecodedPortalCallKind::ProcessWithdrawals(_) => None,
            DecodedPortalCallKind::SetBouncebackGas(_) => None,
            DecodedPortalCallKind::EnableToken(_) => None,
            DecodedPortalCallKind::Deposit | DecodedPortalCallKind::ClaimRefund => None,
        }
    }

    pub(crate) fn as_process_withdrawals(&self) -> Option<&ZonePortal::processWithdrawalsCall> {
        match &self.kind {
            DecodedPortalCallKind::ProcessWithdrawals(call) => Some(call),
            DecodedPortalCallKind::SubmitBatch(_) => None,
            DecodedPortalCallKind::SetBouncebackGas(_) => None,
            DecodedPortalCallKind::EnableToken(_) => None,
            DecodedPortalCallKind::Deposit | DecodedPortalCallKind::ClaimRefund => None,
        }
    }

    pub(crate) fn as_set_bounceback_gas(&self) -> Option<&ZonePortal::setBouncebackGasCall> {
        match &self.kind {
            DecodedPortalCallKind::SetBouncebackGas(call) => Some(call),
            DecodedPortalCallKind::SubmitBatch(_)
            | DecodedPortalCallKind::ProcessWithdrawals(_)
            | DecodedPortalCallKind::EnableToken(_)
            | DecodedPortalCallKind::Deposit
            | DecodedPortalCallKind::ClaimRefund => None,
        }
    }

    pub(crate) fn as_enable_token(&self) -> Option<&ZonePortal::enableTokenCall> {
        match &self.kind {
            DecodedPortalCallKind::EnableToken(call) => Some(call),
            DecodedPortalCallKind::SubmitBatch(_)
            | DecodedPortalCallKind::ProcessWithdrawals(_)
            | DecodedPortalCallKind::SetBouncebackGas(_)
            | DecodedPortalCallKind::Deposit
            | DecodedPortalCallKind::ClaimRefund => None,
        }
    }

    pub(crate) const fn is_deposit(&self) -> bool {
        matches!(self.kind, DecodedPortalCallKind::Deposit)
    }

    pub(crate) const fn is_claim_refund(&self) -> bool {
        matches!(self.kind, DecodedPortalCallKind::ClaimRefund)
    }

    pub(crate) fn is_nonempty_process_withdrawals(&self) -> bool {
        self.as_process_withdrawals()
            .is_some_and(|call| !call.withdrawals.is_empty())
    }

    pub(crate) const fn family(&self) -> PortalCallFamily {
        match &self.kind {
            DecodedPortalCallKind::SubmitBatch(_) => PortalCallFamily::SubmitBatch,
            DecodedPortalCallKind::ProcessWithdrawals(_) => PortalCallFamily::ProcessWithdrawals,
            DecodedPortalCallKind::SetBouncebackGas(_) => PortalCallFamily::StateUpdate,
            DecodedPortalCallKind::EnableToken(_) => PortalCallFamily::StateUpdate,
            DecodedPortalCallKind::Deposit | DecodedPortalCallKind::ClaimRefund => {
                PortalCallFamily::StateUpdate
            }
        }
    }
}

/// Strictly decode canonical `advanceTempo` calldata from its authenticated transaction.
pub(crate) fn decode_advance_tempo(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedAdvanceTempo, ObservationError> {
    parse_advance_tempo(calldata).map_err(|error| error.into_observation(transaction))
}

/// Parse `advanceTempo` calldata and reject oversized or non-canonical encodings.
fn parse_advance_tempo(calldata: &[u8]) -> Result<DecodedAdvanceTempo, AbiError> {
    let advance_surface = Surface::new(DataSource::AdvanceTempoCalldata, calldata);
    let decoded = IZoneInbox::advanceTempoCall::abi_decode_validate(calldata)
        .map_err(|error| advance_surface.malformed(error))?;
    if decoded.abi_encode() != calldata {
        return Err(advance_surface.malformed("encoding is non-canonical or has trailing bytes"));
    }
    if decoded.deposits.len() > MAX_DEPOSITS_PER_TEMPO_BLOCK {
        return Err(advance_surface.malformed(format!(
            "deposit count {} exceeds {MAX_DEPOSITS_PER_TEMPO_BLOCK}",
            decoded.deposits.len()
        )));
    }
    if decoded.enabledTokens.len() > MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK {
        return Err(advance_surface.malformed(format!(
            "enabled token count {} exceeds {MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK}",
            decoded.enabledTokens.len()
        )));
    }
    for token in &decoded.enabledTokens {
        for (field, len) in [
            ("name", token.name.len()),
            ("symbol", token.symbol.len()),
            ("currency", token.currency.len()),
        ] {
            if len > MAX_TOKEN_METADATA_BYTES {
                return Err(advance_surface.malformed(format!(
                    "token {field} byte length {len} exceeds {MAX_TOKEN_METADATA_BYTES}"
                )));
            }
        }
    }

    let header_surface = Surface::new(DataSource::AdvanceHeaderRlp, &decoded.header);
    let mut remaining = decoded.header.as_ref();
    let header =
        TempoHeader::decode(&mut remaining).map_err(|error| header_surface.malformed(error))?;
    if !remaining.is_empty() {
        return Err(header_surface.malformed(format!("{} trailing bytes", remaining.len())));
    }
    let mut canonical = Vec::with_capacity(header.length());
    header.encode(&mut canonical);
    if canonical != decoded.header {
        return Err(header_surface.malformed("non-canonical encoding"));
    }
    let imported_header = ImportedTempoHeader::new(header);

    let mut deposits = Vec::with_capacity(decoded.deposits.len());
    let mut ordinary_count = 0usize;
    for queued in decoded.deposits {
        let data = queued.depositData;
        let deposit = match queued.depositType as u8 {
            0 => {
                let surface = Surface::new(DataSource::WithdrawalBounceBackData, data.as_ref());
                let decoded = IZoneInbox::WithdrawalBounceBackDeposit::abi_decode_validate(&data)
                    .map_err(|error| surface.malformed(error))?;
                if decoded.abi_encode() != data {
                    return Err(
                        surface.malformed("encoding is non-canonical or has trailing bytes")
                    );
                }
                ImportedDeposit {
                    kind: ImportedDepositKind::WithdrawalBounceBack(decoded),
                }
            }
            1 => {
                let surface = Surface::new(DataSource::OrdinaryDepositData, data.as_ref());
                let decoded = ZonePortal::Deposit::abi_decode_validate(&data)
                    .map_err(|error| surface.malformed(error))?;
                if decoded.abi_encode() != data {
                    return Err(
                        surface.malformed("encoding is non-canonical or has trailing bytes")
                    );
                }
                if decoded.encrypted.ciphertext.len() != ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE {
                    return Err(surface.malformed(format!(
                        "ciphertext length {}, expected {ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE}",
                        decoded.encrypted.ciphertext.len()
                    )));
                }
                ordinary_count += 1;
                ImportedDeposit {
                    kind: ImportedDepositKind::Ordinary(decoded),
                }
            }
            _ => {
                return Err(advance_surface.malformed("unsupported deposit discriminator"));
            }
        };
        deposits.push(deposit);
    }
    if decoded.decryptions.len() != ordinary_count {
        return Err(advance_surface.malformed(format!(
            "decryption count {} does not match ordinary deposit count {ordinary_count}",
            decoded.decryptions.len()
        )));
    }

    Ok(DecodedAdvanceTempo {
        imported_header,
        deposits,
        enabled_tokens: decoded.enabledTokens,
    })
}

/// Strictly decode canonical finalization calldata from its authenticated transaction.
pub(crate) fn decode_finalization(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedFinalization, ObservationError> {
    parse_finalization(calldata).map_err(|error| error.into_observation(transaction))
}

/// Parse finalization calldata and reject oversized or non-canonical encodings.
fn parse_finalization(calldata: &[u8]) -> Result<DecodedFinalization, AbiError> {
    let surface = Surface::new(DataSource::FinalizationCalldata, calldata);
    let call = IZoneOutbox::finalizeWithdrawalBatchCall::abi_decode_validate(calldata)
        .map_err(|error| surface.malformed(error))?;
    if call.abi_encode() != calldata {
        return Err(surface.malformed("encoding is non-canonical or has trailing bytes"));
    }
    let count =
        usize::try_from(call.count).map_err(|_| surface.malformed("count overflows usize"))?;
    if count != call.encryptedSenders.len() {
        return Err(surface.malformed(format!(
            "count {count} does not match encryptedSenders length {}",
            call.encryptedSenders.len()
        )));
    }
    for (index, sender) in call.encryptedSenders.iter().enumerate() {
        if !matches!(sender.len(), 0 | AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE) {
            return Err(surface.malformed(format!(
                "encrypted sender {index} has length {}, expected 0 or {AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE}",
                sender.len()
            )));
        }
    }
    Ok(DecodedFinalization {
        count,
        block_number: call.blockNumber,
        encrypted_senders: call.encryptedSenders,
    })
}

/// Strictly decode the Portal calldata whose family was implied by receipt outcomes.
pub(crate) fn decode_portal_call(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedPortalCall, ObservationError> {
    parse_portal_call(calldata).map_err(|error| error.into_observation(transaction))
}

/// Parse a supported Portal call using the same trailing-byte tolerance as Solidity.
fn parse_portal_call(calldata: &[u8]) -> Result<DecodedPortalCall, AbiError> {
    if calldata.starts_with(&ZonePortal::submitBatchCall::SELECTOR) {
        return decode_submit_batch(calldata);
    }
    if calldata.starts_with(&ZonePortal::processWithdrawalsCall::SELECTOR) {
        return decode_process_withdrawals(calldata);
    }
    if calldata.starts_with(&ZonePortal::setBouncebackGasCall::SELECTOR) {
        let surface = Surface::new(DataSource::PortalTransactionCalldata, calldata);
        let call = ZonePortal::setBouncebackGasCall::abi_decode_validate(calldata)
            .map_err(|error| surface.malformed(error))?;
        return Ok(DecodedPortalCall {
            kind: DecodedPortalCallKind::SetBouncebackGas(call),
        });
    }
    if calldata.starts_with(&ZonePortal::enableTokenCall::SELECTOR) {
        let surface = Surface::new(DataSource::PortalTransactionCalldata, calldata);
        let call = ZonePortal::enableTokenCall::abi_decode_validate(calldata)
            .map_err(|error| surface.malformed(error))?;
        return Ok(DecodedPortalCall {
            kind: DecodedPortalCallKind::EnableToken(call),
        });
    }
    let kind = if calldata.starts_with(&ZonePortal::depositCall::SELECTOR)
        || calldata.starts_with(&ZonePortal::depositEncryptedCall::SELECTOR)
    {
        Some(DecodedPortalCallKind::Deposit)
    } else if calldata.starts_with(&ZonePortal::claimRefundCall::SELECTOR) {
        Some(DecodedPortalCallKind::ClaimRefund)
    } else {
        None
    };
    if let Some(kind) = kind {
        return Ok(DecodedPortalCall { kind });
    }
    Err(
        Surface::new(DataSource::PortalTransactionCalldata, calldata)
            .malformed("selector does not match its authenticated protocol events"),
    )
}

pub(crate) fn is_direct_portal_state_change(calldata: &[u8]) -> bool {
    calldata.starts_with(&ZonePortal::setBouncebackGasCall::SELECTOR)
        || calldata.starts_with(&ZonePortal::enableTokenCall::SELECTOR)
        || calldata.starts_with(&ZonePortal::depositCall::SELECTOR)
        || calldata.starts_with(&ZonePortal::depositEncryptedCall::SELECTOR)
        || calldata.starts_with(&ZonePortal::claimRefundCall::SELECTOR)
}

/// Decode `submitBatch` calldata, tolerating the same trailing bytes Solidity accepts.
fn decode_submit_batch(calldata: &[u8]) -> Result<DecodedPortalCall, AbiError> {
    let surface = Surface::new(DataSource::SubmitBatchCalldata, calldata);
    let call = ZonePortal::submitBatchCall::abi_decode_validate(calldata)
        .map_err(|error| surface.malformed(error))?;
    if call.signatures.len() > MAX_SEQUENCERS {
        return Err(surface.malformed(format!(
            "signature count {} exceeds {MAX_SEQUENCERS}",
            call.signatures.len()
        )));
    }
    Ok(DecodedPortalCall {
        kind: DecodedPortalCallKind::SubmitBatch(Box::new(call)),
    })
}

/// Decode `processWithdrawals` calldata, tolerating the same trailing bytes Solidity accepts.
fn decode_process_withdrawals(calldata: &[u8]) -> Result<DecodedPortalCall, AbiError> {
    let surface = Surface::new(DataSource::ProcessWithdrawalsCalldata, calldata);
    let call = ZonePortal::processWithdrawalsCall::abi_decode_validate(calldata)
        .map_err(|error| surface.malformed(error))?;
    for (index, withdrawal) in call.withdrawals.iter().enumerate() {
        if withdrawal.callbackData.len() > MAX_CALLBACK_DATA_SIZE {
            return Err(surface.malformed(format!(
                "withdrawal {index} callbackData length {} exceeds {MAX_CALLBACK_DATA_SIZE}",
                withdrawal.callbackData.len()
            )));
        }
        if !matches!(
            withdrawal.encryptedSender.len(),
            0 | AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE
        ) {
            return Err(surface.malformed(format!(
                "withdrawal {index} encryptedSender length {}, expected 0 or {AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE}",
                withdrawal.encryptedSender.len()
            )));
        }
    }
    Ok(DecodedPortalCall {
        kind: DecodedPortalCallKind::ProcessWithdrawals(call),
    })
}

#[cfg(test)]
mod tests;
