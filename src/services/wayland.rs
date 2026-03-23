// Author: Dustin Pilgrim
// License: GPL-3.0-only

use rustix::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    protocol::{
        wl_keyboard::WlKeyboard,
        wl_pointer::WlPointer,
        wl_registry,
        wl_seat::{self, WlSeat},
    },
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{Event as IdleEvent, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
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

    // Direct input listeners so activity is immediate (best-effort; focus rules apply)
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,

    idle_timeout_ms: u32,
    compositor_edge_seen: Arc<AtomicBool>,
}

impl WaylandState {
    fn new(
        tx: mpsc::Sender<ManagerMsg>,
        idle_timeout_ms: u32,
        compositor_edge_seen: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            idle_notifier: None,
            seat: None,
            notification: None,
            pointer: None,
            keyboard: None,
            idle_timeout_ms,
            compositor_edge_seen,
        }
    }

    fn emit_activity(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self.tx.try_send(ManagerMsg::Event(Event::UserActivity {
            kind: ActivityKind::Any,
            now_ms,
        }));
    }

    fn emit_compositor_idled(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self
            .tx
            .try_send(ManagerMsg::Event(Event::CompositorIdled { now_ms }));
    }

    fn emit_compositor_resumed(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self
            .tx
            .try_send(ManagerMsg::Event(Event::CompositorResumed { now_ms }));
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
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
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
        // no-op: ExtIdleNotifierV1 is a factory/manager object.
        // Events are on ExtIdleNotificationV1.
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
                state.compositor_edge_seen.store(true, Ordering::Relaxed);
                // Note: niri may not send this reliably, but when it does, treat as activity.
                state.emit_compositor_resumed();
                state.emit_activity();
            }
            IdleEvent::Idled => {
                state.compositor_edge_seen.store(true, Ordering::Relaxed);
                state.emit_compositor_idled();
                // Intentionally do NOT drive plan timing from this event.
                // Tick remains the source of truth for time-based firing.
            }
            _ => {}
        }
    }
}

/// Poll the Wayland connection fd with a timeout (milliseconds).
/// Returns true if data is available, false on timeout.
fn poll_wayland_fd(fd: std::os::unix::io::RawFd, timeout_ms: i32) -> Result<bool, String> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;

    // Match poll(2) semantics: negative timeout means "infinite".
    let timeout_ts: Option<Timespec> = if timeout_ms < 0 {
        None
    } else {
        Some(
            Timespec::try_from(Duration::from_millis(timeout_ms as u64))
                .map_err(|_| "poll failed: invalid timeout".to_string())?,
        )
    };

    // SAFETY: We only use this borrowed fd for the duration of this call.
    let bfd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut fds = [PollFd::from_borrowed_fd(bfd, PollFlags::IN)];

    match poll(&mut fds, timeout_ts.as_ref()) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(true),
        Err(Errno::INTR) => Ok(false),
        Err(e) => Err(format!("poll failed: {e}")),
    }
}

/// Spawnable Wayland service.
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

    let compositor_edge_seen = Arc::new(AtomicBool::new(false));
    let tx_bootstrap = tx.clone();
    let mut state = WaylandState::new(tx, idle_timeout_ms, compositor_edge_seen.clone());

    let _registry = display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| WaylandError::Roundtrip(e.to_string()))?;

    if let (Some(notifier), Some(seat)) = (&state.idle_notifier, &state.seat) {
        let notification = notifier.get_idle_notification(state.idle_timeout_ms, seat, &qh, ());
        state.notification = Some(notification);
        eventline::info!("wayland: ext_idle_notifier_v1 active");

        let mut shutdown_bootstrap = shutdown.clone();
        let edge_seen = compositor_edge_seen.clone();
        let boot_delay_ms = idle_timeout_ms as u64 + 150;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(boot_delay_ms)) => {
                    if !edge_seen.load(Ordering::Relaxed) {
                        eventline::debug!("wayland: startup bootstrap compositor-idled");
                        let now_ms = crate::core::utils::now_ms();
                        let _ = tx_bootstrap.try_send(ManagerMsg::Event(Event::CompositorIdled { now_ms }));
                    }
                }
                _ = shutdown_bootstrap.changed() => {}
            }
        });
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
        eventline::info!("wayland: compositor disconnected; shutting down");
        let _ = shutdown_tx.send(true);
    } else {
        eventline::info!("wayland: stopping");
    }

    Ok(())
}
