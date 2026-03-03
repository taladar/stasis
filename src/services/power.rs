// Author: Dustin Pilgrim
// License: MIT

use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, sleep};

use crate::core::events::{Event, PowerState};
use crate::core::manager_msg::ManagerMsg;
use crate::core::utils;

pub async fn run_power(tx: Sender<ManagerMsg>) {
    // Desktop? Do nothing forever.
    if !utils::is_laptop() {
        eventline::info!("power: desktop detected, power service disabled");
        return;
    }

    eventline::info!("power: laptop detected, starting power monitor");

    // Initial state
    let mut on_ac = utils::is_on_ac_power();
    let initial = if on_ac {
        PowerState::OnAC
    } else {
        PowerState::OnBattery
    };

    let _ = tx
        .send(ManagerMsg::Event(Event::PowerChanged {
            state: initial,
            now_ms: utils::now_ms(),
        }))
        .await;

    // Poll loop
    loop {
        sleep(Duration::from_secs(5)).await;

        let now_on_ac = utils::is_on_ac_power();
        if now_on_ac != on_ac {
            on_ac = now_on_ac;

            let state = if on_ac {
                PowerState::OnAC
            } else {
                PowerState::OnBattery
            };

            eventline::info!(
                "power: source changed -> {}",
                if on_ac { "AC" } else { "Battery" }
            );

            if tx
                .send(ManagerMsg::Event(Event::PowerChanged {
                    state,
                    now_ms: utils::now_ms(),
                }))
                .await
                .is_err()
            {
                break;
            }
        }
    }
}
