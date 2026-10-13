use std::sync::Arc;

use sqlx::SqlitePool;

use crate::coin_source::ConfiguredCoinSource;
use crate::events::EventHub;
use crate::kiosk_lock::KioskLock;
use crate::sessions::SessionStore;
use crate::terminal_selection::TerminalSelection;

/// Shared Tauri state for the Central Terminal kiosk (M3, ADR-0023).
///
/// M6 retired the desktop owner console (ADR-0024); the Tauri webview is
/// now exclusively the customer-facing kiosk. The owner surface lives at
/// `http://<host>:8080/admin` and owns its own `OwnerServerState` over
/// there, so `owner_tokens` no longer lives here.
pub struct AppState {
    pub pool: SqlitePool,
    pub sessions: SessionStore,
    pub events: EventHub,
    /// Server-owned Central Terminal selection (ADR-0023). M2 wires the
    /// configured simulator or serial source. The `Arc` lets the background poll task
    /// share the singleton with the Tauri commands.
    pub terminal_selection: Arc<TerminalSelection<ConfiguredCoinSource>>,
    /// Ephemeral always-on-top state for the kiosk window (ADR-0056). Never
    /// persisted: every launch starts locked.
    pub kiosk_lock: KioskLock,
}
