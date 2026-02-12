// Author: Dustin Pilgrim
// License: MIT

mod actions;
mod run;

use crate::core::{
    action::Action,
    config::{ConfigFile, PlanSource, Pattern},
    events::{Event, PowerState},
    manager::Manager,
    manager_msg::ManagerMsg,
    state::State,
};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::services::dbus::EventSink;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

fn into_any_error(e: Box<dyn std::error::Error>) -> AnyError {
    e.to_string().into()
}

struct MpscEventSink {
    tx: mpsc::Sender<ManagerMsg>,
}

impl EventSink for MpscEventSink {
    fn push(&self, ev: Event) {
        let _ = self.tx.try_send(ManagerMsg::Event(ev));
    }
}

pub struct Daemon {
    manager: Manager,
    state: State,

    config_path: PathBuf,

    inhibit_apps: Vec<Pattern>,

    monitor_media: bool,
    ignore_remote_media: bool,
    media_blacklist: Vec<Pattern>,

    inhibit_epoch: u64,
    enable_loginctl: bool,

    chassis: crate::core::utils::ChassisKind,
    bad_profile_logged: bool,
}

impl Daemon {
    pub fn new(mut cfg_file: ConfigFile, config_path: PathBuf) -> Self {
        let now_ms = crate::core::utils::now_ms();
        let chassis = crate::core::utils::detect_chassis();

        let plan_src = match chassis {
            crate::core::utils::ChassisKind::Desktop => PlanSource::Desktop,
            crate::core::utils::ChassisKind::Laptop => {
                if crate::core::utils::is_on_ac_power() {
                    PlanSource::Ac
                } else {
                    PlanSource::Battery
                }
            }
        };

        let normalized_active_profile = cfg_file
            .active_profile
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .and_then(|s| {
                if s.eq_ignore_ascii_case("default") || s.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .filter(|name| cfg_file.profiles.iter().any(|p| p.name == *name));

        cfg_file.active_profile = normalized_active_profile;

        let effective = cfg_file
            .effective_for(cfg_file.active_profile.as_deref(), plan_src)
            .unwrap_or_else(|| {
                let mut c = cfg_file.default.clone();
                c.select_plan_source(PlanSource::Desktop);
                c
            });

        let inhibit_apps = effective.inhibit_apps.clone();
        let monitor_media = effective.monitor_media;
        let ignore_remote_media = effective.ignore_remote_media;
        let media_blacklist = effective.media_blacklist.clone();

        let enable_loginctl = effective.enable_loginctl;

        eventline::debug!(
            "daemon: chassis={:?}, plan_src={:?}, active_profile={:?}, monitor_media={}, ignore_remote_media={}, media_blacklist_len={}, inhibit_apps_len={}, enable_loginctl={}, config_path={}",
            chassis,
            plan_src,
            cfg_file.active_profile,
            monitor_media,
            ignore_remote_media,
            media_blacklist.len(),
            inhibit_apps.len(),
            enable_loginctl,
            config_path.display(),
        );

        let mut state = State::new(now_ms);
        state.set_plan_source(plan_src);

        match plan_src {
            PlanSource::Ac => state.set_power_state(PowerState::OnAC),
            PlanSource::Battery => state.set_power_state(PowerState::OnBattery),
            PlanSource::Desktop => {}
        }

        state.set_active_profile(cfg_file.active_profile.clone());

        Self {
            manager: Manager::new(cfg_file),
            state,
            config_path,
            inhibit_apps,
            monitor_media,
            ignore_remote_media,
            media_blacklist,
            inhibit_epoch: 0,
            enable_loginctl,
            chassis,
            bad_profile_logged: false,
        }
    }

    fn push_inhibit_rules_from_effective(&mut self, tx: &mpsc::Sender<ManagerMsg>) {
        let cfg_file = self.manager.cfg_file_ref();

        let plan_src = self.state.plan_source();
        let prof = self.state.active_profile();

        let effective = cfg_file.effective_for(prof, plan_src).unwrap_or_else(|| {
            let mut c = cfg_file.default.clone();
            c.select_plan_source(PlanSource::Desktop);
            c
        });

        self.inhibit_epoch = self.inhibit_epoch.wrapping_add(1);

        let msg = ManagerMsg::UpdateInhibitRules {
            epoch: self.inhibit_epoch,
            inhibit_apps: effective.inhibit_apps.clone(),
            monitor_media: effective.monitor_media,
            ignore_remote_media: effective.ignore_remote_media,
            media_blacklist: effective.media_blacklist.clone(),
        };

        let _ = tx.try_send(msg);
    }

    fn handle_one_event_scoped(&mut self, event: Event) -> Vec<Action> {
        if matches!(event, Event::Tick { .. }) {
            return self
                .manager
                .handle_event(&mut self.state, event)
                .unwrap_or_else(|e| {
                    self.log_handle_event_error_once(&e);
                    Vec::new()
                });
        }

        eventline::scope!("event", {
            eventline::debug!("incoming: {:?}", event);

            match self.manager.handle_event(&mut self.state, event.clone()) {
                Ok(actions) => {
                    if !actions.is_empty() {
                        eventline::debug!("actions: {:?}", actions);
                    }
                    actions
                }
                Err(e) => {
                    self.log_handle_event_error_once(&e);
                    Vec::new()
                }
            }
        })
    }

    fn log_handle_event_error_once(&mut self, e: &crate::core::error::Error) {
        let s = format!("{e:?}");
        if s.contains("ProfileNotFound") {
            if !self.bad_profile_logged {
                self.bad_profile_logged = true;
                eventline::error!(
                    "handle_event failed: {:?} (config selection failed; active_profile is invalid — try `stasis profile none` or a real profile name)",
                    e
                );
            }
        } else {
            eventline::error!("handle_event failed: {:?}", e);
        }
    }
}
