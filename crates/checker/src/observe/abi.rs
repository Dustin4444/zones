//! Allocation-bounded, canonical protocol calldata decoding.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{B256, Bytes, U256};
use alloy_rlp::{Decodable as _, Encodable as _};
use alloy_sol_types::{SolCall as _, SolValue as _};
use reth_primitives_traits::SealedHeader;
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, ZonePortal};

use crate::model::constants::{
    AUTHENTICATED_WITHDRAWAL_SIZE, ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE, MAX_CALLBACK_DATA_SIZE,
    MAX_DEPOSITS_PER_TEMPO_BLOCK, MAX_SEQUENCERS, MAX_TOKEN_CURRENCY_BYTES, MAX_TOKEN_NAME_BYTES,
    MAX_TOKEN_SYMBOL_BYTES, MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK,
};

use super::error::{
    AuthenticatedDataEvidence, AuthenticatedTransaction, DataSource, ObservationError,
    PortalCallFamily,
};

const WORD: usize = 32;
const SELECTOR: usize = 4;
// `ZonePortal.Deposit` is one top-level offset, a six-word tuple head,
// a five-word encrypted-payload head, and a three-word ciphertext tail.
const ORDINARY_DEPOSIT_ENCODED_SIZE: usize = 15 * WORD;

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

    #[cfg(test)]
    pub(crate) fn for_test(header: TempoHeader) -> Self {
        Self::new(header)
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
    decryptions: Vec<IZoneInbox::DecryptionData>,
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

    #[cfg(test)]
    pub(crate) fn empty_for_test(imported_header: ImportedTempoHeader) -> Self {
        Self {
            imported_header,
            deposits: Vec::new(),
            decryptions: Vec::new(),
            enabled_tokens: Vec::new(),
        }
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

/// Selectively acquired top-level Portal calldata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedPortalCall {
    kind: DecodedPortalCallKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DecodedPortalCallKind {
    SubmitBatch(Box<ZonePortal::submitBatchCall>),
    ProcessWithdrawals(ZonePortal::processWithdrawalsCall),
}

impl DecodedPortalCall {
    pub(crate) fn as_submit_batch(&self) -> Option<&ZonePortal::submitBatchCall> {
        match &self.kind {
            DecodedPortalCallKind::SubmitBatch(call) => Some(call),
            DecodedPortalCallKind::ProcessWithdrawals(_) => None,
        }
    }

    pub(crate) fn as_process_withdrawals(&self) -> Option<&ZonePortal::processWithdrawalsCall> {
        match &self.kind {
            DecodedPortalCallKind::ProcessWithdrawals(call) => Some(call),
            DecodedPortalCallKind::SubmitBatch(_) => None,
        }
    }

    pub(crate) fn is_nonempty_process_withdrawals(&self) -> bool {
        self.as_process_withdrawals()
            .is_some_and(|call| !call.withdrawals.is_empty())
    }

    pub(crate) const fn family(&self) -> PortalCallFamily {
        match &self.kind {
            DecodedPortalCallKind::SubmitBatch(_) => PortalCallFamily::SubmitBatch,
            DecodedPortalCallKind::ProcessWithdrawals(_) => PortalCallFamily::ProcessWithdrawals,
        }
    }
}

/// A checked view over an ABI payload, excluding its four-byte selector.
/// Every helper checks integer conversion and range arithmetic before a
/// generated decoder can allocate from an attacker-controlled length word.
struct Bounds<'a> {
    surface: Surface<'a>,
    data: &'a [u8],
}

impl<'a> Bounds<'a> {
    fn from_call(
        source: DataSource,
        calldata: &'a [u8],
        selector: &[u8; SELECTOR],
    ) -> Result<Self, AbiError> {
        let surface = Surface::new(source, calldata);
        if !calldata.starts_with(selector) {
            return Err(surface.malformed("wrong function selector"));
        }
        Ok(Self {
            surface,
            data: &calldata[SELECTOR..],
        })
    }

    fn ensure_head(&self, words: usize) -> Result<(), AbiError> {
        let bytes = checked_mul(words, WORD, self.surface)?;
        if self.data.len() < bytes {
            return Err(self.surface.malformed(format!(
                "ABI head needs {bytes} bytes, got {}",
                self.data.len()
            )));
        }
        Ok(())
    }

    fn word(&self, offset: usize) -> Result<&'a [u8], AbiError> {
        let end = checked_add(offset, WORD, self.surface)?;
        self.data.get(offset..end).ok_or_else(|| {
            self.surface
                .malformed(format!("word at byte {offset} is truncated"))
        })
    }

    fn usize_word(&self, offset: usize) -> Result<usize, AbiError> {
        let value = U256::from_be_slice(self.word(offset)?);
        usize::try_from(value).map_err(|_| {
            self.surface
                .malformed(format!("word at byte {offset} does not fit usize"))
        })
    }

    fn relative(
        &self,
        base: usize,
        word_index: usize,
        minimum_head_words: usize,
    ) -> Result<usize, AbiError> {
        let word_offset = checked_add(
            base,
            checked_mul(word_index, WORD, self.surface)?,
            self.surface,
        )?;
        let relative = self.usize_word(word_offset)?;
        let minimum = checked_mul(minimum_head_words, WORD, self.surface)?;
        if relative < minimum || relative % WORD != 0 {
            return Err(self.surface.malformed(format!(
                "invalid dynamic offset {relative} at byte {word_offset}"
            )));
        }
        let absolute = checked_add(base, relative, self.surface)?;
        self.word(absolute)?;
        Ok(absolute)
    }

    fn bytes_field(
        &self,
        base: usize,
        word_index: usize,
        minimum_head_words: usize,
        maximum: usize,
        field: &'static str,
    ) -> Result<&'a [u8], AbiError> {
        let start = self.relative(base, word_index, minimum_head_words)?;
        let length = self.usize_word(start)?;
        if length > maximum {
            return Err(self
                .surface
                .malformed(format!("{field} length {length} exceeds {maximum}")));
        }
        let data_start = checked_add(start, WORD, self.surface)?;
        let padded = padded_length(length, self.surface)?;
        let data_end = checked_add(data_start, padded, self.surface)?;
        if data_end > self.data.len() {
            return Err(self
                .surface
                .malformed(format!("{field} length {length} exceeds calldata")));
        }
        Ok(&self.data[data_start..data_start + length])
    }

    /// Validate a dynamic byte string whose array element offset points
    /// directly at its length word.
    fn direct_bytes(
        &self,
        start: usize,
        maximum: usize,
        field: &'static str,
    ) -> Result<&'a [u8], AbiError> {
        let length = self.usize_word(start)?;
        if length > maximum {
            return Err(self
                .surface
                .malformed(format!("{field} length {length} exceeds {maximum}")));
        }
        let data_start = checked_add(start, WORD, self.surface)?;
        let data_end = checked_add(
            data_start,
            padded_length(length, self.surface)?,
            self.surface,
        )?;
        if data_end > self.data.len() {
            return Err(self
                .surface
                .malformed(format!("{field} length {length} exceeds calldata")));
        }
        Ok(&self.data[data_start..data_start + length])
    }

    fn dynamic_array(
        &self,
        base: usize,
        word_index: usize,
        minimum_head_words: usize,
        maximum_count: usize,
        field: &'static str,
    ) -> Result<(usize, usize), AbiError> {
        let array = self.relative(base, word_index, minimum_head_words)?;
        let count = self.usize_word(array)?;
        if count > maximum_count {
            return Err(self
                .surface
                .malformed(format!("{field} count {count} exceeds {maximum_count}")));
        }
        Ok((checked_add(array, WORD, self.surface)?, count))
    }

    fn dynamic_element(
        &self,
        array_head: usize,
        count: usize,
        index: usize,
    ) -> Result<usize, AbiError> {
        let table_bytes = checked_mul(count, WORD, self.surface)?;
        let entry = checked_add(
            array_head,
            checked_mul(index, WORD, self.surface)?,
            self.surface,
        )?;
        let relative = self.usize_word(entry)?;
        if relative < table_bytes || relative % WORD != 0 {
            return Err(self.surface.malformed(format!(
                "invalid array element offset {relative} at index {index}"
            )));
        }
        let absolute = checked_add(array_head, relative, self.surface)?;
        self.word(absolute)?;
        Ok(absolute)
    }

    fn static_array(
        &self,
        base: usize,
        word_index: usize,
        minimum_head_words: usize,
        element_words: usize,
        maximum_count: usize,
        field: &'static str,
    ) -> Result<usize, AbiError> {
        let array = self.relative(base, word_index, minimum_head_words)?;
        let count = self.usize_word(array)?;
        if count > maximum_count {
            return Err(self
                .surface
                .malformed(format!("{field} count {count} exceeds {maximum_count}")));
        }
        let body = checked_add(array, WORD, self.surface)?;
        let words = checked_mul(count, element_words, self.surface)?;
        let end = checked_add(body, checked_mul(words, WORD, self.surface)?, self.surface)?;
        if end > self.data.len() {
            return Err(self
                .surface
                .malformed(format!("{field} count {count} exceeds calldata")));
        }
        Ok(count)
    }
}

