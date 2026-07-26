//! Public `zone_` JSON-RPC namespace.

use std::sync::{Arc, OnceLock};

use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{ErrorObjectOwned, error::INTERNAL_ERROR_CODE},
};
use serde_json::value::RawValue;

use crate::{ZoneRpcApi, types::JsonRpcError};

/// Public, authentication-independent Zone RPC methods.
#[rpc(server, namespace = "zone")]
pub trait ZoneApi {
    /// Returns metadata for this Zone.
    #[method(name = "getZoneInfo")]
    async fn get_zone_info(&self) -> RpcResult<Box<RawValue>>;

    /// Returns the encryption key active at the current Tempo L1 head.
    #[method(name = "getEncryptionKey")]
    async fn get_encryption_key(&self) -> RpcResult<Box<RawValue>>;
}

/// Deferred public namespace backed by the same API as the private RPC server.
///
/// Reth configures RPC modules before returning the handles needed to build the
/// private [`ZoneRpcApi`]. This handle lets the add-ons register the namespace
/// up front and install the shared implementation immediately afterward.
#[derive(Clone, Default)]
pub struct PublicZoneRpc {
    api: Arc<OnceLock<Arc<dyn ZoneRpcApi>>>,
}

impl PublicZoneRpc {
    /// Install the shared Zone RPC implementation.
    pub fn set_api(&self, api: Arc<dyn ZoneRpcApi>) -> Result<(), Arc<dyn ZoneRpcApi>> {
        self.api.set(api)
    }

    fn api(&self) -> RpcResult<&dyn ZoneRpcApi> {
        self.api.get().map(AsRef::as_ref).ok_or_else(|| {
            ErrorObjectOwned::owned(INTERNAL_ERROR_CODE, "zone RPC is initializing", None::<()>)
        })
    }
}

#[jsonrpsee::core::async_trait]
impl ZoneApiServer for PublicZoneRpc {
    async fn get_zone_info(&self) -> RpcResult<Box<RawValue>> {
        self.api()?.zone_get_zone_info().await.map_err(Into::into)
    }

    async fn get_encryption_key(&self) -> RpcResult<Box<RawValue>> {
        self.api()?
            .zone_get_encryption_key()
            .await
            .map_err(Into::into)
    }
}

impl From<JsonRpcError> for ErrorObjectOwned {
    fn from(error: JsonRpcError) -> Self {
        let code = i32::try_from(error.code).unwrap_or(INTERNAL_ERROR_CODE);
        Self::owned(code, error.message, error.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_authentication_independent_methods() {
        let methods = PublicZoneRpc::default().into_rpc();
        let names = methods.method_names().collect::<Vec<_>>();

        assert!(names.contains(&"zone_getZoneInfo"));
        assert!(names.contains(&"zone_getEncryptionKey"));
        assert!(!names.contains(&"zone_getAuthorizationTokenInfo"));
    }

    #[test]
    fn out_of_range_private_error_code_becomes_internal_error() {
        let error = ErrorObjectOwned::from(JsonRpcError {
            code: i64::MAX,
            message: "invalid code".to_string(),
            data: None,
        });

        assert_eq!(error.code(), INTERNAL_ERROR_CODE);
        assert_eq!(error.message(), "invalid code");
    }
}
