// Author: Dustin Pilgrim
// License: MIT

use crate::core::{
    config::PlanStepKind,
    error::{ConfigError, Error},
    state::State,
};

use super::Manager;

impl Manager {
    pub fn list_actions(&self, state: &State) -> String {
        let cfg_opt = self
            .cfg_file
            .effective_for(state.active_profile(), state.plan_source());

        let cfg = match cfg_opt {
            Some(c) => c,
            None => {
                let e = Error::InvalidConfig(ConfigError::ProfileNotFound);
                return format!("ERROR: {e}");
            }
        };

        let mut out = String::new();
        out.push_str("Effective action plan:\n");

        for (idx, step) in cfg.plan.iter().enumerate() {
            if !step.enabled() {
                continue;
            }

            let name = match &step.kind {
                PlanStepKind::Startup => "startup".to_string(),
                PlanStepKind::Dpms => "dpms".to_string(),
                PlanStepKind::Brightness => "brightness".to_string(),
                PlanStepKind::LockScreen => "lock-screen".to_string(),
                PlanStepKind::Suspend => "suspend".to_string(),
                PlanStepKind::Custom(k) => format!("custom:{}", Self::normalize_trigger_name(k)),
            };

            let instant = if step.is_instant() { " (instant)" } else { "" };
            let timeout = step.timeout_seconds;

            let cmd = if let Some(c) = step.command.as_deref() {
                c
            } else {
                "<none>"
            };

            let notify = match (&step.notification, step.notify_seconds_before) {
                (Some(msg), Some(sec)) => format!(" notify={}s \"{}\"", sec, msg),
                (Some(msg), None) => format!(" notify \"{}\"", msg),
                _ => "".to_string(),
            };

            out.push_str(&format!(
                "  {:>2}. {:<18} timeout={}s{} cmd={}{}\n",
                idx, name, timeout, instant, cmd, notify
            ));
        }

        out
    }

    pub fn list_profiles(&self) -> String {
        let mut names: Vec<String> = self
            .cfg_file
            .profiles
            .iter()
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        names.dedup();

        if names.is_empty() {
            return "No profiles configured".to_string();
        }

        let mut out = String::new();
        out.push_str("Profiles:\n");
        for n in names {
            out.push_str(&format!("  {n}\n"));
        }
        out
    }
}
