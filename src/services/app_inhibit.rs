// Author: Dustin Pilgrim
// License: MIT

use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::core::config::Pattern;
use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug, Clone)]
pub struct AppRules {
    pub epoch: u64,
    pub apps: Vec<Pattern>,
}

/// Spawnable task: periodically polls app inhibitors and emits events on change.
///
/// Logging policy (INFO):
/// - If count changes: log "X -> Y"
/// - If we are forcing a refresh (profile/reload/rules changed) OR first poll:
///     log "0 -> N" ONLY when N != 0
pub async fn run_app_inhibit(
    tx: mpsc::Sender<ManagerMsg>,
    mut rules_rx: watch::Receiver<AppRules>,
) {
    let initial = rules_rx.borrow().clone();
    let mut last_epoch = initial.epoch;

    let mut svc = AppInhibitService::new(&initial.apps).with_poll_interval_ms(1000);

    eventline::info!("app_inhibit: started (backend={})", svc.backend_name());

    // Ensure we emit once immediately (and log 0->N if N!=0).
    svc.force_emit_next();

    let sleep_ms = 250u64;

    loop {
        tokio::select! {
            changed = rules_rx.changed() => {
                if changed.is_err() {
                    return;
                }

                let rules = rules_rx.borrow().clone();

                let epoch_bumped = rules.epoch != last_epoch;
                if epoch_bumped {
                    last_epoch = rules.epoch;
                    svc.force_emit_next();
                }

                svc.reconfigure(&rules.apps);

                let now_ms = crate::core::utils::now_ms();
                if let Some(ev) = svc.poll_async(now_ms).await {
                    if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                        return;
                    }
                }
            }

            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                let now_ms = crate::core::utils::now_ms();
                if let Some(ev) = svc.poll_async(now_ms).await {
                    if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct AppInhibitService {
    apps: Vec<Pattern>,
    backend: Backend,

    poll_interval_ms: u64,
    last_poll_ms: u64,

    last_count: Option<u64>, // None => never polled yet
    force_emit: bool,        // next poll must emit and do baseline logging
}

#[derive(Debug)]
enum Backend {
    Hyprland(HyprlandBackend),
    Niri(NiriBackend),
    Proc(ProcBackend),
}

#[derive(Debug, Default)]
struct HyprlandBackend {}

#[derive(Debug, Default)]
struct NiriBackend {}

#[derive(Debug, Default)]
struct ProcBackend {}

impl AppInhibitService {
    pub fn new(inhibit_apps: &[Pattern]) -> Self {
        let apps = normalize_patterns(inhibit_apps);
        let backend = detect_backend().unwrap_or_else(|| Backend::Proc(ProcBackend::default()));

        Self {
            apps,
            backend,
            poll_interval_ms: 1000,
            last_poll_ms: 0,
            last_count: None,
            force_emit: false,
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    pub fn reconfigure(&mut self, inhibit_apps: &[Pattern]) {
        let new_apps = normalize_patterns(inhibit_apps);

        if patterns_same(&self.apps, &new_apps) {
            return;
        }

        self.apps = new_apps;
        self.force_emit_next();

        eventline::info!(
            "app_inhibit: reconfigured (apps_len={}, backend={})",
            self.apps.len(),
            self.backend_name(),
        );
    }

    pub fn force_emit_next(&mut self) {
        self.force_emit = true;
        self.last_poll_ms = 0;
    }

    /// Async-aware poll. Subprocess queries run via `spawn_blocking` so the
    /// tokio executor thread is never blocked.
    pub async fn poll_async(&mut self, now_ms: u64) -> Option<Event> {
        if now_ms < self.last_poll_ms.saturating_add(self.poll_interval_ms) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let prev_count = self.last_count.unwrap_or(0);

        let count = if self.apps.is_empty() {
            0
        } else {
            match &self.backend {
                Backend::Hyprland(_) => {
                    let apps = self.apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        HyprlandBackend::default().count_matches_dedup(&apps)
                    })
                    .await
                    {
                        Ok(Ok(n)) => n,
                        Ok(Err(e)) => {
                            eventline::warn!(
                                "app_inhibit: hyprland query failed (keeping previous count={}): {}",
                                prev_count,
                                e
                            );
                            prev_count
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: hyprland task panicked (keeping previous count={}): {}",
                                prev_count,
                                e
                            );
                            prev_count
                        }
                    }
                }

                Backend::Niri(_) => {
                    let apps = self.apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        NiriBackend::default().count_matches_dedup(&apps)
                    })
                    .await
                    {
                        Ok(Ok(n)) => n,
                        Ok(Err(e)) => {
                            eventline::warn!(
                                "app_inhibit: niri query failed (keeping previous count={}): {}",
                                prev_count,
                                e
                            );
                            prev_count
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: niri task panicked (keeping previous count={}): {}",
                                prev_count,
                                e
                            );
                            prev_count
                        }
                    }
                }

                Backend::Proc(_) => {
                    let apps = self.apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        ProcBackend::default().count_matches_dedup(&apps)
                    })
                    .await
                    {
                        Ok(n) => n,
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: proc task panicked (keeping previous count={}): {}",
                                prev_count,
                                e
                            );
                            prev_count
                        }
                    }
                }
            }
        };

        let first_poll = self.last_count.is_none();
        let prev = self.last_count.unwrap_or(0);
        let changed = !first_poll && prev != count;

        if changed {
            eventline::info!(
                "app_inhibit: count {} -> {} (backend={}, apps_len={})",
                prev,
                count,
                self.backend_name(),
                self.apps.len()
            );
        } else if (first_poll || self.force_emit) && count != 0 {
            eventline::info!(
                "app_inhibit: count {} -> {} (backend={}, apps_len={})",
                0u64,
                count,
                self.backend_name(),
                self.apps.len()
            );
        }

        if first_poll || changed || self.force_emit {
            self.last_count = Some(count);
            self.force_emit = false;
            return Some(Event::AppInhibitorCount { count, now_ms });
        }

        None
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Hyprland(_) => "hyprland",
            Backend::Niri(_) => "niri",
            Backend::Proc(_) => "proc",
        }
    }
}

