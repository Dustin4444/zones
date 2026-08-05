//! Cross-layer invariant checks for the Zone checker.
//!
//! Currently a single invariant: for each anchored block, the ordered token
//! addresses from `ZonePortal.TokenEnabled` events on L1 must exactly match
//! the ordered `ZoneInbox.TokenEnabled` events on L2.

use alloy_eips::BlockNumHash;
use alloy_primitives::Address;
use tracing::{debug, info, warn};

use crate::l1_facts::L1BlockFacts;
use crate::l2_facts::L2BlockFacts;

/// Result of comparing the ordered L1 and L2 token-enabled sequences.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TokenEnabledCheck {
    /// Both layers emitted the same token addresses in the same order.
    Pass,
    /// The sequences differ. L1 is expected because it is the source of the
    /// `advanceTempo` input; L2 is the independently observed output.
    Mismatch {
        expected: Vec<Address>,
        observed: Vec<Address>,
    },
}

/// Compare the ordered token-enabled sequences without producing side effects.
fn evaluate_token_enabled(l1_tokens: &[Address], l2_tokens: &[Address]) -> TokenEnabledCheck {
    if l1_tokens == l2_tokens {
        TokenEnabledCheck::Pass
    } else {
        TokenEnabledCheck::Mismatch {
            expected: l1_tokens.to_vec(),
            observed: l2_tokens.to_vec(),
        }
    }
}

/// Evaluate the token-enabled cross-layer invariant for one block and log its result.
///
/// Violations are logged but do not return an error — this is observe-only.
pub(crate) fn check_token_enabled_invariant(
    l1_facts: &L1BlockFacts,
    l2_facts: &L2BlockFacts,
    l2_block: BlockNumHash,
    l1_block: BlockNumHash,
) {
    let l1_tokens = l1_facts.token_enabled_addresses();
    let l2_tokens = l2_facts.token_enabled_addresses();

    match evaluate_token_enabled(&l1_tokens, &l2_tokens) {
        TokenEnabledCheck::Pass if l1_tokens.is_empty() => {
            debug!(
                target: "zone::checker",
                l2_block_number = l2_block.number,
                l1_block_number = l1_block.number,
                "Token-enabled invariant passed",
            );
        }
        TokenEnabledCheck::Pass => {
            info!(
                target: "zone::checker",
                invariant = "token_enabled",
                l2_block_number = l2_block.number,
                l2_block_hash = %l2_block.hash,
                l1_block_number = l1_block.number,
                l1_block_hash = %l1_block.hash,
                token_count = l1_tokens.len(),
                "Token-enabled invariant passed",
            );
        }
        TokenEnabledCheck::Mismatch { expected, observed } => {
            warn!(
                target: "zone::checker",
                invariant = "token_enabled",
                l2_block_number = l2_block.number,
                l2_block_hash = %l2_block.hash,
                l1_block_number = l1_block.number,
                l1_block_hash = %l1_block.hash,
                ?expected,
                ?observed,
                "Token-enabled invariant violated",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_A: Address = Address::repeat_byte(0xa1);
    const TOKEN_B: Address = Address::repeat_byte(0xb2);

    #[test]
    fn exact_matches_pass() {
        for (expected, observed) in [
            (&[][..], &[][..]),
            (&[TOKEN_A][..], &[TOKEN_A][..]),
            (&[TOKEN_A, TOKEN_B][..], &[TOKEN_A, TOKEN_B][..]),
        ] {
            assert_eq!(
                evaluate_token_enabled(expected, observed),
                TokenEnabledCheck::Pass
            );
        }
    }

    #[test]
    fn mismatches_preserve_expected_and_observed_sequences() {
        for (expected, observed) in [
            (&[TOKEN_A][..], &[][..]),
            (&[][..], &[TOKEN_A][..]),
            (&[TOKEN_A][..], &[TOKEN_B][..]),
            (&[TOKEN_A][..], &[TOKEN_A, TOKEN_A][..]),
            (&[TOKEN_A, TOKEN_B][..], &[TOKEN_B, TOKEN_A][..]),
        ] {
            assert_eq!(
                evaluate_token_enabled(expected, observed),
                TokenEnabledCheck::Mismatch {
                    expected: expected.to_vec(),
                    observed: observed.to_vec(),
                }
            );
        }
    }
}
