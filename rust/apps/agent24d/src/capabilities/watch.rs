use tokio::sync::watch;

use super::store::CapabilityStore;

pub struct CapabilityWatch {
    receiver: watch::Receiver<u64>,
    seen: u64,
}

impl CapabilityStore {
    pub fn watch(&self) -> CapabilityWatch {
        CapabilityWatch {
            receiver: self.shared.changed.subscribe(),
            seen: self.epoch(),
        }
    }
}

impl CapabilityWatch {
    pub fn seen(&self) -> u64 {
        self.seen
    }

    pub async fn changed(&mut self) -> u64 {
        if self.receiver.changed().await.is_err() {
            return self.seen;
        }
        self.seen = *self.receiver.borrow_and_update();
        self.seen
    }
}
