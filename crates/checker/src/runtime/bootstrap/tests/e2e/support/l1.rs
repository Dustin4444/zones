use alloy_primitives::{Address, B256, Bytes, FixedBytes, Log, U256, keccak256};
use alloy_sol_types::{SolEvent, SolValue as _};
use tempo_zone_contracts::{IZoneInbox, ZonePortal};

use super::{DEPOSIT_AMOUNT, ZONE_ID};
use crate::model::{
    constants::ZONE_FACTORY_ADDRESS,
    events::{Factory, Portal},
};

pub(super) fn creation_logs(portal: Address, initial_token: Address) -> Vec<Log> {
    vec![
        protocol_log(
            portal,
            Portal::TokenEnabled {
                token: initial_token,
                name: "Initial Token".into(),
                symbol: "INIT".into(),
                currency: "USD".into(),
            },
        ),
        protocol_log(
            ZONE_FACTORY_ADDRESS,
            Factory::ZoneCreated {
                zoneId: ZONE_ID,
                portal,
                initialToken: initial_token,
                accessMode: false,
                gatewayMode: false,
                admin: Address::repeat_byte(0xa1),
                sequencers: vec![Address::repeat_byte(0xa2)],
                threshold: 1,
                verifier: Address::repeat_byte(0xa3),
            },
        ),
    ]
}

pub(super) fn protocol_log<E: SolEvent>(address: Address, event: E) -> Log {
    Log {
        address,
        data: event.encode_log_data(),
    }
}

pub(super) fn ordinary_deposit(token: Address) -> ZonePortal::Deposit {
    ZonePortal::Deposit {
        token,
        sender: Address::repeat_byte(0x41),
        amount: DEPOSIT_AMOUNT,
        tempoRefundRecipient: Address::repeat_byte(0x42),
        keyIndex: U256::from(3),
        encrypted: ZonePortal::DepositPayload {
            ephemeralPubkeyX: B256::repeat_byte(0x43),
            ephemeralPubkeyYParity: 3,
            ciphertext: Bytes::from(vec![0x44; 64]),
            nonce: FixedBytes::repeat_byte(0x45),
            tag: FixedBytes::repeat_byte(0x46),
        },
    }
}

pub(super) fn ordinary_queue_hash(deposit: &ZonePortal::Deposit) -> B256 {
    keccak256(
        (
            IZoneInbox::DepositType::Deposit,
            deposit.clone(),
            B256::ZERO,
        )
            .abi_encode_params(),
    )
}
