//! `ModelRouter` as the host's [`IdorisSlot`]. Lives here because
//! `agent24-models` already depends on `agent24-idoris`.
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use agent24_idoris::managed_idoris::{
    ConnectionCheck, GatewayLease, IdorisSlot, ModelError as HostError, PreparedGateway,
};

use crate::IdorisGateway;
use crate::router::ModelRouter;

fn host_error(error: crate::ModelError) -> HostError {
    use crate::ModelError as E;
    match error {
        E::Unavailable(message) => HostError::Unavailable(message),
        E::Provider(message) => HostError::Provider(message),
        E::Cancelled => HostError::Cancelled,
        // `Rejected` displays as `provider error: {message}`.
        E::Rejected { message, .. } => HostError::Provider(message),
        other => HostError::Other(other.to_string()),
    }
}

fn built_gateway(gateway: PreparedGateway) -> IdorisGateway {
    match gateway.downcast::<IdorisGateway>() {
        Ok(gateway) => gateway,
        Err(_) => panic!("iDoris slot received a gateway it did not build"),
    }
}

fn built_gateway_ref(gateway: &PreparedGateway) -> &IdorisGateway {
    match gateway.downcast_ref::<IdorisGateway>() {
        Some(gateway) => gateway,
        None => panic!("iDoris slot received a gateway it did not build"),
    }
}

#[async_trait]
impl IdorisSlot for ModelRouter {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn block_idoris(&self, reason: &str) {
        ModelRouter::block_idoris(self, reason);
    }

    fn replace_idoris(&self, gateway: PreparedGateway) {
        ModelRouter::replace_idoris(self, built_gateway(gateway));
    }

    fn published_lease(&self) -> Option<GatewayLease> {
        let gateway = self.idoris_gateway().ok()?;
        let weak = Arc::downgrade(&gateway);
        drop(gateway);
        Some(GatewayLease::new(move || weak.strong_count()))
    }

    async fn open_gateway(
        &self,
        url: &str,
        secret: Zeroizing<String>,
        cancel: &CancellationToken,
    ) -> Result<PreparedGateway, HostError> {
        let gateway = IdorisGateway::from_credentials(url, secret).map_err(HostError::Provider)?;
        gateway.check_health(cancel).await.map_err(host_error)?;
        Ok(PreparedGateway::new(gateway))
    }

    async fn verify_connection(
        &self,
        gateway: &PreparedGateway,
        headroom_confirmed: bool,
        cancel: &CancellationToken,
    ) -> Result<ConnectionCheck, HostError> {
        let verification = built_gateway_ref(gateway)
            .verify_connection(headroom_confirmed, cancel)
            .await
            .map_err(host_error)?;
        Ok(ConnectionCheck {
            model_id: verification.model_id,
            record_id: verification.record_id,
            locality: verification.locality,
        })
    }
}
