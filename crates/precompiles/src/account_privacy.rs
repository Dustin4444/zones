//! Shared authorization for account-indexed private reads.

use alloy_primitives::Address;
use alloy_sol_types::SolError;
use tempo_zone_contracts::Unauthorized;

use crate::{
    execution::{CallCheck, CallRuleError},
    storage::{L1State, L1StorageReader},
};

#[derive(Clone)]
pub(crate) struct AccountPrivacy<P> {
    l1: L1State<P>,
}

impl<P> AccountPrivacy<P> {
    pub(crate) fn new(l1: L1State<P>) -> Self {
        Self { l1 }
    }
}

impl<P: L1StorageReader> AccountPrivacy<P> {
    pub(crate) fn authorize(&self, caller: Address, accounts: &[Address]) -> CallCheck {
        if accounts.contains(&caller) {
            return CallCheck::Continue;
        }

        match self.l1.read_portal(|portal| &portal.is_sequencer[caller]) {
            Ok(true) => CallCheck::Continue,
            Ok(false) => CallCheck::Revert(Unauthorized {}.abi_encode().into()),
            Err(error) => CallCheck::Error(CallRuleError::Tempo(error)),
        }
    }
}
