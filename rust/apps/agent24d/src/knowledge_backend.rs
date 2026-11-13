//! `agent24_store::Store` as a [`RunSelector`]. The trait lives in
//! `agent24-knowledge` and the store type lives in `agent24-store`, so the
//! impl sits on a newtype in this crate (orphan rule). The body is the one
//! that used to be `impl RunSelector for Store`.

use agent24_knowledge::backend::RunSelector;
use async_trait::async_trait;

pub struct StoreRuns(pub agent24_store::Store);

#[async_trait]
impl RunSelector for StoreRuns {
    async fn knowledge_binding_id(&self, run_id: &str) -> Option<String> {
        self.0
            .get_run(run_id)
            .await
            .ok()
            .flatten()?
            .input
            .knowledge_binding_id
    }
}
