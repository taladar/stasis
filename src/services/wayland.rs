// Author: Dustin Pilgrim
// License: MIT

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::os::fd::AsFd;
use tokio::sync::{mpsc, watch};

use wayland_client::{
    protocol::{
        wl_keyboard::WlKeyboard,
        wl_pointer::WlPointer,
        wl_registry,
        wl_seat::{self, WlSeat},
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notifier_v1::ExtIdleNotifierV1,
    ext_idle_notification_v1::{Event as IdleEvent, ExtIdleNotificationV1},
};

use crate::core::events::{ActivityKind, Event};
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug)]
pub enum WaylandError {
    Connect(String),
    Roundtrip(String),
}

impl std::fmt::Display for WaylandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaylandError::Connect(s) => write!(f, "wayland connect failed: {s}"),
            WaylandError::Roundtrip(s) => write!(f, "wayland roundtrip failed: {s}"),
        }
    }
}

impl std::error::Error for WaylandError {}

struct WaylandState {
    tx: mpsc::Sender<ManagerMsg>,

    idle_notifier: Option<ExtIdleNotifierV1>,
    seat: Option<WlSeat>,
    notification: Option<ExtIdleNotificationV1>,

    // Direct input listeners so activity is immediate (no idle-notify edge cases)
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,

    idle_timeout_ms: u32,
}

impl WaylandState {
    fn new(tx: mpsc::Sender<ManagerMsg>, idle_timeout_ms: u32) -> Self {
        Self {
            tx,
            idle_notifier: None,
            seat: None,
            notification: None,
            pointer: None,
            keyboard: None,
            idle_timeout_ms,
        }
    }

    fn emit_activity(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self.tx.try_send(ManagerMsg::Event(Event::UserActivity {
            kind: ActivityKind::Any,
            now_ms,
        }));
    }
}

// ---------------- Registry binding ----------------

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "ext_idle_notifier_v1" => {
                    state.idle_notifier =
                        Some(registry.bind::<ExtIdleNotifierV1, _, _>(name, 1, qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // no-op
    }
}

// ---------------- Seat capabilities -> bind pointer/keyboard ----------------

impl Dispatch<WlSeat, ()> for WaylandState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Capabilities { capabilities } => {
                let caps = match capabilities {
                    WEnum::Value(c) => c,
                    WEnum::Unknown(_) => return,
                };

                if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                    state.pointer = Some(seat.get_pointer(qh, ()));
                    eventline::info!("wayland: wl_pointer active");
                }

                if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    eventline::info!("wayland: wl_keyboard active");
                }
            }
            wl_seat::Event::Name { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wayland_client::protocol::wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_pointer::Event as PE;
        match event {
            PE::Motion { .. }
            | PE::Button { .. }
            | PE::Axis { .. }
            | PE::AxisDiscrete { .. }
            | PE::AxisValue120 { .. }
            | PE::AxisStop { .. }
            | PE::AxisSource { .. } => {
                state.emit_activity();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wayland_client::protocol::wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_keyboard::Event as KE;
        match event {
            KE::Key { .. } => {
                state.emit_activity();
            }
            _ => {}
        }
    }
}

// ---------------- Idle notifications ----------------

impl Dispatch<ExtIdleNotificationV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: IdleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            IdleEvent::Resumed => {
                state.emit_activity();
            }
            IdleEvent::Idled => {
                // intentionally ignored; core timing is driven by Tick
            }
            _ => {}
        }
    }
}

/// Poll the Wayland connection fd with a timeout (milliseconds).
/// Returns true if data is available, false on timeout.
fn poll_wayland_fd(fd: std::os::unix::io::RawFd, timeout_ms: i32) -> Result<bool, String> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
    match ret {
        -1 => {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(format!("poll failed: {err}"))
            }
        }
        0 => Ok(false),
        _ => Ok(true),
    }
}

