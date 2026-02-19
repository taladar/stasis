// Author: Dustin Pilgrim
// License: MIT

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use tokio::runtime::Runtime;
use tokio::sync::watch;
use zbus::{Connection, MatchRule, Proxy};

use crate::core::events::Event;

/// Sink for pushing events into the (sync) manager loop.
/// Implement this for whatever channel/queue you're using.
pub trait EventSink: Send + Sync + 'static {
    fn push(&self, ev: Event);
}

/// Spawn D-Bus listeners.
///
/// `enable_loginctl` gates all login1-related monitoring:
/// - PrepareForSleep (org.freedesktop.login1.Manager)
/// - Lock/Unlock (org.freedesktop.login1.Session)
///
/// Lid events via UPower are always monitored.
pub fn spawn_dbus_listeners(
    sink: Arc<dyn EventSink>,
    enable_loginctl: bool,
    shutdown: watch::Receiver<bool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::spawn(move || {
        let rt = Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            if let Err(e) = run_dbus(sink, enable_loginctl, shutdown).await {
                eventline::error!("D-Bus listener failed: {e:?}");
            }
        });
    }))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn run_dbus(
    sink: Arc<dyn EventSink>,
    enable_loginctl: bool,
    mut shutdown: watch::Receiver<bool>,
) -> zbus::Result<()> {
    let sys = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            eventline::warn!("D-Bus: could not connect to system bus: {e:?}");
            return Ok(());
        }
    };

    if enable_loginctl {
        // 1) PrepareForSleep (login1 Manager)
        {
            let proxy = match Proxy::new(
                &sys,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    eventline::warn!("D-Bus: login1 Manager proxy unavailable: {e:?}");
                    // Keep running; lid events may still work.
                    Proxy::new(
                        &sys,
                        "org.freedesktop.DBus",
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                    )
                    .await?
                }
            };

            if let Ok(mut stream) = proxy.receive_signal("PrepareForSleep").await {
                let sink = sink.clone();
                tokio::spawn(async move {
                    while let Some(sig) = stream.next().await {
                        let going_down: bool = match sig.body().deserialize() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let t = now_ms();
                        sink.push(if going_down {
                            Event::PrepareForSleep { now_ms: t }
                        } else {
                            Event::ResumedFromSleep { now_ms: t }
                        });
                    }
                });
            }
        }

        // 2) Lock/Unlock via broadcast match rules on ALL session objects.
        //
        // WHY: The old approach resolved a specific session object path via
        // get_current_session_path() and subscribed only to that session's
        // signals. This breaks on NixOS (and any setup where the user's
        // systemd --user services start before logind has registered a session
        // for that UID), because:
        //   - XDG_SESSION_ID may not be set in the service environment
        //   - ListSessions returns nothing for the UID at startup
        //   - GetSessionByPID fails: "NoSessionForPID: PID N does not belong
        //     to any known session"
        // The session may appear later once the compositor is fully up, but
        // we already gave up.
        //
        // FIX: Use MatchRule-based broadcast listeners that catch Lock/Unlock
        // from ANY session object under /org/freedesktop/login1/session/*.
        // We never need to resolve a session path at all. Since Lock/Unlock are
        // only sent to the owning session and there is typically one graphical
        // session per user, this is safe and correct.
        //
        // NOTE: get_current_session_path() is kept below for potential future
        // use (e.g. reading session properties), but is no longer called here.
        {
            let lock_rule = match MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.login1.Session")?
                .member("Lock")?
                .build()
            {
                r => r,
            };

            let unlock_rule = match MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.login1.Session")?
                .member("Unlock")?
                .build()
            {
                r => r,
            };

            match zbus::MessageStream::for_match_rule(lock_rule, &sys, None).await {
                Ok(mut stream) => {
                    let sink = sink.clone();
                    eventline::info!("D-Bus: monitoring Lock signals from all sessions (broadcast)");
                    tokio::spawn(async move {
                        while let Some(msg) = stream.next().await {
                            if msg.is_ok() {
                                sink.push(Event::SessionLocked { now_ms: now_ms() });
                            }
                        }
                    });
                }
                Err(e) => {
                    eventline::warn!("D-Bus: could not subscribe to Lock signals: {e:?}");
                }
            }

            match zbus::MessageStream::for_match_rule(unlock_rule, &sys, None).await {
                Ok(mut stream) => {
                    let sink = sink.clone();
                    eventline::info!("D-Bus: monitoring Unlock signals from all sessions (broadcast)");
                    tokio::spawn(async move {
                        while let Some(msg) = stream.next().await {
                            if msg.is_ok() {
                                sink.push(Event::SessionUnlocked { now_ms: now_ms() });
                            }
                        }
                    });
                }
                Err(e) => {
                    eventline::warn!("D-Bus: could not subscribe to Unlock signals: {e:?}");
                }
            }
        }
    } else {
        eventline::info!("D-Bus: loginctl integration disabled; skipping login1 monitoring");
    }

    // 3) Lid events via UPower PropertiesChanged
    {
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .path("/org/freedesktop/UPower")?
            .build();

        let mut stream = zbus::MessageStream::for_match_rule(rule, &sys, None).await?;
        let sink = sink.clone();

        tokio::spawn(async move {
            use zbus::zvariant::Value;

            while let Some(msg) = stream.next().await {
                let Ok(msg) = msg else { continue };

                let body = msg.body();
                let parsed: (String, HashMap<String, Value>, Vec<String>) = match body.deserialize() {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let (iface, changed, _invalidated) = parsed;

                if iface != "org.freedesktop.UPower" {
                    continue;
                }

                if let Some(v) = changed.get("LidIsClosed") {
                    if let Ok(closed) = v.clone().downcast::<bool>() {
                        let t = now_ms();
                        sink.push(if closed {
                            Event::LidClosed { now_ms: t }
                        } else {
                            Event::LidOpened { now_ms: t }
                        });
                    }
                }
            }
        });
    }

    // ✅ IMPORTANT: do NOT block forever; exit this thread on shutdown.
    loop {
        if *shutdown.borrow() {
            break;
        }
        let _ = shutdown.changed().await;
        if *shutdown.borrow() {
            break;
        }
    }

    Ok(())
}

