//! Test build of this crate is a different crate from the one `agent24-models`
//! implements `IdorisSlot` for (dev-dependency cycle). Unit tests need their own impl.
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::managed_idoris::{
    ConnectionCheck, GatewayLease, IdorisSlot, ModelError as HostError, PreparedGateway,
};
use agent24_models::IdorisGateway;
use agent24_models::router::ModelRouter;

fn host_error(error: agent24_models::ModelError) -> HostError {
    use agent24_models::ModelError as E;
    match error {
        E::Unavailable(message) => HostError::Unavailable(message),
        E::Provider(message) => HostError::Provider(message),
        E::Cancelled => HostError::Cancelled,
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

/// This copy must stay the production impl in `agent24-models` line for line
/// (paths aside): the unit tests above only prove what they run.
#[test]
fn matches_the_production_impl_in_agent24_models() {
    fn body(source: &str, own_crate: &str) -> Vec<String> {
        let start = source
            .find("fn host_error(")
            .expect("host_error is the first item");
        source[start..]
            .lines()
            .map(|line| line.replace(own_crate, "crate::").trim().to_owned())
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .take_while(|line| !line.starts_with("#[test]"))
            .collect()
    }
    let test = body(include_str!("test_slot.rs"), "agent24_models::");
    let production = body(
        include_str!("../../agent24-models/src/idoris_slot.rs"),
        "crate::",
    );
    assert_eq!(test, production);
}
