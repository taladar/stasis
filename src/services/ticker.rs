// Author: Dustin Pilgrim
// License: MIT

use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, sleep};

pub async fn run_ticker(tx: Sender<ManagerMsg>) {
    eventline::info!("ticker started");

    loop {
        sleep(Duration::from_millis(200)).await;

        let now_ms = now_ms();
        // If the daemon is gone, stop.
        if tx
            .send(ManagerMsg::Event(Event::Tick { now_ms }))
            .await
            .is_err()
        {
            eventline::warn!("ticker stopping (receiver dropped)");
            break;
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0));
    d.as_millis() as u64
}
