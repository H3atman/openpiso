use std::sync::{Arc, RwLock};

use tokio::sync::broadcast;

use crate::{
    models::{CoinTransaction, KioskConfig, KioskStatus, Pc, ShopStatus, ThemeSettings},
    shutdown,
    terminal_selection::SelectionState,
};

#[cfg(not(test))]
use tauri::{AppHandle, Emitter};

#[cfg(not(test))]
pub const PC_UPDATED_EVENT: &str = "pc-updated";

/// Tauri event name carrying every published `SelectionState` (ADR-0023).
/// The frontend uses `get_selection` on mount for the initial state and
/// listens here for live updates. Mirrors `crate::terminal_selection::SELECTION_STATE_EVENT`
/// — they MUST stay in sync; the duplication keeps the event constants
/// co-located with their producers.
pub const SELECTION_STATE_EVENT: &str = "selection-state";

/// Tauri event name carrying every published `KioskStatus` change. The
/// frontend (M3 kiosk) uses `get_kiosk_status` on mount for the initial
/// state and listens here for live updates. The simulator is always
/// `available`; M4 will publish `out_of_order` whenever the serial
/// source is dead.
pub const KIOSK_STATUS_EVENT: &str = "kiosk-status";

/// Full committed shop theme pushed directly to the Central Terminal.
pub const THEME_UPDATED_EVENT: &str = "theme-updated";

/// Committed Shop Name pushed directly to the Central Terminal.
pub const SHOP_NAME_UPDATED_EVENT: &str = "shop-name-updated";

/// Committed Shop Rate pushed directly to the Central Terminal.
pub const SHOP_RATE_UPDATED_EVENT: &str = "shop-rate-updated";

#[derive(Clone)]
pub struct EventHub {
    sender: broadcast::Sender<Pc>,
    coin_sender: broadcast::Sender<CoinTransaction>,
    shop_status: Arc<RwLock<Option<ShopStatus>>>,
    #[cfg(not(test))]
    app: Option<AppHandle>,
}

impl EventHub {
    #[cfg(not(test))]
    pub fn new(app: AppHandle) -> Self {
        let (sender, _) = broadcast::channel(128);
        let (coin_sender, _) = broadcast::channel(128);
        Self {
            sender,
            coin_sender,
            shop_status: Arc::new(RwLock::new(None)),
            app: Some(app),
        }
    }

    #[cfg(test)]
    pub fn new(_app: tauri::AppHandle) -> Self {
        Self::without_ui()
    }

    #[cfg(any(test, feature = "test-harness"))]
    pub fn without_ui() -> Self {
        let (sender, _) = broadcast::channel(128);
        let (coin_sender, _) = broadcast::channel(128);
        Self {
            sender,
            coin_sender,
            shop_status: Arc::new(RwLock::new(None)),
            #[cfg(not(test))]
            app: None,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Pc> {
        self.sender.subscribe()
    }

    pub fn subscribe_coins(&self) -> broadcast::Receiver<CoinTransaction> {
        self.coin_sender.subscribe()
    }

    pub fn publish_coin_accepted(&self, transaction: CoinTransaction) {
        let _ = self.coin_sender.send(transaction);
    }

    /// Replace the cached Shop Status used by the existing `pc.updated`
    /// broadcast. Status changes therefore ride the same delivery path as PC
    /// state instead of introducing a second event channel.
    pub fn set_shop_status(&self, status: ShopStatus) {
        if let Ok(mut cached) = self.shop_status.write() {
            *cached = Some(status);
        }
    }

    pub fn shop_status(&self) -> Option<ShopStatus> {
        self.shop_status
            .read()
            .ok()
            .and_then(|cached| cached.clone())
    }

    pub fn stamp_shop_status(&self, mut pc: Pc) -> Pc {
        pc.shop_status = self.shop_status();
        pc
    }

    /// Broadcast a committed PC state and return it with the authoritative
    /// power command stamped on (ADR-0022, ADR-0034). The power state is
    /// computed here, the single broadcast chokepoint, so every delivery
    /// channel that reuses the returned `Pc` (notably the heartbeat HTTP
    /// response and the SSE updates) carries a consistent command.
    pub fn publish_pc_updated(&self, mut pc: Pc) -> Pc {
        pc = self.stamp_shop_status(pc);
        pc.power_state = shutdown::power_state(&pc.status, pc.remaining_seconds);
        let _ = self.sender.send(pc.clone());

        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(PC_UPDATED_EVENT, pc.clone()) {
                tracing::error!(target: "openpiso_admin_lib::events", pc_id = %pc.id, "Could not emit {PC_UPDATED_EVENT} to the admin UI: {error}");
            }
        }

        pc
    }

    /// Broadcast the current selection state. `Some` means a window is open;
    /// `None` means the server has closed it. The selection is a Tauri-local
    /// concern (the kiosk is the only consumer; the LAN HTTP API on `:3210`
    /// does not expose it), so the event is emitted only to the Tauri webview.
    pub fn publish_selection_state(&self, state: Option<SelectionState>) {
        // Always reference the constant so the test build doesn't drop it
        // as dead code; the cfg-gated emit below is the only caller.
        let event_name = SELECTION_STATE_EVENT;
        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(event_name, &state) {
                let pc_id = state
                    .as_ref()
                    .map(|state| state.pc_id.as_str())
                    .unwrap_or("<closed>");
                tracing::error!(target: "openpiso_admin_lib::events", pc_id = %pc_id, "Could not emit {event_name} to the admin UI: {error}");
            }
        }
        // In test builds the event is a no-op; tests drive the selection
        // through the public API and assert on the returned snapshots.
        let _ = (event_name, state);
    }

