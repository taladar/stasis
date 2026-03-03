// Author: Dustin Pilgrim
// License: MIT

use crate::core::{
    info::{InfoSnapshot, WaybarInfo},
    state::State,
};

use super::Manager;

impl Manager {
    pub fn snapshot(&self, state: &State, now_ms: u64) -> InfoSnapshot {
        let cfg_opt = self
            .cfg_file
            .effective_for(state.active_profile(), state.plan_source());

        let alt = if state.manually_paused() {
            "manually_inhibited"
        } else if state.inhibitors_active() || state.system_paused() || state.is_locked() {
            "idle_inhibited"
        } else if state.debounce_pending() {
            "idle_waiting"
        } else {
            "idle_active"
        };

        let profile = Some(state.active_profile().unwrap_or("default").to_string());

        let rendered = crate::core::manager::info::render_info(cfg_opt.as_ref(), state, now_ms);

        let waybar = WaybarInfo {
            text: "".to_string(),
            alt: alt.to_string(),
            class: alt.to_string(),
            tooltip: rendered.tooltip,
            profile,
        };

        InfoSnapshot::new(waybar, rendered.pretty, state.manually_paused())
    }
}