/// Spawnable Wayland service.
///
/// Now accepts `shutdown_tx` so that when the compositor disconnects for any
/// reason, the daemon shuts down immediately. This is the correct behaviour:
/// stasis is a per-session idle manager and has no useful work to do without
/// a live compositor. The previous behaviour was to return `Ok(())` silently,
/// leaving the daemon alive with no input source whatsoever. The ticker would
/// then keep firing, `maybe_fire_next_step` would advance the plan uncontested,
/// and when the user returned to a compositor there was nothing resetting the
/// idle timer so input appeared to be ignored.
///
/// The dispatch loop uses poll(2) with a short timeout rather than
/// `blocking_dispatch` so the stop flag is checked regularly, preventing the
/// blocking thread from hanging on exit (which previously kept the process
/// alive past its intended lifetime and blocked the instance lock socket).
pub async fn run_wayland(
    tx: mpsc::Sender<ManagerMsg>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), WaylandError> {
    let idle_timeout_ms: u32 = 250;

    eventline::info!("wayland: starting (idle_timeout_ms={})", idle_timeout_ms);

    let conn = Connection::connect_to_env().map_err(|e| WaylandError::Connect(e.to_string()))?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();

    let mut state = WaylandState::new(tx, idle_timeout_ms);

    let _registry = display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| WaylandError::Roundtrip(e.to_string()))?;

    if let (Some(notifier), Some(seat)) = (&state.idle_notifier, &state.seat) {
        let notification = notifier.get_idle_notification(state.idle_timeout_ms, seat, &qh, ());
        state.notification = Some(notification);
        eventline::info!("wayland: ext_idle_notifier_v1 active");
    } else {
        eventline::warn!(
            "wayland: ext_idle_notifier_v1 or wl_seat missing; idle notifier disabled"
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_dispatch = Arc::clone(&stop);

    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            if shutdown.changed().await.is_err() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    });

    let wayland_fd = {
        use std::os::unix::io::AsRawFd;
        conn.as_fd().as_raw_fd()
    };

    // Returns true if the compositor went away, false if we stopped cleanly.
    let join = tokio::task::spawn_blocking(move || {
        const POLL_TIMEOUT_MS: i32 = 200;
        let mut compositor_gone = false;

        loop {
            if stop_dispatch.load(Ordering::Relaxed) {
                break;
            }

            if let Err(e) = event_queue.flush() {
                eventline::error!("wayland: flush error: {}", e);
                compositor_gone = true;
                break;
            }

            match event_queue.prepare_read() {
                Some(read_guard) => match poll_wayland_fd(wayland_fd, POLL_TIMEOUT_MS) {
                    Ok(true) => {
                        if let Err(e) = read_guard.read() {
                            eventline::error!("wayland: read error: {}", e);
                            compositor_gone = true;
                            break;
                        }
                        if let Err(e) = event_queue.dispatch_pending(&mut state) {
                            eventline::error!("wayland: dispatch error: {}", e);
                            compositor_gone = true;
                            break;
                        }
                    }
                    Ok(false) => {
                        drop(read_guard); // timeout, check stop flag
                    }
                    Err(e) => {
                        eventline::error!("wayland: poll error: {}", e);
                        compositor_gone = true;
                        break;
                    }
                },
                None => {
                    // Events already queued; dispatch without reading.
                    if let Err(e) = event_queue.dispatch_pending(&mut state) {
                        eventline::error!("wayland: dispatch error: {}", e);
                        compositor_gone = true;
                        break;
                    }
                }
            }
        }

        compositor_gone
    });

    let compositor_gone = join.await.unwrap_or_else(|e| {
        eventline::error!("wayland: blocking task panicked: {:?}", e);
        true
    });

    if compositor_gone {
        // Compositor is gone — shut the whole daemon down immediately.
        // There is no point staying alive: no input source exists, the plan
        // will run uncontested, and the user cannot interrupt it on return.
        eventline::info!("wayland: compositor disconnected; shutting down");
        let _ = shutdown_tx.send(true);
    } else {
        eventline::info!("wayland: stopping");
    }

    Ok(())
}
