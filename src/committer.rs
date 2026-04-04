use crate::accounts::{AccountStore, OverlayStore};

// TODO: TransactionStatusService
// TODO: extract vote tx
// TODO: upd priority fee cache
// TODO: emit metrics
// TODO: collect balances (?)

pub struct Committer {}

impl Committer {
    pub fn commit_overlays(&self, overlays: Vec<OverlayStore<dyn AccountStore>>) {
        for overlay in overlays {
            overlay.flush();
        }
    }
}
