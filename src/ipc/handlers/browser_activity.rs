// Author: Dustin Pilgrim
// License: MIT

use tokio::sync::mpsc;

use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

/// IPC handler: `browser-activity`
///
/// Emits an activity-like signal without touching inhibitor counters.
pub async fn handle_browser_activity(tx: &mpsc::Sender<ManagerMsg>) -> String {
    let now_ms = crate::core::utils::now_ms();
    eventline::info!("browser: activity pulse");

    if tx
        .send(ManagerMsg::Event(Event::BrowserActivity { now_ms }))
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }

    "OK".to_string()
}
