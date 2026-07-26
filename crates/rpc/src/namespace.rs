//! Public `zone_` JSON-RPC namespace.

use std::sync::{Arc, OnceLock};

use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{ErrorObjectOwned, error::INTERNAL_ERROR_CODE},
};
use serde_json::value::RawValue;
use tokio::sync::watch;

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
    api: Deferred<dyn ZoneRpcApi>,
}

impl PublicZoneRpc {
    /// Install the shared Zone RPC implementation.
    pub fn set_api(&self, api: Arc<dyn ZoneRpcApi>) -> Result<(), Arc<dyn ZoneRpcApi>> {
        self.api.set(api)
    }
}

#[jsonrpsee::core::async_trait]
impl ZoneApiServer for PublicZoneRpc {
    async fn get_zone_info(&self) -> RpcResult<Box<RawValue>> {
        self.api
            .get()
            .await
            .zone_get_zone_info()
            .await
            .map_err(Into::into)
    }

    async fn get_encryption_key(&self) -> RpcResult<Box<RawValue>> {
        self.api
            .get()
            .await
            .zone_get_encryption_key()
            .await
            .map_err(Into::into)
    }
}

/// A value installed once after the RPC servers have begun initializing.
///
/// Keeping readiness separate from the value preserves [`OnceLock`]'s atomic
/// set-once behavior while allowing early requests to wait instead of observing
/// a transient initialization error.
struct Deferred<T: ?Sized> {
    value: Arc<OnceLock<Arc<T>>>,
    ready: watch::Sender<bool>,
}

impl<T: ?Sized> Deferred<T> {
    fn set(&self, value: Arc<T>) -> Result<(), Arc<T>> {
        self.value.set(value)?;
        self.ready.send_replace(true);
        Ok(())
    }

    async fn get(&self) -> Arc<T> {
        if let Some(value) = self.value.get() {
            return value.clone();
        }

        let mut ready = self.ready.subscribe();
        ready
            .wait_for(|ready| *ready)
            .await
            .expect("deferred value retains the readiness sender");
        self.value
            .get()
            .expect("readiness is signalled after installing the value")
            .clone()
    }
}

impl<T: ?Sized> Clone for Deferred<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            ready: self.ready.clone(),
        }
    }
}

impl<T: ?Sized> Default for Deferred<T> {
    fn default() -> Self {
        let (ready, _) = watch::channel(false);
        Self {
            value: Arc::new(OnceLock::new()),
            ready,
        }
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

    #[tokio::test]
    async fn early_requests_wait_for_initialization() {
        let value = Deferred::<u64>::default();
        let mut waiter = tokio::spawn({
            let value = value.clone();
            async move { *value.get().await }
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiter)
                .await
                .is_err()
        );

        value.set(Arc::new(42)).unwrap();
        assert_eq!(waiter.await.unwrap(), 42);
    }
}
