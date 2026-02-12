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
            Action::RunLockScreen { command, use_loginctl } => {
                Self::spawn_lock_screen(tx, command, use_loginctl);
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

            Action::LockSession => {
                eventline::info!("lock-session: loginctl lock-session");
                crate::core::utils::run_shell_command_silent("loginctl lock-session")
                    .map_err(into_any_error)?;
            }

            Action::Suspend => {
                eventline::info!("suspend requested");
            }

            #[cfg(test)]
            Action::Noop => {}
        }

        Ok(())
    }

    fn spawn_lock_screen(tx: mpsc::Sender<ManagerMsg>, command: String, use_loginctl: bool) {
        tokio::spawn(async move {
            if use_loginctl {
                // In loginctl mode, login1 is the source of truth for lock/unlock.
                // Do NOT emit SessionLocked/Unlocked based on process lifetime.

                eventline::info!("lock-session: loginctl lock-session");
                let _ = crate::core::utils::run_shell_command_silent("loginctl lock-session");

                // Fire-and-forget the UI locker. It may block OR daemonize; we don't care here.
                eventline::info!("lock: {} (spawn, loginctl-tracked)", command);
                let _ = Command::new("sh")
                    .arg("-lc")
                    .arg(command)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();

                return;
            }

            // Process-tracked mode: command lifetime defines lock episode.
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
