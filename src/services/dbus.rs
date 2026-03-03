// Author: Dustin Pilgrim
// License: MIT

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
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
///
/// Uses a `current_thread` runtime rather than the default multi-thread one.
/// D-Bus listening is purely I/O-bound with no CPU parallelism needed; the
/// full multi-thread runtime was spending ~1–2 MB on worker-thread stacks and
/// work-stealing queues that were never used.
pub fn spawn_dbus_listeners(
    sink: Arc<dyn EventSink>,
    enable_loginctl: bool,
    shutdown: watch::Receiver<bool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current_thread runtime");

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
        match Proxy::new(
            &sys,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .await
        {
            Ok(proxy) => match proxy.receive_signal("PrepareForSleep").await {
                Ok(mut stream) => {
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
                Err(e) => {
                    eventline::warn!("D-Bus: could not subscribe to PrepareForSleep: {e:?}");
                }
            },
            Err(e) => {
                // Do NOT fall back to a different proxy — PrepareForSleep monitoring
                // is simply unavailable. Log and continue; lid/lock events may still work.
                eventline::warn!(
                    "D-Bus: login1 Manager proxy unavailable: {e:?}; sleep/wake monitoring disabled"
                );
            }
        }

        // 2) Lock/Unlock (login1 Session)
        match get_current_session_path(&sys).await {
            Ok(session_path) => {
                eventline::info!("D-Bus: monitoring session {}", session_path.as_str());

                match Proxy::new(
                    &sys,
                    "org.freedesktop.login1",
                    session_path,
                    "org.freedesktop.login1.Session",
                )
                .await
                {
                    Ok(proxy) => {
                        let lock_stream = proxy.receive_signal("Lock").await;
                        let unlock_stream = proxy.receive_signal("Unlock").await;

                        match (lock_stream, unlock_stream) {
                            (Ok(mut lock_stream), Ok(mut unlock_stream)) => {
                                let sink_lock = sink.clone();
                                tokio::spawn(async move {
                                    while let Some(_) = lock_stream.next().await {
                                        sink_lock.push(Event::SessionLocked { now_ms: now_ms() });
                                    }
                                });

                                let sink_unlock = sink.clone();
                                tokio::spawn(async move {
                                    while let Some(_) = unlock_stream.next().await {
                                        sink_unlock
                                            .push(Event::SessionUnlocked { now_ms: now_ms() });
                                    }
                                });
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                eventline::warn!(
                                    "D-Bus: could not subscribe to session Lock/Unlock: {e:?}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eventline::warn!("D-Bus: could not create session proxy: {e:?}");
                    }
                }
            }
            Err(e) => {
                eventline::warn!("D-Bus: could not resolve session path for lock/unlock: {e:?}");
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
                let parsed: (String, HashMap<String, Value>, Vec<String>) = match body.deserialize()
                {
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

    // Wait for shutdown; do NOT block forever.
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
    let uid: u32 = rustix::process::getuid().as_raw();

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

    // 4) Last-resort: PID method. Note: this will fail for processes running
    // outside a logind session (e.g. systemd --user services on some compositors).
    let pid = std::process::id();
    let path: zbus::zvariant::OwnedObjectPath = proxy.call("GetSessionByPID", &(pid,)).await?;
    Ok(path)
}
