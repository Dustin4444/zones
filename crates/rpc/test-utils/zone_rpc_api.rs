use std::{collections::HashMap, sync::Arc};

use alloy_primitives::{Address, B256, Bytes, b256};
use alloy_rpc_types_eth::{
    BlockId, BlockNumberOrTag, Filter, FilterId, state::StateOverride,
};
use futures::stream;
use parking_lot::Mutex;
use tempo_alloy::rpc::TempoTransactionRequest;
use tempo_contracts::precompiles::account_keychain::IAccountKeychain::{
    KeyInfo, SignatureType as KeyInfoSignatureType,
};
use tokio::sync::Notify;

use rpc::{
    auth::AuthContext,
    handlers::ZoneRpcApi,
    subscription::{BoxWsSubscriptionFut, WsSubscriptionStream},
    types::{BoxEyreFut, BoxFut, JsonRpcError, to_raw},
};

#[derive(Clone, Copy, Default)]
enum ResponseProfile {
    #[default]
    Stub,
    Handler,
    WebSocket,
}

/// Reusable [`ZoneRpcApi`] fixture shared by unit and integration tests.
///
/// Profiles keep each suite's successful responses explicit while all other
/// methods retain the common "not implemented" failure behavior.
#[derive(Default)]
pub(super) struct TestZoneRpcApi {
    profile: ResponseProfile,
    key_infos: Mutex<HashMap<(Address, Address), KeyInfo>>,
    key_lookup_error: Option<&'static str>,
    ws_subscriptions_enabled: bool,
    blocking_rpc_started: Option<Arc<Notify>>,
}

impl TestZoneRpcApi {
    pub(super) fn for_handler_tests() -> Self {
        Self {
            profile: ResponseProfile::Handler,
            ..Self::default()
        }
    }

    pub(super) fn for_websocket_tests() -> Self {
        Self {
            profile: ResponseProfile::WebSocket,
            ..Self::default()
        }
    }

    pub(super) fn with_key_info(account: Address, key_id: Address, key_info: KeyInfo) -> Self {
        let api = Self::default();
        api.key_infos.lock().insert((account, key_id), key_info);
        api
    }

    pub(super) fn with_key(
        account: Address,
        key_id: Address,
        signature_type: KeyInfoSignatureType,
    ) -> Self {
        let api = Self::for_websocket_tests();
        api.key_infos.lock().insert(
            (account, key_id),
            KeyInfo {
                signatureType: signature_type,
                keyId: key_id,
                expiry: u64::MAX,
                enforceLimits: false,
                isRevoked: false,
            },
        );
        api
    }

    pub(super) fn with_key_and_blocking_request(
        account: Address,
        key_id: Address,
        signature_type: KeyInfoSignatureType,
    ) -> (Self, Arc<Notify>) {
        let mut api = Self::with_key(account, key_id, signature_type);
        let started = Arc::new(Notify::new());
        api.blocking_rpc_started = Some(started.clone());
        (api, started)
    }

    pub(super) fn with_key_lookup_error(message: &'static str) -> Self {
        Self {
            profile: ResponseProfile::WebSocket,
            key_lookup_error: Some(message),
            ..Self::default()
        }
    }

    pub(super) fn with_ws_subscriptions() -> Self {
        Self {
            profile: ResponseProfile::WebSocket,
            ws_subscriptions_enabled: true,
            ..Self::default()
        }
    }

    pub(super) fn revoke_key(&self, account: Address, key_id: Address) {
        if let Some(key_info) = self.key_infos.lock().get_mut(&(account, key_id)) {
            key_info.isRevoked = true;
        }
    }

    fn unimplemented(&self) -> BoxFut<'_> {
        Box::pin(async { Err(JsonRpcError::internal("not implemented")) })
    }
}

macro_rules! stub {
    ($method:ident $(, $arg:ident : $ty:ty)*) => {
        fn $method(&self $(, $arg: $ty)*) -> BoxFut<'_> {
            self.unimplemented()
        }
    };
}

impl ZoneRpcApi for TestZoneRpcApi {
    fn get_keychain_key(&self, account: Address, key_id: Address) -> BoxEyreFut<'_, KeyInfo> {
        if matches!(self.profile, ResponseProfile::Handler) {
            return Box::pin(async { Err(eyre::eyre!("not implemented")) });
        }
        if let Some(message) = self.key_lookup_error {
            return Box::pin(async move { Err(eyre::eyre!(message)) });
        }

