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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use agent24_idoris::managed_idoris::IdorisSlot;

    fn gateway() -> IdorisGateway {
        IdorisGateway::from_credentials("http://127.0.0.1:9/", Zeroizing::new("idk_test".into()))
            .expect("a loopback URL is accepted")
    }

    #[test]
    fn errors_keep_the_text_the_host_showed_before() {
        let rejected = crate::ModelError::Rejected {
            status: 400,
            message: "bad".into(),
        };
        let before = rejected.to_string();
        let after = host_error(rejected);
        assert!(matches!(after, HostError::Provider(ref m) if m == "bad"));
        assert_eq!(after.to_string(), before);

        let busy = crate::ModelError::IdorisSupervisorBusy;
        let before = busy.to_string();
        let after = host_error(busy);
        assert!(matches!(after, HostError::Other(_)));
        assert_eq!(after.to_string(), before);

        assert!(matches!(
            host_error(crate::ModelError::Cancelled),
            HostError::Cancelled
        ));
    }

    #[test]
    fn the_lease_follows_the_published_gateway() {
        let router = ModelRouter::idoris_unavailable("starting");
        assert!(IdorisSlot::published_lease(&router).is_none());

        IdorisSlot::replace_idoris(&router, PreparedGateway::new(gateway()));
        let lease = IdorisSlot::published_lease(&router).expect("a published gateway");
        assert_eq!(lease.strong_count(), 1);
        let reader = router.idoris_gateway().expect("published");
        assert_eq!(lease.strong_count(), 2);
        drop(reader);

        IdorisSlot::block_idoris(&router, "stopping");
        assert_eq!(lease.strong_count(), 0);
        assert!(router.idoris_gateway().is_err());
    }

    #[test]
    #[should_panic(expected = "did not build")]
    fn a_gateway_the_slot_did_not_build_is_refused() {
        let router = ModelRouter::idoris_unavailable("starting");
        IdorisSlot::replace_idoris(&router, PreparedGateway::new(42_u8));
    }
}
