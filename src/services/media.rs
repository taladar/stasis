// Author: Dustin Pilgrim
// License: MIT

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

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

/// Spawnable task: periodically polls media playback state and emits events on change.
///
/// Logging policy (INFO):
/// - Real transitions: "count X -> Y"
/// - First poll OR forced refresh (profile/reload/rules): log "count 0 -> N" if N != 0
pub async fn run_media(tx: mpsc::Sender<ManagerMsg>, mut rules_rx: watch::Receiver<MediaRules>) {
    let initial = rules_rx.borrow().clone();
    let mut last_epoch = initial.epoch;

    let mut ignore_first_epoch_bump = true;

    let mut svc = MediaService::new(initial.ignore_remote_media, initial.media_blacklist.clone())
        .with_poll_interval_ms(1000);

    eventline::info!(
        "media: started (monitor_media={}, ignore_remote_media={}, blacklist_len={})",
        initial.monitor_media,
        initial.ignore_remote_media,
        svc.blacklist_len(),
    );

    // Seed core immediately on startup.
    if initial.monitor_media {
        svc.force_emit_next(); // baseline refresh log if non-zero
        let now_ms = crate::core::utils::now_ms();
        if let Some(evs) = svc.poll(now_ms) {
            for ev in evs {
                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                    return;
                }
            }
        }
    } else {
        // Disabled at startup: set core to clean 0/Idle once.
        let now_ms = crate::core::utils::now_ms();
        for ev in [
            Event::MediaInhibitorCount { count: 0, now_ms },
            Event::MediaStateChanged {
                state: MediaState::Idle,
                now_ms,
            },
        ] {
            if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                return;
            }
        }
    }

    let sleep_ms = 250u64;
    let mut last_enabled: bool = initial.monitor_media;

    loop {
        tokio::select! {
            // --- rule updates (profile switch / reload config) ---
            changed = rules_rx.changed() => {
                if changed.is_err() {
                    return; // sender dropped => shutting down
                }

                // IMPORTANT: clone rules so we don't hold a watch::Ref across await
                let rules = rules_rx.borrow().clone();
                let MediaRules { epoch, monitor_media, ignore_remote_media, media_blacklist } = rules;

                // Detect epoch bump
                let epoch_bumped = epoch != last_epoch;
                if epoch_bumped {
                    last_epoch = epoch;
                }

                // Apply semantics changes; if they changed, svc will force refresh.
                svc.reconfigure(ignore_remote_media, media_blacklist.clone());

                // monitor_media toggle handling
                if monitor_media != last_enabled {
                    last_enabled = monitor_media;

                    if monitor_media {
                        // Enabling: baseline refresh and send immediately.
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
                        // Disabling: ensure core ends at 0/Idle.
                        let now_ms = crate::core::utils::now_ms();
                        for ev in [
                            Event::MediaInhibitorCount { count: 0, now_ms },
                            Event::MediaStateChanged { state: MediaState::Idle, now_ms },
                        ] {
                            if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                                return;
                            }
                        }
                    }
                } else if monitor_media && epoch_bumped {
                    // Same enabled state; baseline refresh ONLY on epoch bump (reload/profile).
                    // BUT: ignore the first bump after startup because we already seeded above.
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

            // --- periodic tick ---
            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                let rules = rules_rx.borrow().clone();
                let MediaRules { monitor_media, .. } = rules;

                if !monitor_media {
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

#[derive(Debug)]
pub struct MediaService {
    ignore_remote_media: bool,
    media_blacklist: Vec<Pattern>,

    poll_interval_ms: u64,
    last_poll_ms: u64,

    last_count: Option<u64>,
    last_state: Option<MediaState>,

    // When true: next poll emits even if unchanged AND uses baseline logging.
    force_emit: bool,
}

impl MediaService {
    pub fn new(ignore_remote_media: bool, media_blacklist: Vec<Pattern>) -> Self {
        Self {
            ignore_remote_media,
            media_blacklist,

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

    /// Apply updated semantics (profile switch / reload config).
    /// If semantics changed, force a baseline refresh emission on next poll.
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

    /// Force the next poll() to emit current truth even if unchanged,
    /// and allow immediate poll.
    pub fn force_emit_next(&mut self) {
        self.force_emit = true;
        self.last_poll_ms = 0;
    }

    /// Poll once. Returns events on:
    /// - first poll
    /// - real change
    /// - forced refresh
    pub fn poll(&mut self, now_ms: u64) -> Option<Vec<Event>> {
        if now_ms < self.last_poll_ms.saturating_add(self.poll_interval_ms) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let snapshot = match read_pactl_snapshot(&self.media_blacklist) {
            Ok(s) => s,
            Err(_e) => MediaSnapshot::default(),
        };

        let local = snapshot.local_keys.len() as u64;
        let remote = snapshot.remote_keys.len() as u64;

        let inhibitor_count = if self.ignore_remote_media {
            local
        } else {
            local + remote
        };

        let state = if local > 0 {
            MediaState::PlayingLocal
        } else if remote > 0 {
            MediaState::PlayingRemote
        } else {
            MediaState::Idle
        };

        let first_poll = self.last_count.is_none() && self.last_state.is_none();
        let prev_count = self.last_count.unwrap_or(0);

        let count_changed = !first_poll && self.last_count != Some(inhibitor_count);
        let state_changed = !first_poll && self.last_state != Some(state);

        // -------- INFO logging (consistent) --------
        if count_changed {
            eventline::info!(
                "media: count {} -> {} (local={}, remote={}, ignore_remote={})",
                prev_count,
                inhibitor_count,
                local,
                remote,
                self.ignore_remote_media
            );
        } else if (first_poll || self.force_emit) && inhibitor_count != 0 {
            eventline::info!(
                "media: count {} -> {} (local={}, remote={}, ignore_remote={})",
                0u64,
                inhibitor_count,
                local,
                remote,
                self.ignore_remote_media
            );
        }
        // ------------------------------------------

        // Emit on first poll, changes, or forced refresh
        if first_poll || count_changed || state_changed || self.force_emit {
            self.last_count = Some(inhibitor_count);
            self.last_state = Some(state);

            let out = vec![
                Event::MediaInhibitorCount {
                    count: inhibitor_count,
                    now_ms,
                },
                Event::MediaStateChanged { state, now_ms },
            ];

            self.force_emit = false;
            return Some(out);
        }

        None
    }
}

#[derive(Debug, Default)]
struct MediaSnapshot {
    // dedup: each “app” counts once even if multiple sink-inputs exist
    local_keys: HashSet<String>,
    remote_keys: HashSet<String>,
}

fn read_pactl_snapshot(media_blacklist: &[Pattern]) -> Result<MediaSnapshot, String> {
    let out = Command::new("sh")
        .arg("-lc")
        .arg("pactl list sink-inputs")
        .output()
        .map_err(|e| format!("failed to run pactl: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "pactl exited non-zero: {}",
            out.status.code().unwrap_or(-1)
        ));
    }

    let s = String::from_utf8_lossy(&out.stdout);
    Ok(parse_pactl_sink_inputs(&s, media_blacklist))
}

fn parse_pactl_sink_inputs(text: &str, media_blacklist: &[Pattern]) -> MediaSnapshot {
    let mut local_keys: HashSet<String> = HashSet::new();
    let mut remote_keys: HashSet<String> = HashSet::new();

    let mut in_block = false;

    // We consider playing if corked==false, otherwise fall back to RUNNING.
    let mut seen_state = false;
    let mut is_running = false;
    let mut seen_corked = false;
    let mut corked = true;

    // Fields per block (we keep originals; matching/needles do their own lowercase)
    let mut app_name = String::new();
    let mut app_bin = String::new();
    let mut node_name = String::new();
    let mut media_name = String::new();
    let mut sink_str = String::new();
    let mut proc_id = String::new();

    let flush_block = |in_block: bool,
                       seen_state: bool,
                       is_running: bool,
                       seen_corked: bool,
                       corked: bool,
                       app_name: &str,
                       app_bin: &str,
                       node_name: &str,
                       media_name: &str,
                       sink_str: &str,
                       proc_id: &str,
                       local_keys: &mut HashSet<String>,
                       remote_keys: &mut HashSet<String>| {
        if !in_block {
            return;
        }

        // Decide “playing”
        let playing = if seen_corked {
            !corked
        } else if seen_state {
            is_running
        } else {
            false
        };
        if !playing {
            return;
        }

        if looks_like_system_audio(app_name, app_bin, node_name, media_name) {
            return;
        }

        // ---- FIRST: ignore obvious games (cheap heuristic) ----
        if looks_like_game(app_name, app_bin, node_name) {
            return;
        }

        // Blacklist check (pattern match over combined identity fields)
        if is_blacklisted(media_blacklist, app_name, app_bin, node_name, media_name) {
            return;
        }

        let remote = is_remote_stream(app_name, app_bin, node_name, media_name, sink_str);

        // Dedup key: prefer process id so 2 sink-inputs for the same game/app count once.
        let key = if !proc_id.is_empty() {
            format!("pid:{proc_id}")
        } else if !node_name.is_empty() {
            format!("node:{}", node_name.to_lowercase())
        } else if !app_name.is_empty() {
            format!("app:{}", app_name.to_lowercase())
        } else {
            return;
        };

        if remote {
            remote_keys.insert(key);
        } else {
            local_keys.insert(key);
        }
    };

    for line in text.lines() {
        let line = line.trim_end();

        if line.starts_with("Sink Input #") {
            flush_block(
                in_block,
                seen_state,
                is_running,
                seen_corked,
                corked,
                &app_name,
                &app_bin,
                &node_name,
                &media_name,
                &sink_str,
                &proc_id,
                &mut local_keys,
                &mut remote_keys,
            );

            in_block = true;
            seen_state = false;
            is_running = false;
            seen_corked = false;
            corked = true;

            app_name.clear();
            app_bin.clear();
            node_name.clear();
            media_name.clear();
            sink_str.clear();
            proc_id.clear();
            continue;
        }

        if !in_block {
            continue;
        }

        let l = line.trim();

        if let Some(rest) = l.strip_prefix("State:") {
            seen_state = true;
            let st = rest.trim();
            is_running = st.eq_ignore_ascii_case("RUNNING");
            continue;
        }

        if let Some(rest) = l.strip_prefix("Corked:") {
            seen_corked = true;
            let v = rest.trim();
            corked = v.eq_ignore_ascii_case("yes");
            continue;
        }

        if let Some(rest) = l.strip_prefix("Sink:") {
            sink_str = rest.trim().to_lowercase();
            continue;
        }

        if let Some((k, v)) = parse_pactl_kv(l) {
            match k {
                "application.name" => app_name = v,
                "application.process.binary" => app_bin = v,
                "application.process.id" => proc_id = v,
                "node.name" => node_name = v,
                "media.name" => media_name = v,
                _ => {}
            }
        }
    }

    flush_block(
        in_block,
        seen_state,
        is_running,
        seen_corked,
        corked,
        &app_name,
        &app_bin,
        &node_name,
        &media_name,
        &sink_str,
        &proc_id,
        &mut local_keys,
        &mut remote_keys,
    );

    MediaSnapshot {
        local_keys,
        remote_keys,
    }
}

fn parse_pactl_kv(line: &str) -> Option<(&str, String)> {
    let mut parts = line.splitn(2, '=');
    let k = parts.next()?.trim();
    let v = parts.next()?.trim();

    let v = v.strip_prefix('"').unwrap_or(v);
    let v = v.strip_suffix('"').unwrap_or(v);

    Some((k, v.to_string()))
}

fn is_blacklisted(
    blacklist: &[Pattern],
    app_name: &str,
    app_bin: &str,
    node_name: &str,
    media_name: &str,
) -> bool {
    if blacklist.is_empty() {
        return false;
    }
    let hay_lc = format!("{app_name} {app_bin} {node_name} {media_name}").to_lowercase();
    blacklist.iter().any(|p| p.matches_lc(&hay_lc))
}

fn is_remote_stream(
    app_name: &str,
    app_bin: &str,
    node_name: &str,
    media_name: &str,
    sink_str: &str,
) -> bool {
    let hay = format!("{app_name} {app_bin} {node_name} {media_name} {sink_str}").to_lowercase();

    const NEEDLES: &[&str] = &[
        "bluez",
        "raop",
        "airplay",
        "rtp",
        "rtsp",
        "tunnel",
        "network",
        "chromecast",
        "cast",
        "spotify",
        "connect",
        "sonos",
    ];

    NEEDLES.iter().any(|n| hay.contains(n))
}

fn looks_like_system_audio(app_name: &str, app_bin: &str, node_name: &str, media_name: &str) -> bool {
    let app_lc = app_name.to_lowercase();
    let bin_lc = app_bin.to_lowercase();
    let node_lc = node_name.to_lowercase();
    let media_lc = media_name.to_lowercase();

    if app_lc.starts_with("speech-dispatcher-")
        || node_lc.starts_with("speech-dispatcher-")
        || bin_lc == "sd_generic"
        || bin_lc == "sd_dummy"
        || bin_lc.starts_with("sd_")
    {
        return true;
    }

    if app_lc == "speech-dispatcher" && media_lc == "playback" {
        return true;
    }

    false
}

// Cheap “game” heuristic: conservative. You can tune these as you observe misses/false positives.
fn looks_like_game(app_name: &str, app_bin: &str, node_name: &str) -> bool {
    let hay = format!("{app_name} {app_bin} {node_name}").to_lowercase();

    // Proton/Wine games (your RE4 example)
    if hay.contains("wine64-preloader") || hay.contains("wine-preloader") {
        return true;
    }

    // If you want: treat Steam's game audio as "not media" too (riskier)
    // Keep it strict: only when "steam" + "steam_app_" show up somewhere.
    if hay.contains("steam") && hay.contains("steam_app_") {
        return true;
    }

    false
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