        let key_info = self
            .key_infos
            .lock()
            .get(&(account, key_id))
            .cloned()
            .unwrap_or(KeyInfo {
                signatureType: KeyInfoSignatureType::Secp256k1,
                keyId: Address::ZERO,
                expiry: 0,
                enforceLimits: false,
                isRevoked: false,
            });
        Box::pin(async move { Ok(key_info) })
    }

    fn block_number(&self) -> BoxFut<'_> {
        if matches!(self.profile, ResponseProfile::WebSocket) {
            Box::pin(async { to_raw(&"0x42") })
        } else {
            self.unimplemented()
        }
    }

    fn chain_id(&self) -> BoxFut<'_> {
        if matches!(self.profile, ResponseProfile::WebSocket) {
            Box::pin(async { to_raw(&"0x1") })
        } else {
            self.unimplemented()
        }
    }

    stub!(net_version);

    fn syncing(&self) -> BoxFut<'_> {
        if matches!(self.profile, ResponseProfile::Handler) {
            Box::pin(async { to_raw(&false) })
        } else {
            self.unimplemented()
        }
    }

    fn coinbase(&self) -> BoxFut<'_> {
        if matches!(self.profile, ResponseProfile::Handler) {
            Box::pin(async { to_raw(&Address::repeat_byte(0xbb)) })
        } else {
            self.unimplemented()
        }
    }

    stub!(gas_price);
    stub!(max_priority_fee_per_gas);
    stub!(fee_history, _block_count: u64, _newest_block: BlockNumberOrTag, _reward_percentiles: Option<Vec<f64>>);
    stub!(get_balance, _address: Address, _block: Option<BlockId>, _auth: AuthContext);
    stub!(get_transaction_count, _address: Address, _block: Option<BlockId>, _auth: AuthContext);
    stub!(block_by_number, _number: BlockNumberOrTag, _full: bool, _auth: AuthContext);
    stub!(block_by_hash, _hash: B256, _full: bool, _auth: AuthContext);
    stub!(transaction_by_hash, _hash: B256, _auth: AuthContext);
    stub!(transaction_receipt, _hash: B256, _auth: AuthContext);
    stub!(call, _request: TempoTransactionRequest, _block: Option<BlockId>, _state_override: Option<StateOverride>, _auth: AuthContext);
    stub!(estimate_gas, _request: TempoTransactionRequest, _block: Option<BlockId>, _state_override: Option<StateOverride>, _auth: AuthContext);
    stub!(send_raw_transaction, _data: Bytes, _auth: AuthContext);

    fn send_raw_transaction_sync(&self, _data: Bytes, _auth: AuthContext) -> BoxFut<'_> {
        let started = self.blocking_rpc_started.clone();
        Box::pin(async move {
            if let Some(started) = started {
                started.notify_one();
                std::future::pending::<()>().await;
            }
            Err(JsonRpcError::internal("not implemented"))
        })
    }

    stub!(fill_transaction, _request: TempoTransactionRequest, _auth: AuthContext);
    stub!(get_logs, _filter: Filter, _auth: AuthContext);
    stub!(new_filter, _filter: Filter, _auth: AuthContext);
    stub!(get_filter_logs, _id: FilterId, _auth: AuthContext);
    stub!(get_filter_changes, _id: FilterId, _auth: AuthContext);
    stub!(new_block_filter, _auth: AuthContext);
    stub!(uninstall_filter, _id: FilterId, _auth: AuthContext);

    fn ws_subscribe_new_heads(&self, _auth: AuthContext) -> BoxWsSubscriptionFut<'_> {
        let enabled = self.ws_subscriptions_enabled;
        Box::pin(async move {
            if !enabled {
                return Err(JsonRpcError::method_disabled());
            }

            let stream = stream::iter(vec![to_raw(&serde_json::json!({
                "hash": format!(
                    "{:#x}",
                    b256!("0x4444444444444444444444444444444444444444444444444444444444444444")
                ),
                "number": "0x42",
                "parentHash": format!("{:#x}", B256::ZERO),
                "logsBloom": format!("0x{}", "0".repeat(512)),
                "gasUsed": "0x0",
                "size": "0x0",
                "transactionsRoot": format!("{:#x}", B256::ZERO),
                "receiptsRoot": format!("{:#x}", B256::ZERO),
                "stateRoot": format!("{:#x}", B256::ZERO),
                "extraData": "0x",
            }))]);
            let stream: WsSubscriptionStream = Box::pin(stream);
            Ok(stream)
        })
    }

    fn ws_subscribe_logs(
        &self,
        _filter: Filter,
        _auth: AuthContext,
    ) -> BoxWsSubscriptionFut<'_> {
        let enabled = self.ws_subscriptions_enabled;
        Box::pin(async move {
            if !enabled {
                return Err(JsonRpcError::method_disabled());
            }

            let stream = stream::iter(vec![to_raw(&serde_json::json!({
                "address": format!("{:#x}", Address::ZERO),
                "topics": [format!(
                    "{:#x}",
                    b256!("0x1111111111111111111111111111111111111111111111111111111111111111")
                )],
                "data": "0x",
                "blockHash": format!(
                    "{:#x}",
                    b256!("0x2222222222222222222222222222222222222222222222222222222222222222")
                ),
                "blockNumber": "0x42",
                "transactionHash": format!(
                    "{:#x}",
                    b256!("0x3333333333333333333333333333333333333333333333333333333333333333")
                ),
                "transactionIndex": "0x0",
                "logIndex": "0x0",
                "removed": false,
            }))]);
            let stream: WsSubscriptionStream = Box::pin(stream);
            Ok(stream)
        })
    }

    fn zone_get_authorization_token_info(&self, auth: AuthContext) -> BoxFut<'_> {
        if matches!(
            self.profile,
            ResponseProfile::Handler | ResponseProfile::WebSocket
        ) {
            Box::pin(async move {
                to_raw(&serde_json::json!({
                    "account": auth.caller,
                    "expiresAt": alloy_primitives::U64::from(auth.expires_at),
                }))
            })
        } else {
            self.unimplemented()
        }
    }

    fn zone_get_zone_info(&self, _auth: AuthContext) -> BoxFut<'_> {
        match self.profile {
            ResponseProfile::Handler => Box::pin(async {
                to_raw(&serde_json::json!({
                    "zoneId": "0x1",
                    "zoneTokens": [format!("{:#x}", Address::repeat_byte(0x11))],
                    "sequencers": [format!("{:#x}", Address::repeat_byte(0x22))],
                    "chainId": "0x2a",
                    "tempoBlockNumber": "0x7",
                }))
            }),
            ResponseProfile::WebSocket => Box::pin(async {
                to_raw(&serde_json::json!({
                    "zoneId": "0x1",
                    "zoneTokens": [format!("{:#x}", Address::repeat_byte(0x11))],
                    "chainId": "0x2a",
                }))
            }),
            ResponseProfile::Stub => self.unimplemented(),
        }
    }

    stub!(zone_get_encryption_key, _auth: AuthContext);
}