// ----------------------------- backend detection -----------------------------

fn detect_backend() -> Option<Backend> {
    detect_hyprland_backend().or_else(detect_niri_backend)
}

fn detect_hyprland_backend() -> Option<Backend> {
    if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return Some(Backend::Hyprland(HyprlandBackend::default()));
    }

    if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
        if desktop.to_lowercase().contains("hyprland") {
            return Some(Backend::Hyprland(HyprlandBackend::default()));
        }
    }

    None
}

fn detect_niri_backend() -> Option<Backend> {
    if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
        if desktop.to_lowercase().contains("niri") {
            return Some(Backend::Niri(NiriBackend::default()));
        }
    }

    if env::var("NIRI_SOCKET").is_ok() {
        return Some(Backend::Niri(NiriBackend::default()));
    }

    None
}

// ----------------------------- matching helpers -----------------------------

fn should_inhibit_app_id(app_id: &str, patterns: &[Pattern]) -> bool {
    if app_id.is_empty() {
        return false;
    }

    for pat in patterns {
        let matched = match pat {
            Pattern::Literal(s) => app_id_matches_static(s, app_id),
            Pattern::Regex(r) => r.is_match(app_id),
        };

        if matched {
            return true;
        }
    }

    false
}

fn app_id_matches_static(pattern: &str, app_id: &str) -> bool {
    if pattern.eq_ignore_ascii_case(app_id) {
        return true;
    }
    if app_id.ends_with(".exe") {
        let name = app_id.strip_suffix(".exe").unwrap_or(app_id);
        if pattern.eq_ignore_ascii_case(name) {
            return true;
        }
    }
    if let Some(last) = pattern.split('.').last() {
        if last.eq_ignore_ascii_case(app_id) {
            return true;
        }
    }
    false
}

