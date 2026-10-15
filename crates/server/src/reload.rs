//! Watches `lsk.json` for hand edits: the control panel saves
//! through the server, but a file edited by hand loads too, through the
//! config store (which ignores the server's own saves).

use std::sync::Arc;
use std::time::Duration;

use crate::services::config_store::ConfigStore;

const CHECK_EVERY: Duration = Duration::from_secs(1);
/// Editors may save in several writes; wait this long after a change before
/// reading the file for real.
const SETTLE: Duration = Duration::from_millis(300);

pub fn spawn(store: Arc<ConfigStore>) {
    tokio::spawn(async move {
        let mut last = std::fs::read_to_string(store.path()).ok();
        let mut interval = tokio::time::interval(CHECK_EVERY);
        loop {
            interval.tick().await;
            let text = std::fs::read_to_string(store.path()).ok();
            if text == last || text.is_none() {
                continue;
            }
            tokio::time::sleep(SETTLE).await;
            last = std::fs::read_to_string(store.path()).ok();
            store.file_changed().await;
        }
    });
}
