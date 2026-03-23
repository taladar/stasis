// Author: Dustin Pilgrim
// License: GPL-3.0-only

use tokio::sync::{mpsc, oneshot};

use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

pub async fn handle_toggle_inhibit(tx: &mpsc::Sender<ManagerMsg>) -> String {
    // Ask daemon for current state.
    let (reply_tx, reply_rx) = oneshot::channel();
    if tx
        .send(ManagerMsg::GetInfo { reply: reply_tx })
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }

    let info = match reply_rx.await {
        Ok(x) => x,
        Err(_) => return "ERROR: daemon info channel closed".to_string(),
    };

    // Manual toggle should be driven by the daemon's authoritative manual pause bit.
    let is_paused = info.manually_paused;

    let now_ms = crate::core::utils::now_ms();

    if is_paused {
        if tx
            .send(ManagerMsg::Event(Event::ManualResume { now_ms }))
            .await
            .is_err()
        {
            return "ERROR: daemon event channel closed".to_string();
        }
        "Idle timers resumed".to_string()
    } else {
        if tx
            .send(ManagerMsg::Event(Event::ManualPause { now_ms }))
            .await
            .is_err()
        {
            return "ERROR: daemon event channel closed".to_string();
        }
        "Idle timers paused".to_string()
    }
}
