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
//
// Key change: return io::Result so the caller can treat
// io::ErrorKind::AlreadyExists as a clean exit and avoid
// printing an extra wrapper ("Error: ...") on top of the message.

fn runtime_dir() -> io::Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))
}

fn lock_path() -> io::Result<PathBuf> {
    Ok(runtime_dir()?.join("stasis").join("stasis.lock"))
}

pub fn acquire_single_instance_lock() -> io::Result<UnixListener> {
    let path = lock_path()?;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match UnixListener::bind(&path) {
        Ok(l) => Ok(l),

        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // If we can connect, another instance is alive.
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "stasis is already running (another instance holds {})",
                        path.display()
                    ),
                ));
            }

            // Otherwise it's probably stale; remove and retry once.
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to bind instance lock {}: {e}", path.display()),
                )
            })
        }

        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("failed to bind instance lock {}: {e}", path.display()),
        )),
    }
}

// ---------------- wayland check ----------------

fn wayland_socket_path_probe() -> Result<PathBuf, String> {
    let rt = runtime_dir()
        .map_err(|e| format!("XDG_RUNTIME_DIR is not set (cannot probe wayland socket): {e}"))?;

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
    let sock = wayland_socket_path_probe().map_err(|probe_err| {
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

// ---------------- session liveness ----------------
//
// Watches only the Wayland socket. When the compositor goes away the socket
// becomes unconnectable and we shut down immediately (one failure = done).
//
// logind / login1 has been removed: it added latency (3 × 2 s = 6 s minimum)
// and complexity with no benefit. The Wayland socket is the ground truth for
// whether a compositor session is live; if it is gone, stasis has no input
// source and must not continue running.

pub fn spawn_wayland_socket_watcher(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    let sock = match wayland_socket_path() {
        Ok(p) => p,
        Err(e) => {
            eventline::warn!("wayland watcher disabled: {e}");
            return;
        }
    };

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            if *shutdown_tx.borrow() {
                break;
            }

            if UnixStream::connect(&sock).is_err() {
                // Socket is gone — compositor is dead. Shut down immediately.
                // Do not wait for multiple failures: every second we stay alive
                // without a compositor is a second the ticker runs uncontested.
                eventline::info!(
                    "wayland socket not connectable ({}); shutting down",
                    sock.display()
                );
                let _ = shutdown_tx.send(true);
                break;
            }
        }
    });
}
