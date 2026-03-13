// Author: Dustin Pilgrim
// License: GPLv3

use std::collections::HashSet;
use std::env;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::core::config::Pattern;
use crate::core::events::{Event, MediaState};
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug, Clone)]
pub struct MediaRules {
    pub epoch: u64, // forces watch::changed() on profile/reload even if values are identical
    pub monitor_media: bool,
    pub ignore_remote_media: bool,
    pub media_blacklist: Vec<Pattern>,
}

/// Spawnable task: polls compositor idle-inhibitor state for non-browser media and emits events on change.
pub async fn run_media(tx: mpsc::Sender<ManagerMsg>, mut rules_rx: watch::Receiver<MediaRules>) {
    let initial = rules_rx.borrow().clone();
    let mut last_epoch = initial.epoch;
    let mut ignore_first_epoch_bump = true;

    let mut svc = MediaService::new(initial.ignore_remote_media, initial.media_blacklist.clone())
        .with_poll_interval_ms(500);

    eventline::info!(
        "media: started (monitor_media={}, ignore_remote_media={}, blacklist_len={}, backend={}) [idle-inhibitor-truth]",
        initial.monitor_media,
        initial.ignore_remote_media,
        svc.blacklist_len(),
        svc.backend_name(),
    );

    if initial.monitor_media {
        svc.force_emit_next();
        let now_ms = crate::core::utils::now_ms();
        if let Some(evs) = svc.poll(now_ms) {
            for ev in evs {
                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                    return;
                }
            }
        }
    } else {
        seed_idle(&tx).await;
    }

    let sleep_ms = 250u64;
    let mut last_enabled: bool = initial.monitor_media;

    loop {
        tokio::select! {
            changed = rules_rx.changed() => {
                if changed.is_err() {
                    return;
                }

                let rules = rules_rx.borrow().clone();
                let MediaRules { epoch, monitor_media, ignore_remote_media, media_blacklist } = rules;

                let epoch_bumped = epoch != last_epoch;
                if epoch_bumped {
                    last_epoch = epoch;
                }

                svc.reconfigure(ignore_remote_media, media_blacklist.clone());

                if monitor_media != last_enabled {
                    last_enabled = monitor_media;

                    if monitor_media {
                        svc.force_emit_next();
                        let now_ms = crate::core::utils::now_ms();
                        if let Some(evs) = svc.poll(now_ms) {
                            for ev in evs {
                                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    } else {
                        seed_idle(&tx).await;
                    }
                } else if monitor_media && epoch_bumped {
                    if ignore_first_epoch_bump {
                        ignore_first_epoch_bump = false;
                    } else {
                        svc.force_emit_next();
                        let now_ms = crate::core::utils::now_ms();
                        if let Some(evs) = svc.poll(now_ms) {
                            for ev in evs {
                                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }

            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                let rules = rules_rx.borrow().clone();
                if !rules.monitor_media {
                    continue;
                }

                let now_ms = crate::core::utils::now_ms();
                if let Some(evs) = svc.poll(now_ms) {
                    for ev in evs {
                        if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn seed_idle(tx: &mpsc::Sender<ManagerMsg>) {
    let now_ms = crate::core::utils::now_ms();
    for ev in [
        Event::MediaInhibitorCount { count: 0, now_ms },
        Event::MediaStateChanged {
            state: MediaState::Idle,
            now_ms,
        },
    ] {
        let _ = tx.send(ManagerMsg::Event(ev)).await;
    }
}

#[derive(Debug)]
pub struct MediaService {
    ignore_remote_media: bool,
    media_blacklist: Vec<Pattern>,
    backend: MediaBackend,

    poll_interval_ms: u64,
    last_poll_ms: u64,

    last_count: Option<u64>,
    last_state: Option<MediaState>,

    force_emit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaBackend {
    Hyprland,
    Niri,
    None,
}

impl MediaService {
    pub fn new(ignore_remote_media: bool, media_blacklist: Vec<Pattern>) -> Self {
        Self {
            ignore_remote_media,
            media_blacklist,
            backend: detect_backend(),
            poll_interval_ms: 1000,
            last_poll_ms: 0,
            last_count: None,
            last_state: None,
            force_emit: false,
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    pub fn blacklist_len(&self) -> usize {
        self.media_blacklist.len()
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            MediaBackend::Hyprland => "hyprland",
            MediaBackend::Niri => "niri",
            MediaBackend::None => "none",
        }
    }

    pub fn reconfigure(&mut self, ignore_remote_media: bool, media_blacklist: Vec<Pattern>) {
        let changed = self.ignore_remote_media != ignore_remote_media
            || !patterns_same(&self.media_blacklist, &media_blacklist);

        self.ignore_remote_media = ignore_remote_media;
        self.media_blacklist = media_blacklist;

        if changed {
            self.force_emit_next();
            eventline::info!(
                "media: reconfigured (ignore_remote_media={}, blacklist_len={})",
                self.ignore_remote_media,
                self.media_blacklist.len()
            );
        }
    }

    pub fn force_emit_next(&mut self) {
        self.force_emit = true;
        self.last_poll_ms = 0;
    }

    pub fn poll(&mut self, now_ms: u64) -> Option<Vec<Event>> {
        if now_ms < self.last_poll_ms.saturating_add(self.poll_interval_ms) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let count = match self.backend {
            MediaBackend::Hyprland => match hyprland_idle_inhibitor_count(&self.media_blacklist) {
                Ok(n) => n,
                Err(e) => {
                    eventline::warn!(
                        "media: hyprland idle-inhibitor query failed (keeping previous): {}",
                        e
                    );
                    self.last_count.unwrap_or(0)
                }
            },
            MediaBackend::Niri => match niri_idle_inhibitor_count(&self.media_blacklist) {
                Ok(n) => n,
                Err(e) => {
                    eventline::warn!(
                        "media: niri idle-inhibitor query failed (keeping previous): {}",
                        e
                    );
                    self.last_count.unwrap_or(0)
                }
            },
            MediaBackend::None => 0,
        };

        let state = if count > 0 {
            MediaState::PlayingLocal
        } else {
            MediaState::Idle
        };

        self.emit_state(now_ms, count, state)
    }

    fn emit_state(&mut self, now_ms: u64, count: u64, state: MediaState) -> Option<Vec<Event>> {
        let first_poll = self.last_count.is_none() && self.last_state.is_none();
        let prev_count = self.last_count.unwrap_or(0);

        let count_changed = !first_poll && self.last_count != Some(count);
        let state_changed = !first_poll && self.last_state != Some(state);

        if count_changed {
            eventline::info!(
                "media: count {} -> {} (state={:?})",
                prev_count,
                count,
                state
            );
        } else if (first_poll || self.force_emit) && count != 0 {
            eventline::info!("media: count {} -> {} (state={:?})", 0u64, count, state);
        }

        if first_poll || count_changed || state_changed || self.force_emit {
            self.last_count = Some(count);
            self.last_state = Some(state);

            let out = vec![
                Event::MediaInhibitorCount { count, now_ms },
                Event::MediaStateChanged { state, now_ms },
            ];

            self.force_emit = false;
            return Some(out);
        }

        None
    }
}

fn detect_backend() -> MediaBackend {
    if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return MediaBackend::Hyprland;
    }

    if env::var("NIRI_SOCKET").is_ok() {
        return MediaBackend::Niri;
    }

    if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
        let d = desktop.to_lowercase();
        if d.contains("hyprland") {
            return MediaBackend::Hyprland;
        }
        if d.contains("niri") {
            return MediaBackend::Niri;
        }
    }

    MediaBackend::None
}

fn hyprland_idle_inhibitor_count(media_blacklist: &[Pattern]) -> Result<u64, String> {
    let out = Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .map_err(|e| format!("hyprctl spawn failed: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("hyprctl clients -j failed: {}", err.trim()));
    }

    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("hyprctl json parse failed: {e}"))?;

    let arr = v
        .as_array()
        .ok_or_else(|| "hyprctl json: expected array".to_string())?;

    let mut seen: HashSet<String> = HashSet::new();

    for item in arr {
        if !client_is_idle_inhibiting(item) {
            continue;
        }

        if is_browser_client(item) {
            continue;
        }

        if is_blacklisted_client(media_blacklist, item) {
            continue;
        }

        let key = client_key(item);
        seen.insert(key);
    }

    Ok(seen.len() as u64)
}

fn niri_idle_inhibitor_count(media_blacklist: &[Pattern]) -> Result<u64, String> {
    let _ = media_blacklist;
    Ok(0)
}

fn client_key(item: &Value) -> String {
    if let Some(addr) = get_str(item, "address") {
        return format!("addr:{}", addr.to_lowercase());
    }

    if let Some(pid) = item.get("pid").and_then(|v| v.as_i64()) {
        return format!("pid:{pid}");
    }

    let class = get_str(item, "class").unwrap_or("").to_lowercase();
    let title = get_str(item, "title").unwrap_or("").to_lowercase();
    if !class.is_empty() || !title.is_empty() {
        return format!("{class}:{title}");
    }

    "unknown".to_string()
}

fn client_is_idle_inhibiting(item: &Value) -> bool {
    const KEYS: &[&str] = &[
        "inhibitingIdle",
        "inhibitingidle",
        "idleInhibit",
        "idle_inhibit",
        "isIdleInhibited",
        "is_idle_inhibited",
    ];

    if KEYS
        .iter()
        .any(|k| item.get(k).is_some_and(value_is_truthy))
    {
        return true;
    }

    item.get("idleInhibitor").is_some_and(value_is_truthy)
}

fn value_is_truthy(v: &Value) -> bool {
    if let Some(b) = v.as_bool() {
        return b;
    }

    if let Some(n) = v.as_i64() {
        return n > 0;
    }

    if let Some(n) = v.as_u64() {
        return n > 0;
    }

    if let Some(s) = v.as_str() {
        let t = s.trim().to_lowercase();
        if t.is_empty() {
            return false;
        }
        return !matches!(t.as_str(), "0" | "false" | "off" | "none" | "no");
    }

    if let Some(obj) = v.as_object() {
        if obj.values().any(value_is_truthy) {
            return true;
        }
    }

    if let Some(arr) = v.as_array() {
        if arr.iter().any(value_is_truthy) {
            return true;
        }
    }

    false
}

fn is_blacklisted_client(blacklist: &[Pattern], item: &Value) -> bool {
    if blacklist.is_empty() {
        return false;
    }

    let class = get_str(item, "class").unwrap_or("");
    let initial_class = get_str(item, "initialClass").unwrap_or("");
    let title = get_str(item, "title").unwrap_or("");
    let initial_title = get_str(item, "initialTitle").unwrap_or("");
    let hay_lc = format!("{class} {initial_class} {title} {initial_title}").to_lowercase();

    blacklist.iter().any(|p| p.matches_lc(&hay_lc))
}

fn is_browser_client(item: &Value) -> bool {
    let class = get_str(item, "class").unwrap_or("");
    let initial_class = get_str(item, "initialClass").unwrap_or("");
    let title = get_str(item, "title").unwrap_or("");
    let initial_title = get_str(item, "initialTitle").unwrap_or("");
    let hay_lc = format!("{class} {initial_class} {title} {initial_title}").to_lowercase();

    const NEEDLES: &[&str] = &[
        "firefox",
        "chromium",
        "google-chrome",
        "chrome",
        "brave",
        "vivaldi",
        "microsoft-edge",
        "msedge",
        "opera",
        "tor browser",
        "zen browser",
        "waterfox",
        "librewolf",
    ];

    NEEDLES.iter().any(|n| hay_lc.contains(n))
}

fn get_str<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key).and_then(|v| v.as_str())
}

fn pattern_key(p: &Pattern) -> String {
    match p {
        Pattern::Literal(s) => s.clone(),
        Pattern::Regex(r) => format!("/{}/", r.as_str()),
    }
}

fn patterns_same(a: &[Pattern], b: &[Pattern]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .map(pattern_key)
        .zip(b.iter().map(pattern_key))
        .all(|(x, y)| x == y)
}