// ----------------------------- Hyprland (hyprctl) -----------------------------

impl HyprlandBackend {
    fn count_matches_dedup(&self, apps: &[Pattern]) -> Result<u64, String> {
        let out = std::process::Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .map_err(|e| format!("hyprctl spawn failed: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("hyprctl clients -j failed: {}", err.trim()));
        }

        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("hyprctl json parse failed: {e}"))?;

        let arr = v
            .as_array()
            .ok_or_else(|| "hyprctl json: expected array".to_string())?;

        let mut seen: HashSet<String> = HashSet::new();

        for item in arr {
            let class = item.get("class").and_then(|x| x.as_str()).unwrap_or("");
            if class.is_empty() {
                continue;
            }

            if should_inhibit_app_id(class, apps) {
                seen.insert(class.to_string());
            }
        }

        Ok(seen.len() as u64)
    }
}

// ----------------------------- Niri (niri msg windows) -----------------------------

impl NiriBackend {
    fn count_matches_dedup(&self, apps: &[Pattern]) -> Result<u64, String> {
        let out = std::process::Command::new("niri")
            .args(["msg", "windows"])
            .output()
            .map_err(|e| format!("niri spawn failed: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("niri msg windows failed: {}", err.trim()));
        }

        let text = String::from_utf8_lossy(&out.stdout);

        let mut seen: HashSet<String> = HashSet::new();

        for line in text.lines() {
            let Some(rest) = line.strip_prefix("  App ID: ") else {
                continue;
            };

            let app_id = rest.trim().trim_matches('"');
            if app_id.is_empty() {
                continue;
            }

            if should_inhibit_app_id(app_id, apps) {
                seen.insert(app_id.to_string());
            }
        }

        Ok(seen.len() as u64)
    }
}

// ----------------------------- /proc fallback -----------------------------

impl ProcBackend {
    fn count_matches_dedup(&self, apps: &[Pattern]) -> u64 {
        let Ok(rd) = std::fs::read_dir("/proc") else {
            return 0;
        };

        let mut seen: HashSet<String> = HashSet::new();

        for ent in rd.flatten() {
            let pid_str = ent.file_name().to_string_lossy().to_string();
            if !pid_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }

            let pid_path = ent.path();

            if let Some(comm) = read_proc_comm(&pid_path) {
                if let Some(key) = proc_match_key(&comm, apps) {
                    seen.insert(key);
                    continue;
                }
            }

            if let Some(cmd) = read_proc_cmdline(&pid_path) {
                let argv0 = cmd.split_whitespace().next().unwrap_or("").to_string();
                if !argv0.is_empty() {
                    if let Some(key) = proc_match_key(&argv0, apps) {
                        seen.insert(key);
                        continue;
                    }
                }
            }
        }

        seen.len() as u64
    }
}

fn proc_match_key(hay: &str, apps: &[Pattern]) -> Option<String> {
    for p in apps {
        let matched = match p {
            Pattern::Literal(s) => hay.eq_ignore_ascii_case(s),
            Pattern::Regex(r) => r.is_match(hay),
        };
        if matched {
            return Some(hay.to_lowercase());
        }
    }
    None
}

fn read_proc_comm(pid_path: &Path) -> Option<String> {
    let p = pid_path.join("comm");
    let s = std::fs::read_to_string(p).ok()?;
    Some(s.trim().to_string())
}

fn read_proc_cmdline(pid_path: &Path) -> Option<String> {
    let p = pid_path.join("cmdline");
    let bytes = std::fs::read(p).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let s = bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect::<Vec<_>>()
        .join(" ");

    Some(s)
}

// ----------------------------- utils -----------------------------

fn normalize_patterns(inhibit_apps: &[Pattern]) -> Vec<Pattern> {
    inhibit_apps.iter().cloned().collect()
}

fn patterns_same(a: &[Pattern], b: &[Pattern]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .map(|p| p.to_string())
        .zip(b.iter().map(|p| p.to_string()))
        .all(|(x, y)| x == y)
}