// ---- Session path resolution ----
//
// Kept for potential future use (reading session properties, etc.).
// No longer called for Lock/Unlock monitoring — see the broadcast match rule
// approach in run_dbus() above, which is NixOS-compatible.

#[allow(dead_code)]
async fn get_current_session_path(
    connection: &Connection,
) -> zbus::Result<zbus::zvariant::OwnedObjectPath> {
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;

    // 1) XDG_SESSION_ID if present
    if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
        let result: zbus::Result<zbus::zvariant::OwnedObjectPath> =
            proxy.call("GetSession", &(session_id.as_str(),)).await;

        if let Ok(path) = result {
            eventline::debug!("D-Bus: using session from XDG_SESSION_ID");
            return Ok(path);
        }
    }

    // 2) Search ListSessions for our UID, prefer graphical seat0
    let uid = unsafe { libc::getuid() };

    let sessions: Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)> =
        proxy.call("ListSessions", &()).await?;

    for (session_id, session_uid, _username, seat, path) in sessions.clone() {
        if session_uid != uid {
            continue;
        }

        if let Ok(sproxy) = Proxy::new(
            connection,
            "org.freedesktop.login1",
            path.clone(),
            "org.freedesktop.login1.Session",
        )
        .await
        {
            if let Ok(session_type) = sproxy.get_property::<String>("Type").await {
                if (session_type == "wayland" || session_type == "x11") && seat == "seat0" {
                    eventline::info!(
                        "D-Bus: selected graphical session '{}' (type: {}, seat: {})",
                        session_id,
                        session_type,
                        seat
                    );
                    return Ok(path);
                }
            }
        }
    }

    // 3) Fallback: first session for UID
    for (_session_id, session_uid, _username, _seat, path) in sessions {
        if session_uid == uid {
            eventline::warn!("D-Bus: using first session for UID {}", uid);
            return Ok(path);
        }
    }

    // 4) Last resort: PID method (fails on NixOS user services without PAMName=login)
    let pid = std::process::id();
    let path: zbus::zvariant::OwnedObjectPath = proxy.call("GetSessionByPID", &(pid,)).await?;
    Ok(path)
}
