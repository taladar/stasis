// Author: Dustin Pilgrim
// License: MIT

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

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
                    // Version 1 is enough for our needs.
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
                // wayland-client gives WEnum<Capability> here.
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
        // Still useful, but no longer the *only* activity signal.
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

/// Spawnable Wayland service.
///
/// - Connects to Wayland from env
/// - Sets up ext_idle_notifier_v1 if available
/// - Also binds wl_pointer/wl_keyboard from wl_seat for immediate activity events
/// - Runs a blocking dispatch loop in a blocking task
pub async fn run_wayland(
    tx: mpsc::Sender<ManagerMsg>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), WaylandError> {
    // This can stay at 250ms now; pointer/keyboard provide immediate activity.
    let idle_timeout_ms: u32 = 250;

    eventline::info!("wayland: starting (idle_timeout_ms={})", idle_timeout_ms);

    let conn = Connection::connect_to_env().map_err(|e| WaylandError::Connect(e.to_string()))?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();

    let mut state = WaylandState::new(tx, idle_timeout_ms);

    // Bind globals
    let _registry = display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| WaylandError::Roundtrip(e.to_string()))?;

    // Enable idle notifications if supported
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
    let stop2 = Arc::clone(&stop);

    // Shutdown watcher
    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                stop2.store(true, Ordering::Relaxed);
                break;
            }
            if shutdown.changed().await.is_err() {
                stop2.store(true, Ordering::Relaxed);
                break;
            }
        }
    });

    // Run Wayland dispatch in a blocking task.
    tokio::task::spawn_blocking(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = event_queue.blocking_dispatch(&mut state) {
                let msg = e.to_string();
                // Keep it non-fatal; a compositor restart should just stop the service.
                eventline::error!("wayland: dispatch error: {}", msg);
                break;
            }
        }

        eventline::info!("wayland: stopping");
    });

    Ok(())
}
