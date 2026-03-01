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
        .with_poll_interval_ms(500)
        // Tune if needed:
        // .with_chromium_single_grace_ms(30_000)
        ;

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

    // Heuristic: Chromium-family can leave one uncorked zombie stream forever.
    // If the ONLY playing stream is a single Chromium/Vivaldi stream for too long,
    // treat it as phantom and ignore it.
    chromium_single_grace_ms: u64,
    chromium_single_since_ms: Option<u64>,
    chromium_single_ignored_logged: bool,
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

            chromium_single_grace_ms: 1_500,
            chromium_single_since_ms: None,
            chromium_single_ignored_logged: false,
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    #[allow(dead_code)]
    pub fn with_chromium_single_grace_ms(mut self, ms: u64) -> Self {
        self.chromium_single_grace_ms = ms;
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

        let mut snapshot = match read_pactl_snapshot(&self.media_blacklist) {
            Ok(s) => s,
            Err(_e) => MediaSnapshot::default(),
        };

        // If no sinks are RUNNING, treat all sink-input activity as idle.
        // This prevents "uncorked zombie" streams from inhibiting forever.
        // (Chromium/Discord zombie after leaving a call hits this path.)
        // call_keys (source-outputs) are NOT cleared here — mic capture is
        // independent of sink state.
        if !snapshot.any_sink_running {
            snapshot.local_keys.clear();
            snapshot.remote_keys.clear();

            // also reset chromium heuristic state
            self.chromium_single_since_ms = None;
            self.chromium_single_ignored_logged = false;
        }

        // Heuristic: if the ONLY playing audio activity is a single Chromium-family stream,
        // and it persists, treat as phantom and ignore it.
        let only_one_playing_stream = snapshot.playing_streams_total == 1;
        let that_one_is_chromium = snapshot.playing_streams_chromium == 1;

        if self.chromium_single_grace_ms > 0 && only_one_playing_stream && that_one_is_chromium {
            match self.chromium_single_since_ms {
                None => {
                    self.chromium_single_since_ms = Some(now_ms);
                    self.chromium_single_ignored_logged = false;
                }
                Some(since) => {
                    if now_ms.saturating_sub(since) >= self.chromium_single_grace_ms {
                        snapshot.local_keys.clear();
                        snapshot.remote_keys.clear();

                        if !self.chromium_single_ignored_logged {
                            eventline::info!(
                                "media: ignoring single chromium stream after {}ms (phantom heuristic)",
                                self.chromium_single_grace_ms
                            );
                            self.chromium_single_ignored_logged = true;
                        }
                    }
                }
            }
        } else {
            self.chromium_single_since_ms = None;
            self.chromium_single_ignored_logged = false;
        }

        let local = snapshot.local_keys.len() as u64;
        let remote = snapshot.remote_keys.len() as u64;
        // source-outputs: always count as local inhibitors regardless of ignore_remote_media
        let call = snapshot.call_keys.len() as u64;

        let inhibitor_count = if self.ignore_remote_media {
            local + call
        } else {
            local + remote + call
        };

        let state = if local > 0 || call > 0 {
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
                "media: count {} -> {} (local={}, remote={}, call={}, ignore_remote={}, sinks_running={}, streams_total={}, streams_chromium={})",
                prev_count,
                inhibitor_count,
                local,
                remote,
                call,
                self.ignore_remote_media,
                snapshot.any_sink_running,
                snapshot.playing_streams_total,
                snapshot.playing_streams_chromium
            );
        } else if (first_poll || self.force_emit) && inhibitor_count != 0 {
            eventline::info!(
                "media: count {} -> {} (local={}, remote={}, call={}, ignore_remote={}, sinks_running={}, streams_total={}, streams_chromium={})",
                0u64,
                inhibitor_count,
                local,
                remote,
                call,
                self.ignore_remote_media,
                snapshot.any_sink_running,
                snapshot.playing_streams_total,
                snapshot.playing_streams_chromium
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
    // dedup: each "app" counts once even if multiple sink-inputs exist
    local_keys: HashSet<String>,
    remote_keys: HashSet<String>,

    // source-outputs: any active mic capture counts as a call inhibitor
    call_keys: HashSet<String>,

    // global "is the audio device actually active?"
    any_sink_running: bool,

    // Heuristic inputs:
    // How many sink-input blocks look "playing" (uncorked/running), irrespective of dedup.
    playing_streams_total: u64,
    playing_streams_chromium: u64,
}

fn read_pactl_snapshot(media_blacklist: &[Pattern]) -> Result<MediaSnapshot, String> {
    // --- sink-inputs (per-stream) ---
    let out_inputs = Command::new("pactl")
        .arg("list")
        .arg("sink-inputs")
        .output()
        .map_err(|e| format!("failed to run pactl sink-inputs: {e}"))?;

    if !out_inputs.status.success() {
        return Err(format!(
            "pactl sink-inputs exited non-zero: {}",
            out_inputs.status.code().unwrap_or(-1)
        ));
    }

    // --- sinks (device-level state) ---
    // If this fails, DO NOT force-unpause: assume running (safe default).
    let any_sink_running = match Command::new("pactl").arg("list").arg("sinks").output() {
        Ok(out_sinks) if out_sinks.status.success() => {
            let sinks_txt = String::from_utf8_lossy(&out_sinks.stdout);
            parse_pactl_sinks_any_running(&sinks_txt)
        }
        _ => true,
    };

    // --- source-outputs (mic capture / call streams) ---
    let (call_keys, capturing_pids) = match Command::new("pactl").arg("list").arg("source-outputs").output() {
        Ok(out) if out.status.success() => {
            let txt = String::from_utf8_lossy(&out.stdout);
            parse_pactl_source_outputs(&txt)
        }
        _ => (HashSet::new(), HashSet::new()),
    };

    let inputs_txt = String::from_utf8_lossy(&out_inputs.stdout);
    let mut snap = parse_pactl_sink_inputs(&inputs_txt, media_blacklist, &capturing_pids);
    snap.any_sink_running = any_sink_running;
    snap.call_keys = call_keys;
    Ok(snap)
}

fn parse_pactl_sinks_any_running(text: &str) -> bool {
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("State:") {
            if rest.trim().eq_ignore_ascii_case("RUNNING") {
                return true;
            }
        }
    }
    false
}

