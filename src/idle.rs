//! Idle detection via the Wayland `ext-idle-notify-v1` protocol.
//!
//! The compositor itself watches for user activity (keyboard, pointer,
//! touchscreen — everything, without reading key contents) and pushes
//! `idled` / `resumed` events over the Wayland connection. This is the
//! exact mechanism Omarchy's own shell uses (Quickshell IdleMonitor), so
//! the behaviour matches what the desktop already does — including
//! respecting idle inhibitors (video players, "stay awake" toggles): while
//! an inhibitor is active the compositor simply never reports idle.
//!
//! Runs on a dedicated OS thread with a blocking event loop; events are
//! forwarded to the daemon over the same channel as Hyprland events. No
//! polling anywhere: the thread sleeps inside the Wayland event loop until
//! the compositor has something to say.

use std::time::Duration;

use tokio::sync::mpsc;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

use crate::tracker::Input;

struct IdleState {
    tx: mpsc::Sender<Input>,
    timeout_ms: u32,
    manager: Option<ExtIdleNotifierV1>,
    seat: Option<WlSeat>,
    /// Keep the notification object alive for the whole session: dropping
    /// the last handle would tear down the protocol object.
    notification: Option<ExtIdleNotificationV1>,
}

impl IdleState {
    /// The manager global and the seat global arrive in arbitrary order;
    /// create the notification as soon as both are available.
    fn try_create_notification(&mut self, qh: &QueueHandle<Self>) {
        if self.notification.is_some() {
            return;
        }
        if let (Some(manager), Some(seat)) = (&self.manager, &self.seat) {
            self.notification = Some(manager.get_idle_notification(self.timeout_ms, seat, qh, ()));
        }
    }
}

impl Dispatch<WlRegistry, ()> for IdleState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "ext_idle_notifier_v1" => {
                    state.manager = Some(registry.bind(name, version.min(2), qh, ()));
                    state.try_create_notification(qh);
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version.min(1), qh, ()));
                    state.try_create_notification(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for IdleState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // capabilities/name events are irrelevant for idle monitoring.
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for IdleState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtIdleNotifierV1,
        _event: ext_idle_notifier_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The manager object has no events.
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for IdleState {
    fn event(
        state: &mut Self,
        _proxy: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                let _ = state.tx.blocking_send(Input::Idle);
            }
            ext_idle_notification_v1::Event::Resumed => {
                let _ = state.tx.blocking_send(Input::Active);
            }
            _ => {}
        }
    }
}

/// Spawn the idle monitor on a dedicated thread. Never panics; connection
/// failures are logged and retried with backoff (e.g. compositor restart).
pub fn spawn(tx: mpsc::Sender<Input>, timeout_seconds: u32) {
    // The protocol timeout is in milliseconds; clamp to at least 1s so a
    // misconfigured value cannot spin the compositor.
    let timeout_ms = timeout_seconds.saturating_mul(1000).max(1000);
    let _ = std::thread::Builder::new()
        .name("atw-idle".to_string())
        .spawn(move || run_loop(tx, timeout_ms));
}

fn run_loop(tx: mpsc::Sender<Input>, timeout_ms: u32) {
    let mut backoff: u64 = 1;
    loop {
        match run_once(&tx, timeout_ms) {
            Ok(()) => return, // daemon is shutting down
            Err(e) => {
                tracing::warn!(target: "atw", error = %e, "idle monitor unavailable; retrying");
            }
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(60);
    }
}

fn run_once(tx: &mpsc::Sender<Input>, timeout_ms: u32) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
    let mut queue = conn.new_event_queue::<IdleState>();
    let qh = queue.handle();
    let mut state = IdleState {
        tx: tx.clone(),
        timeout_ms,
        manager: None,
        seat: None,
        notification: None,
    };
    conn.display().get_registry(&qh, ());
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("wayland roundtrip: {e}"))?;
    if state.notification.is_none() {
        return Err("compositor does not advertise ext_idle_notifier_v1 + wl_seat".to_string());
    }
    tracing::info!(target: "atw", timeout_ms, "idle monitor armed");
    loop {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("wayland dispatch: {e}"))?;
        if tx.is_closed() {
            tracing::info!(target: "atw", "idle monitor stopping");
            return Ok(());
        }
    }
}
