// Author: Dustin Pilgrim
// License: MIT

use crate::core::{action::Action, events::Event, manager_msg::ManagerMsg};

use tokio::process::Command;
use tokio::sync::mpsc;

use std::process::Stdio;

use super::{into_any_error, AnyError, Daemon};

impl Daemon {
    pub(super) async fn exec_action_with_tx(
        &mut self,
        action: Action,
        tx: mpsc::Sender<ManagerMsg>,
    ) -> Result<(), AnyError> {
        match action {
            Action::RunLockScreen { command } => {
                Self::spawn_lock_screen(tx, command);
            }

            Action::RunCommand { command } => {
                eventline::info!("run: {}", command);
                crate::core::utils::run_shell_command_silent(&command).map_err(into_any_error)?;
            }

            Action::RunResumeCommand { command } => {
                eventline::info!("resume: {}", command);
                crate::core::utils::run_shell_command_silent(&command).map_err(into_any_error)?;
            }

            Action::Notify { message } => {
                eventline::info!("notify: {}", message);
                let _ = std::process::Command::new("sh")
                    .arg("-lc")
                    .arg(format!(
                        "notify-send -a Stasis '{}'",
                        crate::core::utils::escape_single_quotes(&message)
                    ))
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }

            Action::Suspend => {
                eventline::info!("suspend requested");
            }
        }

        Ok(())
    }

    fn spawn_lock_screen(tx: mpsc::Sender<ManagerMsg>, command: String) {
        tokio::spawn(async move {
            // Process-tracked mode is ALWAYS the source of truth.
            let _ = tx
                .send(ManagerMsg::Event(Event::SessionLocked {
                    now_ms: crate::core::utils::now_ms(),
                }))
                .await;

            eventline::info!("lock: {} (await exit)", command);

            let mut child = match Command::new("sh")
                .arg("-lc")
                .arg(command)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    eventline::error!("lock spawn failed: {e}");

                    let _ = tx
                        .send(ManagerMsg::Event(Event::SessionUnlocked {
                            now_ms: crate::core::utils::now_ms(),
                        }))
                        .await;

                    return;
                }
            };

            let _ = child.wait().await;

            let _ = tx
                .send(ManagerMsg::Event(Event::SessionUnlocked {
                    now_ms: crate::core::utils::now_ms(),
                }))
                .await;
        });
    }
}
