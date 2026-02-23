// Author: Dustin Pilgrim
// License: MIT

use std::collections::HashSet;
use std::mem;
use std::process::Command;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::core::config::Pattern;
use crate::core::events::{Event, MediaState};
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug, Clone)]
pub struct MediaRules {
    pub epoch: u64,
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

    force_emit: bool,

    /// Reused scratch sets — cleared before every poll, never dropped.
    local_scratch: HashSet<String>,
    remote_scratch: HashSet<String>,
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

            local_scratch: HashSet::with_capacity(8),
            remote_scratch: HashSet::with_capacity(4),
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    pub fn blacklist_len(&self) -> usize {
        self.media_blacklist.len()
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

        // Take scratch sets out so we can pass mut refs into the parser.
        // On return they come back populated; we read counts then clear for next poll.
        let mut local = mem::take(&mut self.local_scratch);
        let mut remote = mem::take(&mut self.remote_scratch);
        local.clear();
        remote.clear();

        if let Err(_e) = read_pactl_snapshot(&self.media_blacklist, &mut local, &mut remote) {
            // On error leave sets empty — counts stay 0, which is safe.
        }

        let local_count = local.len() as u64;
        let remote_count = remote.len() as u64;

        // Shrink if they ballooned unexpectedly.
        if local.capacity() > 64 && local.len() < 8 {
            local.shrink_to(8);
        }
        if remote.capacity() > 32 && remote.len() < 4 {
            remote.shrink_to(4);
        }

        self.local_scratch = local;
        self.remote_scratch = remote;

        let inhibitor_count = if self.ignore_remote_media {
            local_count
        } else {
            local_count + remote_count
        };

        let state = if local_count > 0 {
            MediaState::PlayingLocal
        } else if remote_count > 0 {
            MediaState::PlayingRemote
        } else {
            MediaState::Idle
        };

        let first_poll = self.last_count.is_none() && self.last_state.is_none();
        let prev_count = self.last_count.unwrap_or(0);

        let count_changed = !first_poll && self.last_count != Some(inhibitor_count);
        let state_changed = !first_poll && self.last_state != Some(state);

        if count_changed {
            eventline::info!(
                "media: count {} -> {} (local={}, remote={}, ignore_remote={})",
                prev_count,
                inhibitor_count,
                local_count,
                remote_count,
                self.ignore_remote_media
            );
        } else if (first_poll || self.force_emit) && inhibitor_count != 0 {
            eventline::info!(
                "media: count {} -> {} (local={}, remote={}, ignore_remote={})",
                0u64,
                inhibitor_count,
                local_count,
                remote_count,
                self.ignore_remote_media
            );
        }

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

// ---------------------------------------------------------------------------
// pactl parsing
// ---------------------------------------------------------------------------

fn read_pactl_snapshot(
    media_blacklist: &[Pattern],
    local_keys: &mut HashSet<String>,
    remote_keys: &mut HashSet<String>,
) -> Result<(), String> {
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
    parse_pactl_sink_inputs(&s, media_blacklist, local_keys, remote_keys);
    Ok(())
}

fn parse_pactl_sink_inputs(
    text: &str,
    media_blacklist: &[Pattern],
    local_keys: &mut HashSet<String>,
    remote_keys: &mut HashSet<String>,
) {
    let mut in_block = false;

    let mut seen_state = false;
    let mut is_running = false;
    let mut seen_corked = false;
    let mut corked = true;

    let mut app_name = String::new();
    let mut app_bin = String::new();
    let mut node_name = String::new();
    let mut media_name = String::new();
    let mut sink_str = String::new();
    let mut proc_id = String::new();

    macro_rules! flush {
        () => {
            if in_block {
                let playing = if seen_corked {
                    !corked
                } else if seen_state {
                    is_running
                } else {
                    false
                };

                if playing
                    && !looks_like_system_audio(&app_name, &app_bin, &node_name, &media_name)
                    && !looks_like_game(&app_name, &app_bin, &node_name)
                    && !is_blacklisted(media_blacklist, &app_name, &app_bin, &node_name, &media_name)
                {
                    let key = if !proc_id.is_empty() {
                        format!("pid:{proc_id}")
                    } else if !node_name.is_empty() {
                        format!("node:{}", node_name.to_lowercase())
                    } else if !app_name.is_empty() {
                        format!("app:{}", app_name.to_lowercase())
                    } else {
                        String::new()
                    };

                    if !key.is_empty() {
                        if is_remote_stream(&app_name, &app_bin, &node_name, &media_name, &sink_str) {
                            remote_keys.insert(key);
                        } else {
                            local_keys.insert(key);
                        }
                    }
                }
            }
        };
    }

    for line in text.lines() {
        let line = line.trim_end();

        if line.starts_with("Sink Input #") {
            flush!();

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
            is_running = rest.trim().eq_ignore_ascii_case("RUNNING");
            continue;
        }

        if let Some(rest) = l.strip_prefix("Corked:") {
            seen_corked = true;
            corked = rest.trim().eq_ignore_ascii_case("yes");
            continue;
        }

        if let Some(rest) = l.strip_prefix("Sink:") {
            sink_str.clear();
            sink_str.push_str(rest.trim());
            // lowercase in place for is_remote_stream checks
            sink_str.make_ascii_lowercase();
            continue;
        }

        if let Some((k, v)) = parse_pactl_kv(l) {
            match k {
                "application.name" => { app_name.clear(); app_name.push_str(v); }
                "application.process.binary" => { app_bin.clear(); app_bin.push_str(v); }
                "application.process.id" => { proc_id.clear(); proc_id.push_str(v); }
                "node.name" => { node_name.clear(); node_name.push_str(v); }
                "media.name" => { media_name.clear(); media_name.push_str(v); }
                _ => {}
            }
        }
    }

    flush!();
}

/// Returns a str slice from the value side of `key = "value"` lines without allocating.
fn parse_pactl_kv(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, '=');
    let k = parts.next()?.trim();
    let v = parts.next()?.trim();
    let v = v.strip_prefix('"').unwrap_or(v);
    let v = v.strip_suffix('"').unwrap_or(v);
    Some((k, v))
}

/// Check each field independently to avoid a format! allocation on every call.
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
    blacklist.iter().any(|p| {
        p.matches_lc(&app_name.to_lowercase())
            || p.matches_lc(&app_bin.to_lowercase())
            || p.matches_lc(&node_name.to_lowercase())
            || p.matches_lc(&media_name.to_lowercase())
    })
}