fn checked_add(a: usize, b: usize, surface: Surface<'_>) -> Result<usize, AbiError> {
    a.checked_add(b)
        .ok_or_else(|| surface.malformed("ABI range addition overflow"))
}

fn checked_mul(a: usize, b: usize, surface: Surface<'_>) -> Result<usize, AbiError> {
    a.checked_mul(b)
        .ok_or_else(|| surface.malformed("ABI range multiplication overflow"))
}

fn padded_length(length: usize, surface: Surface<'_>) -> Result<usize, AbiError> {
    checked_add(length, WORD - 1, surface).map(|length| length / WORD * WORD)
}

fn preflight_ordinary_deposit(data: &[u8]) -> Result<(), AbiError> {
    let surface = Surface::new(DataSource::OrdinaryDepositData, data);
    if data.len() != ORDINARY_DEPOSIT_ENCODED_SIZE {
        return Err(surface.malformed(format!(
            "encoded deposit length {}, expected {ORDINARY_DEPOSIT_ENCODED_SIZE}",
            data.len()
        )));
    }
    let bounds = Bounds { surface, data };
    bounds.ensure_head(1)?;
    let deposit = bounds.relative(0, 0, 1)?;
    let encrypted = bounds.relative(deposit, 5, 6)?;
    let ciphertext = bounds.bytes_field(
        encrypted,
        2,
        5,
        ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
        "ciphertext",
    )?;
    if ciphertext.len() != ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE {
        return Err(surface.malformed(format!(
            "ciphertext length {}, expected {ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE}",
            ciphertext.len()
        )));
    }
    Ok(())
}

