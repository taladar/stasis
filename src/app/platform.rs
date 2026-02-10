// Author: Dustin Pilgrim
// License: MIT

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

pub fn default_log_path() -> Option<PathBuf> {
    let state_base = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))?;

    Some(state_base.join("stasis").join("stasis.log"))
}


// ---------------- single-instance lock ----------------

fn runtime_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set (cannot create instance lock)".to_string())
}

fn lock_path() -> Result<PathBuf, String> {
    Ok(runtime_dir()?.join("stasis").join("stasis.lock"))
}

pub fn acquire_single_instance_lock() -> Result<UnixListener, String> {
    let path = lock_path()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match UnixListener::bind(&path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => match UnixStream::connect(&path) {
            Ok(_) => Err(format!(
                "stasis is already running (another instance holds {})",
                path.display()
            )),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                UnixListener::bind(&path)
                    .map_err(|e| format!("failed to bind instance lock {}: {e}", path.display()))
            }
        },
        Err(e) => Err(format!("failed to bind instance lock {}: {e}", path.display())),
    }
}

// ---------------- wayland check ----------------

fn wayland_socket_path_probe() -> Result<PathBuf, String> {
    let rt = runtime_dir()?;

    if let Ok(display) = std::env::var("WAYLAND_DISPLAY") {
        if !display.is_empty() {
            return Ok(rt.join(display));
        }
    }

    for entry in std::fs::read_dir(&rt)
        .map_err(|e| format!("failed to read {}: {e}", rt.display()))?
    {
        let entry =
            entry.map_err(|e| format!("failed to read entry in {}: {e}", rt.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.starts_with("wayland-") {
            continue;
        }

        let p = entry.path();
        if UnixStream::connect(&p).is_ok() {
            return Ok(p);
        }
    }

    Err(
        "WAYLAND_DISPLAY is not set and no connectable wayland-* socket was found in XDG_RUNTIME_DIR"
            .to_string(),
    )
}

pub fn ensure_wayland_alive() -> Result<(), String> {
    // Ground truth: a connectable Wayland socket.
    let sock = wayland_socket_path_probe().map_err(|probe_err| {
        // Only use XDG_SESSION_TYPE for diagnostics.
        let session_type =
            std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "<unset>".to_string());

        if session_type != "<unset>" && session_type != "wayland" {
            format!("not a wayland session: XDG_SESSION_TYPE={}", session_type)
        } else {
            probe_err
        }
    })?;

    UnixStream::connect(&sock)
        .map(|_| ())
        .map_err(|e| format!("failed to connect to wayland socket {}: {e}", sock.display()))
}

fn wayland_socket_path() -> Result<PathBuf, String> {
    wayland_socket_path_probe()
}

// ---------------- session liveness (login1) ----------------
//
// Socket-connectable does NOT always mean “session still active”.
// On VT switch or logout transitions, the compositor/socket may remain connectable.
// We therefore additionally consult logind (org.freedesktop.login1.Session.Active).
//
// This does not rely on systemd; it relies on logind over D-Bus.

async fn login1_session_active() -> Result<bool, String> {
    use zbus::{Connection, Proxy};
    use zbus::zvariant::OwnedObjectPath;

    let sys = Connection::system()
        .await
        .map_err(|e| format!("logind: could not connect to system bus: {e}"))?;

    let mgr = Proxy::new(
        &sys,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    .map_err(|e| format!("logind: failed to create Manager proxy: {e}"))?;

    // Use PID-based resolution; it works even when XDG_SESSION_TYPE/env isn't present.
    let pid = std::process::id() as u32;
    let (session_path,): (OwnedObjectPath,) = mgr
        .call("GetSessionByPID", &(pid,))
        .await
        .map_err(|e| format!("logind: GetSessionByPID({pid}) failed: {e}"))?;

    let sess = Proxy::new(
        &sys,
        "org.freedesktop.login1",
        session_path.as_str(),
        "org.freedesktop.login1.Session",
    )
    .await
    .map_err(|e| format!("logind: failed to create Session proxy: {e}"))?;

    let active: bool = sess
        .get_property("Active")
        .await
        .map_err(|e| format!("logind: failed to read Session.Active: {e}"))?;

    Ok(active)
}

pub fn spawn_wayland_socket_watcher(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    let sock = match wayland_socket_path() {
        Ok(p) => p,
        Err(e) => {
            eventline::warn!("wayland watcher disabled: {e}");
            return;
        }
    };

    tokio::spawn(async move {
        let mut socket_failures: u32 = 0;
        let mut inactive_failures: u32 = 0;

        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            if *shutdown_tx.borrow() {
                break;
            }

            // 1) Wayland socket liveness (compositor/socket really gone)
            if UnixStream::connect(&sock).is_err() {
                socket_failures += 1;
            } else {
                socket_failures = 0;
            }

            if socket_failures >= 3 {
                eventline::info!(
                    "wayland socket not connectable ({}); shutting down",
                    sock.display()
                );
                let _ = shutdown_tx.send(true);
                break;
            }

            // 2) Session liveness (covers VT switch / session end while socket may linger)
            match login1_session_active().await {
                Ok(true) => {
                    inactive_failures = 0;
                }
                Ok(false) => {
                    inactive_failures += 1;
                    if inactive_failures >= 3 {
                        eventline::info!("logind session inactive; shutting down");
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                }
                Err(e) => {
                    // If logind is unavailable/transiently failing, don't kill the app.
                    // Socket-based shutdown remains the backstop.
                    eventline::warn!("logind liveness probe failed: {e}");
                    inactive_failures = 0;
                }
            }
        }
    });
}
