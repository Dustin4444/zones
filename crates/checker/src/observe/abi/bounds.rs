//! Allocation-bounded ABI surface navigation.

use alloy_primitives::U256;

use super::{AbiError, SELECTOR_LEN, Surface, WORD};
use crate::observe::error::DataSource;

/// A checked ABI view that never uses calldata lengths before validating bounds.
pub(super) struct Bounds<'a> {
    surface: Surface<'a>,
    pub(super) data: &'a [u8],
}

impl<'a> Bounds<'a> {
    pub(super) const fn from_data(surface: Surface<'a>, data: &'a [u8]) -> Self {
        Self { surface, data }
    }

    pub(super) fn from_call(
        source: DataSource,
        calldata: &'a [u8],
        selector: &[u8; SELECTOR_LEN],
    ) -> Result<Self, AbiError> {
        let surface = Surface::new(source, calldata);
        if !calldata.starts_with(selector) {
            return Err(surface.malformed("wrong function selector"));
        }
        Ok(Self {
            surface,
            data: &calldata[SELECTOR_LEN..],
        })
    }

    pub(super) fn ensure_head(&self, words: usize) -> Result<(), AbiError> {
        let bytes = checked_mul(words, WORD, self.surface)?;
        if self.data.len() < bytes {
            return Err(self.surface.malformed(format!(
                "ABI head needs {bytes} bytes, got {}",
                self.data.len()
            )));
        }
        Ok(())
    }

    pub(super) fn word(&self, offset: usize) -> Result<&'a [u8], AbiError> {
        let end = checked_add(offset, WORD, self.surface)?;
        self.data.get(offset..end).ok_or_else(|| {
            self.surface
                .malformed(format!("word at byte {offset} is truncated"))
        })
    }

    pub(super) fn usize_word(&self, offset: usize) -> Result<usize, AbiError> {
        let value = U256::from_be_slice(self.word(offset)?);
        usize::try_from(value).map_err(|_| {
            self.surface
                .malformed(format!("word at byte {offset} does not fit usize"))
        })
    }

    pub(super) fn relative(
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

    pub(super) fn bytes_field(
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
    pub(super) fn direct_bytes(
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

    pub(super) fn dynamic_array(
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

    pub(super) fn dynamic_element(
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

    pub(super) fn static_array(
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
