//! Primitive types used by the zone.
//!
//! This crate is `no_std` compatible so it can be used inside SP1 (RISC-V) guest
//! programs and TEE enclaves, as well as in the host-side prover.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod constants;

/// Return the L1 genesis anchor that makes a fresh zone replay its portal-creation block.
///
/// The creation block must come from the confirmed `createZone` receipt. Sampling the L1 head
/// before submission is unsafe because transaction inclusion can be delayed by one or more blocks.
pub const fn portal_creation_anchor(creation_block_number: u64) -> Option<u64> {
    creation_block_number.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::portal_creation_anchor;

    #[test]
    fn delayed_portal_creation_replays_the_creation_block_first() {
        let pre_submit_head = 100;
        let creation_block = 105;

        let anchor = portal_creation_anchor(creation_block).expect("non-genesis creation block");

        assert!(creation_block > pre_submit_head + 1);
        assert_eq!(anchor + 1, creation_block);
        assert_ne!(anchor, pre_submit_head);
    }
}
