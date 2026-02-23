// Author: Dustin Pilgrim
// License: MIT

use crate::core::{
    events::Event,
    manager_msg::{ListKind, ManagerMsg},
};

use tokio::sync::{mpsc, watch};

use std::sync::Arc;

use crate::services::dbus::EventSink;

use super::{AnyError, Daemon, MpscEventSink};

impl Daemon {
    pub async fn run(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Result<(), AnyError> {
        eventline::info!("daemon starting");

        let (tx, mut rx) = mpsc::channel::<ManagerMsg>(256);

        if let Err(e) = crate::ipc::server::spawn_ipc_server(tx.clone()).await {
            eventline::warn!("ipc: failed to start: {}", e);
        }

        {
            let sink: Arc<dyn EventSink> = Arc::new(MpscEventSink { tx: tx.clone() });

            match crate::services::dbus::spawn_dbus_listeners(
                sink,
                self.enable_loginctl,
                shutdown.clone(),
            ) {
                Ok(_handle) => eventline::info!("dbus: listener started"),
                Err(e) => eventline::warn!("dbus: failed to start listener thread: {e}"),
            }
        }

        tokio::spawn(crate::services::ticker::run_ticker(tx.clone()));

        let (app_rules_tx, app_rules_rx) = watch::channel(crate::services::app_inhibit::AppRules {
            epoch: self.inhibit_epoch,
            apps: self.inhibit_apps.clone(),
        });
        tokio::spawn(crate::services::app_inhibit::run_app_inhibit(tx.clone(), app_rules_rx));

        let (media_rules_tx, media_rules_rx) =
            watch::channel(crate::services::media::MediaRules {
                epoch: self.inhibit_epoch,
                monitor_media: self.monitor_media,
                ignore_remote_media: self.ignore_remote_media,
                media_blacklist: self.media_blacklist.clone(),
            });
        tokio::spawn(crate::services::media::run_media(tx.clone(), media_rules_rx));

        if matches!(self.chassis, crate::core::utils::ChassisKind::Laptop) {
            tokio::spawn(crate::services::power::run_power(tx.clone()));
        } else {
            eventline::info!("power: skipped (desktop chassis)");
        }

        tokio::spawn({
            let tx = tx.clone();
            let shutdown = shutdown.clone();
            let shutdown_tx = shutdown_tx.clone();
            async move {
                let _ = crate::services::wayland::run_wayland(tx, shutdown, shutdown_tx).await;
            }
        });

        self.push_inhibit_rules_from_effective(&tx);

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        eventline::info!("daemon stopping (shutdown requested)");
                        break;
                    }
                }

                maybe = rx.recv() => {
                    let Some(msg) = maybe else {
                        eventline::info!("daemon stopping (event channel closed)");
                        break;
                    };

                    match msg {
                        ManagerMsg::Event(event) => {
                            let refresh_after = matches!(event, Event::ProfileChanged{..} | Event::PowerChanged{..});

                            let actions = self.handle_one_event_scoped(event);

                            if refresh_after {
                                self.push_inhibit_rules_from_effective(&tx);
                            }

                            for action in actions {
                                if let Err(e) = self.exec_action_with_tx(action, tx.clone()).await {
                                    eventline::error!("action failed: {}", e);
                                }
                            }
                        }

                        ManagerMsg::UpdateInhibitRules { epoch, inhibit_apps, monitor_media, ignore_remote_media, media_blacklist } => {
                            self.inhibit_epoch = epoch;
                            self.inhibit_apps = inhibit_apps.clone();
                            self.monitor_media = monitor_media;
                            self.ignore_remote_media = ignore_remote_media;
                            self.media_blacklist = media_blacklist.clone();

                            let _ = app_rules_tx.send(crate::services::app_inhibit::AppRules {
                                epoch,
                                apps: inhibit_apps,
                            });

                            let _ = media_rules_tx.send(crate::services::media::MediaRules {
                                epoch,
                                monitor_media,
                                ignore_remote_media,
                                media_blacklist,
                            });
                        }

                        ManagerMsg::List { kind, reply } => {
                            let out = match kind {
                                ListKind::Actions => Ok(self.manager.list_actions(&self.state)),
                                ListKind::Profiles => Ok(self.manager.list_profiles()),
                            };
                            let _ = reply.send(out);
                        }

                        ManagerMsg::GetInfo { reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let snap = self.manager.snapshot(&self.state, now_ms);
                            let _ = reply.send(snap);
                        }

                        ManagerMsg::SetProfile { name, reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let raw = name.clone().unwrap_or_else(|| "none".to_string());

                            let ev = Event::ProfileChanged { name: raw, now_ms };
                            let res = self.manager.handle_event(&mut self.state, ev);

                            let out = match res {
                                Ok(_actions) => {
                                    self.bad_profile_logged = false;
                                    let shown = name.unwrap_or_else(|| "none".to_string());

                                    self.push_inhibit_rules_from_effective(&tx);

                                    Ok(format!("Profile set: {shown}"))
                                }
                                Err(e) => Err(format!("{e:?}")),
                            };

                            let _ = reply.send(out);
                        }

                        ManagerMsg::ReloadConfig { reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let loaded = crate::config::load_from_path(&self.config_path);

                            let out = match loaded {
                                Ok(loaded) => {
                                    if loaded.path != self.config_path {
                                        eventline::warn!(
                                            "reload: primary config failed; fell back to {}",
                                            loaded.path.display()
                                        );
                                        self.config_path = loaded.path.clone();
                                    }

                                    let new_cfg_file = loaded.cfg;
                                    self.manager.set_config(new_cfg_file.clone());

                                    let desired = match self.state.active_profile() {
                                        Some(name) => {
                                            if new_cfg_file.effective_for(Some(name), self.state.plan_source()).is_some() {
                                                name.to_string()
                                            } else {
                                                "none".to_string()
                                            }
                                        }
                                        None => "none".to_string(),
                                    };

                                    let ev = Event::ProfileChanged { name: desired.clone(), now_ms };

                                    match self.manager.handle_event(&mut self.state, ev) {
                                        Ok(actions) => {
                                            self.bad_profile_logged = false;

                                            for action in actions {
                                                if let Err(e) = self.exec_action_with_tx(action, tx.clone()).await {
                                                    eventline::error!("action failed: {}", e);
                                                }
                                            }

                                            self.push_inhibit_rules_from_effective(&tx);

                                            if desired == "none" {
                                                Ok("Reloaded (profile missing; switched to none)".to_string())
                                            } else {
                                                Ok(format!("Reloaded (profile kept: {desired})"))
                                            }
                                        }
                                        Err(e) => Err(format!("{e:?}")),
                                    }
                                }
                                Err(e) => Err(e),
                            };

                            let _ = reply.send(out);
                        }

                        ManagerMsg::StopDaemon { reply } => {
                            eventline::info!("daemon stopping (stop requested via IPC)");
                            let _ = reply.send(Ok("Stopping Stasis daemon".to_string()));
                            let _ = shutdown_tx.send(true);
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