fn preflight_advance_tempo(calldata: &[u8]) -> Result<(), AbiError> {
    let surface = Surface::new(DataSource::AdvanceTempoCalldata, calldata);
    let bounds = Bounds::from_call(
        DataSource::AdvanceTempoCalldata,
        calldata,
        &IZoneInbox::advanceTempoCall::SELECTOR,
    )?;
    bounds.ensure_head(4)?;
    bounds.bytes_field(0, 0, 4, bounds.data.len(), "header")?;

    let (deposit_head, deposit_count) =
        bounds.dynamic_array(0, 1, 4, MAX_DEPOSITS_PER_TEMPO_BLOCK, "deposits")?;
    let mut ordinary_count = 0usize;
    for index in 0..deposit_count {
        let deposit = bounds.dynamic_element(deposit_head, deposit_count, index)?;
        let kind = bounds.usize_word(deposit)?;
        let data = bounds.bytes_field(deposit, 1, 2, bounds.data.len(), "depositData")?;
        match kind {
            0 => {
                if data.len() != 3 * WORD {
                    return Err(Surface::new(DataSource::WithdrawalBounceBackData, data)
                        .malformed(format!(
                            "withdrawal bounce-back depositData length {}, expected {}",
                            data.len(),
                            3 * WORD
                        )));
                }
            }
            1 => {
                ordinary_count += 1;
                preflight_ordinary_deposit(data)?;
            }
            other => {
                return Err(surface.malformed(format!("unsupported deposit discriminator {other}")));
            }
        }
    }

    let decryption_count =
        bounds.static_array(0, 2, 4, 4, MAX_DEPOSITS_PER_TEMPO_BLOCK, "decryptions")?;
    if decryption_count != ordinary_count {
        return Err(surface.malformed(format!(
                "decryption count {decryption_count} does not match ordinary deposit count {ordinary_count}"
            )));
    }

    let (token_head, token_count) =
        bounds.dynamic_array(0, 3, 4, MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK, "enabledTokens")?;
    for index in 0..token_count {
        let token = bounds.dynamic_element(token_head, token_count, index)?;
        bounds.bytes_field(token, 1, 4, MAX_TOKEN_NAME_BYTES, "token name")?;
        bounds.bytes_field(token, 2, 4, MAX_TOKEN_SYMBOL_BYTES, "token symbol")?;
        bounds.bytes_field(token, 3, 4, MAX_TOKEN_CURRENCY_BYTES, "token currency")?;
    }
    Ok(())
}