/// Returns keys for all active (uncorked) source-output streams, and
/// the set of PIDs that have active captures (used to suppress Firefox call sink-inputs).
fn parse_pactl_source_outputs(text: &str) -> (HashSet<String>, HashSet<String>) {
    let mut keys: HashSet<String> = HashSet::new();
    let mut capturing_pids: HashSet<String> = HashSet::new();

    let mut in_block = false;
    let mut seen_state = false;
    let mut is_running = false;
    let mut seen_corked = false;
    let mut corked = true;

    let mut app_name = String::new();
    let mut app_bin = String::new();
    let mut node_name = String::new();
    let mut proc_id = String::new();
    let mut object_serial = String::new();

    macro_rules! flush {
        () => {
            if in_block {
                let playing = if seen_corked { !corked } else if seen_state { is_running } else { false };
                if playing {
                    let key = if !object_serial.is_empty() {
                        format!("src:serial:{object_serial}")
                    } else if !proc_id.is_empty() {
                        format!("src:pid:{proc_id}")
                    } else if !node_name.is_empty() {
                        format!("src:node:{}", node_name.to_lowercase())
                    } else if !app_name.is_empty() {
                        format!("src:app:{}", app_name.to_lowercase())
                    } else {
                        String::new()
                    };
                    if !key.is_empty() {
                        keys.insert(key);
                    }
                    // Track which PIDs are actively capturing so we can suppress
                    // their Firefox sink-inputs (call audio, not media playback).
                    if !proc_id.is_empty() {
                        capturing_pids.insert(proc_id.clone());
                    }
                }
            }
        };
    }

    for line in text.lines() {
        let line = line.trim_end();

        if line.starts_with("Source Output #") {
            flush!();
            in_block = true;
            seen_state = false; is_running = false;
            seen_corked = false; corked = true;
            app_name.clear(); app_bin.clear(); node_name.clear();
            proc_id.clear(); object_serial.clear();
            continue;
        }

        if !in_block { continue; }
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
        if let Some((k, v)) = parse_pactl_kv(l) {
            match k {
                "application.name"           => app_name = v,
                "application.process.binary" => app_bin = v,
                "application.process.id"     => proc_id = v,
                "node.name"                  => node_name = v,
                "object.serial"              => object_serial = v,
                _ => {}
            }
        }
    }
    flush!();

    (keys, capturing_pids)
}

