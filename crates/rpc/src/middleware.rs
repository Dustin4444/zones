//! Access policy and metrics for the authenticated RPC surface.

use std::{future::Future, time::Instant};

use jsonrpsee::server::{
    BatchResponseBuilder, MethodResponse,
    middleware::rpc::{Batch, BatchEntry, Notification, Request, RpcServiceT},
};
use tower::Layer;

use crate::{
    metrics::RedactedRpcCallMetrics,
    server::{MAX_RPC_MESSAGE_SIZE, RpcTransport},
    types::{JsonRpcError, MethodTier, classify_method},
};

/// Applies the redacted RPC allowlist before invoking registered methods.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedactedRpcLayer;

impl<S> Layer<S> for RedactedRpcLayer {
    type Service = RedactedRpcMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        RedactedRpcMiddleware { service }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RedactedRpcMiddleware<S> {
    service: S,
}

impl<S> RedactedRpcMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    async fn call_one<'a>(&self, req: Request<'a>) -> MethodResponse {
        let method = req.method_name().to_owned();
        let metrics = RedactedRpcCallMetrics::new_for(&method);
        let started_at = Instant::now();
        metrics.started_total.increment(1);

        let transport = req.extensions().get::<RpcTransport>().copied();
        let denied = match classify_method(&method) {
            Some(MethodTier::Public) => None,
            Some(MethodTier::Restricted) => Some(JsonRpcError::sequencer_only()),
            Some(MethodTier::Disabled)
                if transport == Some(RpcTransport::WebSocket)
                    && matches!(method.as_str(), "eth_subscribe" | "eth_unsubscribe") =>
            {
                None
            }
            Some(MethodTier::Disabled) => Some(JsonRpcError::method_disabled()),
            None => None,
        };

        let response = if let Some(error) = denied {
            MethodResponse::error(req.id, jsonrpsee::types::ErrorObjectOwned::from(error))
                .with_extensions(req.extensions)
        } else {
            self.service.call(req).await
        };

        metrics
            .time_seconds
            .record(started_at.elapsed().as_secs_f64());
        if response.is_error() {
            metrics.failed_total.increment(1);
        } else {
            metrics.successful_total.increment(1);
        }
        response
    }
}

impl<S> RpcServiceT for RedactedRpcMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let service = self.clone();
        async move { service.call_one(req).await }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let service = self.clone();
        async move {
            let mut responses = BatchResponseBuilder::new_with_limit(MAX_RPC_MESSAGE_SIZE as usize);
            let mut got_notification = false;

            for entry in batch {
                match entry {
                    Ok(BatchEntry::Call(req)) => {
                        if let Err(error) = responses.append(service.call_one(req).await) {
                            return error;
                        }
                    }
                    Ok(BatchEntry::Notification(notification)) => {
                        got_notification = true;
                        service.notification(notification).await;
                    }
                    Err(error) => {
                        let (error, id) = error.into_parts();
                        if let Err(error) = responses.append(MethodResponse::error(id, error)) {
                            return error;
                        }
                    }
                }
            }

            if responses.is_empty() && got_notification {
                MethodResponse::notification()
            } else {
                MethodResponse::from_batch(responses.finish())
            }
        }
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.service.notification(notification)
    }
}