pub(crate) fn decode_advance_tempo(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedAdvanceTempo, ObservationError> {
    decode_advance_tempo_inner(calldata).map_err(|error| error.into_observation(transaction))
}

fn decode_advance_tempo_inner(calldata: &[u8]) -> Result<DecodedAdvanceTempo, AbiError> {
    let advance_surface = Surface::new(DataSource::AdvanceTempoCalldata, calldata);
    preflight_advance_tempo(calldata)?;
    let call = IZoneInbox::advanceTempoCall::abi_decode_validate(calldata)
        .map_err(|error| advance_surface.malformed(error))?;
    if call.abi_encode() != calldata {
        return Err(advance_surface.malformed("encoding is non-canonical or has trailing bytes"));
    }

    let header_surface = Surface::new(DataSource::AdvanceHeaderRlp, call.header.as_ref());
    let mut remaining = call.header.as_ref();
    let header =
        TempoHeader::decode(&mut remaining).map_err(|error| header_surface.malformed(error))?;
    if !remaining.is_empty() {
        return Err(header_surface.malformed(format!("{} trailing bytes", remaining.len())));
    }
    let mut canonical_header = Vec::with_capacity(header.length());
    header.encode(&mut canonical_header);
    if canonical_header.as_slice() != call.header.as_ref() {
        return Err(header_surface.malformed("non-canonical encoding"));
    }
    let imported_header = ImportedTempoHeader::new(header);

    let mut deposits = Vec::with_capacity(call.deposits.len());
    for queued in call.deposits {
        let deposit = match queued.depositType {
            IZoneInbox::DepositType::WithdrawalBounceBack => {
                let surface = Surface::new(
                    DataSource::WithdrawalBounceBackData,
                    queued.depositData.as_ref(),
                );
                let decoded = IZoneInbox::WithdrawalBounceBackDeposit::abi_decode_validate(
                    &queued.depositData,
                )
                .map_err(|error| surface.malformed(error))?;
                if decoded.abi_encode() != queued.depositData {
                    return Err(
                        surface.malformed("encoding is non-canonical or has trailing bytes")
                    );
                }
                ImportedDeposit {
                    kind: ImportedDepositKind::WithdrawalBounceBack(decoded),
                }
            }
            IZoneInbox::DepositType::Deposit => {
                let surface =
                    Surface::new(DataSource::OrdinaryDepositData, queued.depositData.as_ref());
                let decoded = ZonePortal::Deposit::abi_decode_validate(&queued.depositData)
                    .map_err(|error| surface.malformed(error))?;
                if decoded.abi_encode() != queued.depositData {
                    return Err(
                        surface.malformed("encoding is non-canonical or has trailing bytes")
                    );
                }
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

    Ok(DecodedAdvanceTempo {
        imported_header,
        deposits,
        decryptions: call.decryptions,
        enabled_tokens: call.enabledTokens,
    })
}

fn preflight_finalization(calldata: &[u8]) -> Result<(), AbiError> {
    let surface = Surface::new(DataSource::FinalizationCalldata, calldata);
    let bounds = Bounds::from_call(
        DataSource::FinalizationCalldata,
        calldata,
        &IZoneOutbox::finalizeWithdrawalBatchCall::SELECTOR,
    )?;
    bounds.ensure_head(3)?;
    let count = bounds.usize_word(0)?;
    let maximum = bounds.data.len() / WORD;
    let (sender_head, sender_count) = bounds.dynamic_array(0, 2, 3, maximum, "encryptedSenders")?;
    if count != sender_count {
        return Err(surface.malformed(format!(
            "count {count} does not match encryptedSenders length {sender_count}"
        )));
    }
    for index in 0..sender_count {
        let sender = bounds.dynamic_element(sender_head, sender_count, index)?;
        let bytes =
            bounds.direct_bytes(sender, AUTHENTICATED_WITHDRAWAL_SIZE, "encrypted sender")?;
        if !matches!(bytes.len(), 0 | AUTHENTICATED_WITHDRAWAL_SIZE) {
            return Err(surface.malformed(format!(
                    "encrypted sender {index} has length {}, expected 0 or {AUTHENTICATED_WITHDRAWAL_SIZE}",
                    bytes.len()
                )));
        }
    }
    Ok(())
}

pub(crate) fn decode_finalization(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedFinalization, ObservationError> {
    decode_finalization_inner(calldata).map_err(|error| error.into_observation(transaction))
}

fn decode_finalization_inner(calldata: &[u8]) -> Result<DecodedFinalization, AbiError> {
    let surface = Surface::new(DataSource::FinalizationCalldata, calldata);
    preflight_finalization(calldata)?;
    let call = IZoneOutbox::finalizeWithdrawalBatchCall::abi_decode_validate(calldata)
        .map_err(|error| surface.malformed(error))?;
    if call.abi_encode() != calldata {
        return Err(surface.malformed("encoding is non-canonical or has trailing bytes"));
    }
    let count =
        usize::try_from(call.count).map_err(|_| surface.malformed("count overflows usize"))?;
    Ok(DecodedFinalization {
        count,
        block_number: call.blockNumber,
        encrypted_senders: call.encryptedSenders,
    })
}

fn preflight_process_withdrawals(calldata: &[u8]) -> Result<(), AbiError> {
    let surface = Surface::new(DataSource::ProcessWithdrawalsCalldata, calldata);
    let bounds = Bounds::from_call(
        DataSource::ProcessWithdrawalsCalldata,
        calldata,
        &ZonePortal::processWithdrawalsCall::SELECTOR,
    )?;
    bounds.ensure_head(2)?;
    let maximum = bounds.data.len() / WORD;
    let (withdrawal_head, withdrawal_count) =
        bounds.dynamic_array(0, 0, 2, maximum, "withdrawals")?;
    for index in 0..withdrawal_count {
        let withdrawal = bounds.dynamic_element(withdrawal_head, withdrawal_count, index)?;
        bounds.bytes_field(withdrawal, 7, 9, MAX_CALLBACK_DATA_SIZE, "callbackData")?;
        let encrypted = bounds.bytes_field(
            withdrawal,
            8,
            9,
            AUTHENTICATED_WITHDRAWAL_SIZE,
            "encryptedSender",
        )?;
        if !matches!(encrypted.len(), 0 | AUTHENTICATED_WITHDRAWAL_SIZE) {
            return Err(surface.malformed(format!(
                    "encryptedSender {index} has length {}, expected 0 or {AUTHENTICATED_WITHDRAWAL_SIZE}",
                    encrypted.len()
                )));
        }
    }
    Ok(())
}

fn preflight_submit_batch(calldata: &[u8]) -> Result<(), AbiError> {
    let bounds = Bounds::from_call(
        DataSource::SubmitBatchCalldata,
        calldata,
        &ZonePortal::submitBatchCall::SELECTOR,
    )?;
    bounds.ensure_head(13)?;
    let maximum = bounds.data.len();
    bounds.bytes_field(0, 9, 13, maximum, "verifierConfig")?;
    bounds.bytes_field(0, 10, 13, maximum, "proof")?;
    let (signature_head, signature_count) =
        bounds.dynamic_array(0, 12, 13, MAX_SEQUENCERS, "signatures")?;
    for index in 0..signature_count {
        let signature = bounds.dynamic_element(signature_head, signature_count, index)?;
        bounds.direct_bytes(signature, maximum, "signature")?;
    }
    Ok(())
}

pub(crate) fn decode_portal_call(
    calldata: &[u8],
    transaction: AuthenticatedTransaction,
) -> Result<DecodedPortalCall, ObservationError> {
    decode_portal_call_inner(calldata).map_err(|error| error.into_observation(transaction))
}

fn decode_portal_call_inner(calldata: &[u8]) -> Result<DecodedPortalCall, AbiError> {
    if calldata.starts_with(&ZonePortal::submitBatchCall::SELECTOR) {
        let surface = Surface::new(DataSource::SubmitBatchCalldata, calldata);
        preflight_submit_batch(calldata)?;
        let call = ZonePortal::submitBatchCall::abi_decode_validate(calldata)
            .map_err(|error| surface.malformed(error))?;
        if call.abi_encode() != calldata {
            return Err(surface.malformed("encoding is non-canonical or has trailing bytes"));
        }
        Ok(DecodedPortalCall {
            kind: DecodedPortalCallKind::SubmitBatch(Box::new(call)),
        })
    } else if calldata.starts_with(&ZonePortal::processWithdrawalsCall::SELECTOR) {
        let surface = Surface::new(DataSource::ProcessWithdrawalsCalldata, calldata);
        preflight_process_withdrawals(calldata)?;
        let call = ZonePortal::processWithdrawalsCall::abi_decode_validate(calldata)
            .map_err(|error| surface.malformed(error))?;
        if call.abi_encode() != calldata {
            return Err(surface.malformed("encoding is non-canonical or has trailing bytes"));
        }
        Ok(DecodedPortalCall {
            kind: DecodedPortalCallKind::ProcessWithdrawals(call),
        })
    } else {
        Err(
            Surface::new(DataSource::PortalTransactionCalldata, calldata)
                .malformed("selector does not match its authenticated protocol events"),
        )
    }
}

#[cfg(test)]
mod tests;
