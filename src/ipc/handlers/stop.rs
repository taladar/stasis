// Author: Dustin Pilgrim
// License: GPL-3.0-only

use tokio::sync::{mpsc, oneshot};

use crate::core::manager_msg::ManagerMsg;

/// Handle `stasis stop` (no args).
///
/// Semantics:
/// - Ask the daemon to exit cleanly.
/// - Reply once the daemon has acknowledged the request.
pub async fn handle_stop(tx: &mpsc::Sender<ManagerMsg>) -> String {
    let (reply_tx, reply_rx) = oneshot::channel();

    if tx
        .send(ManagerMsg::StopDaemon { reply: reply_tx })
        .await
        .is_err()
    {
        return "Stasis daemon not running".to_string();
    }

    match reply_rx.await {
        Ok(Ok(msg)) => {
            let out = msg.trim_end();
            if out.is_empty() {
                "Stopping Stasis daemon".to_string()
            } else {
                out.to_string()
            }
        }
        Ok(Err(e)) => {
            let out = e.trim_end();
            if out.is_empty() {
                "ERROR: stop failed".to_string()
            } else {
                format!("ERROR: {out}")
            }
        }
        Err(_) => "ERROR: No response from daemon".to_string(),
    }
}
