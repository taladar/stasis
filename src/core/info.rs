// Author: Dustin Pilgrim
// License: GPL-3.0-only

use serde::Serialize;

/// Snapshot returned from the daemon/manager for `stasis info`.
///
/// - `waybar` is the stable JSON contract.
/// - `pretty_text` is CLI-facing output for `stasis info`.
#[derive(Debug, Clone, Serialize)]
pub struct InfoSnapshot {
    pub waybar: WaybarInfo,

    #[serde(skip_serializing)]
    pub pretty_text: String,

    pub manually_paused: bool,
}

/// Waybar JSON contract.
#[derive(Debug, Clone, Serialize)]
pub struct WaybarInfo {
    pub text: String,
    pub alt: String,
    pub class: String,
    pub tooltip: String,
    pub profile: Option<String>,
}

impl InfoSnapshot {
    pub fn new(waybar: WaybarInfo, pretty_text: impl Into<String>, manually_paused: bool) -> Self {
        Self {
            waybar,
            pretty_text: pretty_text.into(),
            manually_paused,
        }
    }
}