    /// Broadcast a `KioskStatus` change. The kiosk is the only consumer;
    /// the LAN HTTP API on `:3210` does not expose it, so the event is
    /// emitted only to the Tauri webview. The simulator is always
    /// `available`; M4 will publish `out_of_order` whenever the serial
    /// source goes down.
    pub fn publish_kiosk_status(&self, status: KioskStatus) {
        let event_name = KIOSK_STATUS_EVENT;
        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(event_name, &status) {
                tracing::error!(target: "openpiso_admin_lib::events", "Could not emit {event_name} to the admin UI: {error}");
            }
        }
        let _ = (event_name, status);
    }

    /// Push a committed theme to the kiosk immediately after the owner PUT.
    pub fn publish_theme_updated(&self, theme: ThemeSettings) {
        let event_name = THEME_UPDATED_EVENT;
        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(event_name, &theme) {
                tracing::error!(target: "openpiso_admin_lib::events", "Could not emit {event_name} to the admin UI: {error}");
            }
        }
        let _ = (event_name, theme);
    }

    /// Push a committed Shop Name to the kiosk immediately after owner save.
    pub fn publish_shop_name_updated(&self, shop_name: String) {
        let event_name = SHOP_NAME_UPDATED_EVENT;
        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(event_name, &shop_name) {
                tracing::error!(target: "openpiso_admin_lib::events", "Could not emit {event_name} to the admin UI: {error}");
            }
        }
        let _ = (event_name, shop_name);
    }

    /// Push a committed Shop Rate to the kiosk immediately after owner save.
    pub fn publish_shop_rate_updated(&self, rate: KioskConfig) {
        let event_name = SHOP_RATE_UPDATED_EVENT;
        #[cfg(not(test))]
        if let Some(app) = &self.app {
            if let Err(error) = app.emit(event_name, &rate) {
                tracing::error!(target: "openpiso_admin_lib::events", "Could not emit {event_name} to the admin UI: {error}");
            }
        }
        let _ = (event_name, rate);
    }
}

#[cfg(test)]
mod tests {
    use openpiso_core::session::PowerState;

    use super::*;

    fn pc(id: &str, status: &str, remaining: i64) -> Pc {
        let mut pc = Pc::from((
            id.to_owned(),
            id.to_uppercase(),
            status.to_owned(),
            remaining,
            None,
            None,
        ));
        pc.remaining_seconds = remaining;
        pc
    }

    #[test]
    fn broadcast_stamps_the_authoritative_power_command() {
        let hub = EventHub::without_ui();
        let mut rx = hub.subscribe();

        // pc-01 winds down to a low-time warning, then reaches zero and is
        // commanded to shut down.
        let stamped = hub.publish_pc_updated(pc("pc-01", "in_use", 1));
        assert_eq!(stamped.power_state, PowerState::Warning);
        assert_eq!(rx.try_recv().unwrap().power_state, PowerState::Warning);
        let stamped = hub.publish_pc_updated(pc("pc-01", "online", 0));
        // The returned value matches the broadcast so the heartbeat HTTP
        // response and the SSE stream cannot disagree (ADR-0034).
        assert_eq!(stamped.power_state, PowerState::Shutdown);
        let expired = rx.try_recv().unwrap();
        assert_eq!(expired.id, "pc-01");
        assert_eq!(expired.power_state, PowerState::Shutdown);

        // A freshly booted PC reporting zero is now also commanded off
        // (ADR-0034 supersedes the positive→zero exception).
        let other = hub.publish_pc_updated(pc("pc-02", "online", 0));
        assert_eq!(other.power_state, PowerState::Shutdown);
        assert_eq!(rx.try_recv().unwrap().power_state, PowerState::Shutdown);
    }

    #[test]
    fn pc_broadcast_carries_the_cached_shop_status() {
        let hub = EventHub::without_ui();
        let status = ShopStatus {
            open: false,
            next_change_at: Some("2026-08-03T08:00:00+08:00".to_owned()),
            reason: openpiso_core::shop_hours::ShopStatusReason::Schedule,
            last_call_active: false,
        };
        hub.set_shop_status(status.clone());
        let mut receiver = hub.subscribe();

        let published = hub.publish_pc_updated(pc("pc-01", "online", 0));
        assert_eq!(published.shop_status, Some(status.clone()));
        assert_eq!(receiver.try_recv().unwrap().shop_status, Some(status));
    }
}