/// Check remote indicators per-field to avoid a format! allocation on every call.
fn is_remote_stream(
    app_name: &str,
    app_bin: &str,
    node_name: &str,
    media_name: &str,
    sink_str: &str, // already lowercased by the parser
) -> bool {
    const NEEDLES: &[&str] = &[
        "bluez", "raop", "airplay", "rtp", "rtsp", "tunnel", "network",
        "chromecast", "cast", "spotify", "connect", "sonos",
    ];

    let fields: [&str; 4] = [app_name, app_bin, node_name, media_name];

    for needle in NEEDLES {
        // sink_str is pre-lowercased
        if sink_str.contains(needle) {
            return true;
        }
        for field in &fields {
            // fields arrive in original case; do a case-insensitive contains
            if field.to_ascii_lowercase().contains(needle) {
                return true;
            }
        }
    }

    false
}

fn looks_like_system_audio(app_name: &str, app_bin: &str, node_name: &str, media_name: &str) -> bool {
    let bin_lc  = app_bin.to_ascii_lowercase();
    let app_lc  = app_name.to_ascii_lowercase();
    let node_lc = node_name.to_ascii_lowercase();
    let media_lc = media_name.to_ascii_lowercase();

    // Speech dispatcher
    if bin_lc == "sd_generic" || bin_lc == "sd_dummy" || bin_lc.starts_with("sd_") {
        return true;
    }
    if app_lc.starts_with("speech-dispatcher-") || node_lc.starts_with("speech-dispatcher-") {
        return true;
    }
    if app_lc == "speech-dispatcher" && media_lc == "playback" {
        return true;
    }

    // PipeWire / PulseAudio internal plumbing streams.
    // These are infrastructure sinks, not user media — no one should need
    // to add these to their personal blacklist.
    const SYSTEM_NEEDLES: &[&str] = &[
        "pwalarmd",              // PipeWire ALSA alarm daemon
        "pipewire-alsa",         // generic PipeWire ALSA bridge
        "pipewire-pulse",        // PipeWire PulseAudio compat layer
        "pw-alsa",               // alternate PipeWire ALSA bridge name
        "alsa_playback.pwalarm", // node.name variant of the above
    ];
    for needle in SYSTEM_NEEDLES {
        if app_lc.contains(needle)
            || bin_lc.contains(needle)
            || node_lc.contains(needle)
            || media_lc.contains(needle)
        {
            return true;
        }
    }

    // "ALSA Playback" with no meaningful app identity is almost always
    // internal infrastructure (e.g. pwalarmd, event-sound daemons).
    if media_lc == "alsa playback"
        && (app_lc.is_empty() || app_lc.starts_with("pipewire") || app_lc.contains("alsa"))
    {
        return true;
    }

    false
}

fn looks_like_game(app_name: &str, app_bin: &str, node_name: &str) -> bool {
    for field in &[app_name, app_bin, node_name] {
        let lc = field.to_ascii_lowercase();
        if lc.contains("wine64-preloader") || lc.contains("wine-preloader") {
            return true;
        }
        if lc.contains("steam") && lc.contains("steam_app_") {
            return true;
        }
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