fn parse_pactl_sink_inputs(text: &str, media_blacklist: &[Pattern], capturing_pids: &HashSet<String>) -> MediaSnapshot {
    let mut local_keys: HashSet<String> = HashSet::new();
    let mut remote_keys: HashSet<String> = HashSet::new();

    let mut playing_streams_total: u64 = 0;
    let mut playing_streams_chromium: u64 = 0;

    let mut in_block = false;

    // We consider playing if corked==false, otherwise fall back to RUNNING.
    // NOTE: On PipeWire-pulse, sink-input blocks often lack "State:" entirely.
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
    let mut object_serial = String::new(); // helps Firefox per-tab counting

    macro_rules! flush {
        () => {
            if in_block {
                let playing = if seen_corked { !corked } else if seen_state { is_running } else { false };

                if playing {
                    // Skip obvious games
                    if looks_like_game(&app_name, &app_bin, &node_name) {
                        // no-op, fall through to end of block
                    }
                    // Suppress call audio sink-inputs:
                    // 1. PID is capturing mic AND media.name is generic — call voice stream.
                    //    Vivaldi reports "Playback" for everything, so we can only suppress it
                    //    when the PID is confirmed in a call via source-output.
                    //    Vivaldi YouTube (no source-output) keeps its generic name and passes.
                    // 2. Firefox Discord tab by media.name — always suppress regardless of mic.
                    else if (capturing_pids.contains(&proc_id) && is_generic_media_name(&media_name))
                        || (is_firefox(&app_name, &app_bin, &node_name) && looks_like_discord_tab(&media_name))
                    {
                        // no-op — intentionally excluded from heuristic counters too
                    }
                    // Blacklist
                    else if is_blacklisted(media_blacklist, &app_name, &app_bin, &node_name, &media_name) {
                        // no-op
                    }
                    else {
                        // Count toward heuristics ONLY after filtering.
                        // This ensures the chromium single-stream heuristic fires correctly
                        // even when a filtered-out Firefox Discord tab is also uncorked.
                        playing_streams_total += 1;
                        if looks_like_chromium(&app_name, &app_bin, &node_name) {
                            playing_streams_chromium += 1;
                        }
                        let remote = is_remote_stream(&app_name, &app_bin, &node_name, &media_name, &sink_str);
                        let is_ff = is_firefox(&app_name, &app_bin, &node_name);

                        // Dedup key strategy:
                        // - Firefox: dedup by media.name (tab title) so multiple playing tabs
                        //   each count once, but PipeWire duplicate sink-inputs for the same
                        //   tab (same title, different serials) collapse to one.
                        // - Non-Firefox: pid-based dedup to avoid overcount for multi-stream apps.
                        let key = if is_ff {
                            if !media_name.is_empty() {
                                format!("media:{}", media_name.to_lowercase())
                            } else if !object_serial.is_empty() {
                                format!("serial:{object_serial}")
                            } else if !node_name.is_empty() {
                                format!("node:{}", node_name.to_lowercase())
                            } else {
                                String::new()
                            }
                        } else if !proc_id.is_empty() {
                            format!("pid:{proc_id}")
                        } else if !node_name.is_empty() {
                            format!("node:{}", node_name.to_lowercase())
                        } else if !app_name.is_empty() {
                            format!("app:{}", app_name.to_lowercase())
                        } else {
                            String::new()
                        };

                        if !key.is_empty() {
                            if remote { remote_keys.insert(key); } else { local_keys.insert(key); }
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
            seen_state = false; is_running = false;
            seen_corked = false; corked = true;
            app_name.clear(); app_bin.clear(); node_name.clear();
            media_name.clear(); sink_str.clear(); proc_id.clear(); object_serial.clear();
            continue;
        }

        if !in_block { continue; }
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
            sink_str = rest.trim().to_lowercase();
            continue;
        }
        if let Some((k, v)) = parse_pactl_kv(l) {
            match k {
                "application.name"           => app_name = v,
                "application.process.binary" => app_bin = v,
                "application.process.id"     => proc_id = v,
                "node.name"                  => node_name = v,
                "media.name"                 => media_name = v,
                "object.serial"              => object_serial = v,
                _ => {}
            }
        }
    }
    flush!();

    MediaSnapshot {
        local_keys,
        remote_keys,
        call_keys: HashSet::new(), // filled by read_pactl_snapshot()
        any_sink_running: true,    // overwritten by read_pactl_snapshot()
        playing_streams_total,
        playing_streams_chromium,
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
    if blacklist.is_empty() { return false; }
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
        "bluez", "raop", "airplay", "rtp", "rtsp", "tunnel",
        "network", "chromecast", "cast", "spotify", "connect", "sonos",
    ];
    NEEDLES.iter().any(|n| hay.contains(n))
}

fn is_firefox(app_name: &str, app_bin: &str, node_name: &str) -> bool {
    let hay = format!("{app_name} {app_bin} {node_name}").to_lowercase();
    hay.contains("firefox")
}

fn looks_like_chromium(app_name: &str, app_bin: &str, node_name: &str) -> bool {
    let hay = format!("{app_name} {app_bin} {node_name}").to_lowercase();
    hay.contains("vivaldi")
        || hay.contains("chromium")
        || hay.contains("chrome")
        || hay.contains("brave")
        || hay.contains("microsoft-edge")
        || hay.contains("msedge")
}

/// Chromium zombie filter: generic placeholder names that indicate Discord/WebRTC
/// background streams rather than real media playback.
fn is_generic_media_name(media_name: &str) -> bool {
    matches!(media_name.to_lowercase().trim(), "playback" | "audiostream" | "audio stream" | "")
}

/// Firefox-only: skip sink-inputs whose media.name looks like a Discord tab.
/// "• Discord | General | TuTu's server" → matches.
/// Chromium Discord is handled upstream by any_sink_running + chromium_single heuristic.
fn looks_like_discord_tab(media_name: &str) -> bool {
    media_name.to_lowercase().contains("discord")
}

fn looks_like_game(app_name: &str, app_bin: &str, node_name: &str) -> bool {
    let hay = format!("{app_name} {app_bin} {node_name}").to_lowercase();
    hay.contains("wine64-preloader") || hay.contains("wine-preloader")
        || (hay.contains("steam") && hay.contains("steam_app_"))
}

fn pattern_key(p: &Pattern) -> String {
    match p {
        Pattern::Literal(s) => s.clone(),
        Pattern::Regex(r) => format!("/{}/", r.as_str()),
    }
}

fn patterns_same(a: &[Pattern], b: &[Pattern]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().map(pattern_key).zip(b.iter().map(pattern_key)).all(|(x, y)| x == y)
}
