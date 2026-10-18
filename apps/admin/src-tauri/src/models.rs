use openpiso_core::{
    backup_schedule::BackupFrequency,
    forwarding::ForwardedDiagnostic,
    notification::NotificationSeverity,
    report::ReportPeriod,
    session::{PowerState, TimerMode},
    shop_hours::{ShopOverrideMode, ShopStatusReason},
};
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    pub status: String,
    pub created_at: String,
    pub deleted_at: Option<String>,
    /// Owner-only provenance; omitted from customer-facing Account responses.
    #[serde(skip)]
    pub imported_from: Option<String>,
    #[serde(skip)]
    pub imported_at: Option<String>,
    /// Owner-only derived state; no login-history column is stored.
    #[serde(skip)]
    pub legacy_not_logged_in: bool,
    /// Owner-only derived state from successful login audit events.
    #[serde(skip)]
    pub last_login_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OwnerAccount {
    pub id: String,
    pub username: String,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    pub status: String,
    pub created_at: String,
    pub deleted_at: Option<String>,
    pub imported_from: Option<String>,
    pub imported_at: Option<String>,
    pub legacy_not_logged_in: bool,
    pub last_login_at: Option<String>,
}

impl From<Account> for OwnerAccount {
    fn from(account: Account) -> Self {
        Self {
            id: account.id,
            username: account.username,
            remaining_seconds: account.remaining_seconds,
            points_balance_centi: account.points_balance_centi,
            status: account.status,
            created_at: account.created_at,
            deleted_at: account.deleted_at,
            imported_from: account.imported_from,
            imported_at: account.imported_at,
            legacy_not_logged_in: account.legacy_not_logged_in,
            last_login_at: account.last_login_at,
        }
    }
}

impl
    From<(
        String,
        String,
        i64,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
    )> for Account
{
    fn from(
        value: (
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ),
    ) -> Self {
        Self {
            id: value.0,
            username: value.1,
            remaining_seconds: value.2,
            points_balance_centi: value.3,
            status: value.4,
            created_at: value.5,
            deleted_at: value.6,
            imported_from: value.7,
            imported_at: value.8,
            last_login_at: value.9,
            legacy_not_logged_in: value.10,
        }
    }
}

impl
    From<(
        String,
        String,
        i64,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
    )> for Account
{
    fn from(
        value: (
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ),
    ) -> Self {
        Self {
            id: value.0,
            username: value.1,
            remaining_seconds: value.2,
            points_balance_centi: value.3,
            status: value.4,
            created_at: value.5,
            deleted_at: value.6,
            imported_from: value.7,
            imported_at: value.8,
            legacy_not_logged_in: value.9,
            last_login_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AccountCredentials {
    pub username: String,
    pub password: String,
    pub pc_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountAuthResponse {
    pub token: String,
    pub account: Account,
    pub session: ActiveSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    pub id: String,
    pub account_id: String,
    pub username: String,
    pub pc_id: String,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    pub claimable_guest_seconds: i64,
    /// ISO-8601 timestamp used internally for tamper-kill attribution.
    #[serde(skip_serializing)]
    pub started_at: String,
}

#[derive(Debug, Serialize)]
pub struct OwnerAuthResponse {
    /// Session-bound CSRF token for the browser to keep in memory. The owner
    /// token itself travels only in the HttpOnly cookie (ADR-0074).
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminAccountInput {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminAccountMutationInput {
    pub account_id: String,
    pub password: Option<String>,
}

/// Body of `POST /web-admin/accounts/delete`: the explicit Account IDs the
/// owner selected for one all-or-nothing deletion batch.
#[derive(Debug, Deserialize)]
pub struct AdminAccountDeleteInput {
    #[serde(default)]
    pub account_ids: Vec<String>,
    /// Backward-compatible singleton form used by older owner clients.
    #[serde(default)]
    pub account_id: Option<String>,
}

/// Body of `POST /web-admin/notifications/acknowledge`: the explicit Alert
/// Audit Log IDs the owner had rendered when they acknowledged in bulk.
#[derive(Debug, Deserialize)]
pub struct NotificationAcknowledgeInput {
    #[serde(default)]
    pub audit_ids: Vec<String>,
}

/// Result of one all-or-nothing owner Account deletion request.
#[derive(Debug, Serialize)]
pub struct AccountDeletionResult {
    pub deleted_count: usize,
}

/// Body of `POST /web-admin/accounts/adjust-{time,points}` — the owner
/// grants or deducts an account balance directly. `delta` is signed
/// (negative deducts); the server clamps the resulting balance at zero and
/// never trusts a frontend-supplied absolute value.
#[derive(Debug, Deserialize)]
pub struct AdminAccountAdjustInput {
    pub account_id: String,
    pub delta: i64,
}

/// Body of `POST /web-admin/accounts/adjust-points` — like
/// [`AdminAccountAdjustInput`] but the signed delta is integer centipoints
/// (ADR-0038), named `_centi` so no consumer can silently send whole points.
#[derive(Debug, Deserialize)]
pub struct AdminAccountAdjustPointsInput {
    pub account_id: String,
    pub delta_centi: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pc {
    pub id: String,
    pub name: String,
    pub status: String,
    pub remaining_seconds: i64,
    pub guest_remaining_seconds: i64,
    pub timer_mode: TimerMode,
    pub active_session: Option<ActiveSession>,
    /// The server-resolved Shop Status carried by the existing `pc.updated`
    /// broadcast consumed by the Central Terminal and Clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shop_status: Option<ShopStatus>,
    /// Server-commanded expiry action for the connected client (ADR-0022).
    /// Computed in the broadcast path; defaults to `active` for read queries.
    pub power_state: PowerState,
    /// Owner-editable grace countdown before the Client issues OS shutdown.
    pub shutdown_grace_seconds: i64,
    pub last_seen_at: Option<String>,
    /// Signed Admin receipt time minus the Client's reported Unix time. A
    /// positive value means the Client clock is behind the Admin clock.
    pub clock_difference_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PcSettings {
    pub id: String,
    pub name: String,
    pub visible: bool,
    /// Owner-authored static IP used to bind a PC on first boot
    /// (ADR-0031). `None` until the owner pre-registers the PC.
    pub registered_ip: Option<String>,
    /// Server-bound MAC after first-boot identity resolution
    /// (ADR-0031). `None` until the row has been claimed.
    pub mac_address: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PcSettingsInput {
    pub name: String,
    pub visible: bool,
    /// Optional static IP. Pass an empty string to clear. Pre-registration
    /// field for the first-boot IP bind (ADR-0031).
    #[serde(default)]
    pub registered_ip: Option<String>,
    /// Optional MAC. Pass an empty string to clear. The owner clears this
    /// to allow a hardware/NIC replacement to re-bind (ADR-0031).
    #[serde(default)]
    pub mac_address: Option<String>,
}

/// Body of `POST /web-admin/pcs` — pre-register a PC by number/name + a
/// static IP (ADR-0031 / plan §4 M5). The `pc_id` is the owner-facing
/// identifier (e.g. `pc-05`); the row lands with `mac_address` NULL so
/// the next boot from that IP can bind it.
#[allow(dead_code)] // M5: wired to the web-admin "register PC" surface.
#[derive(Debug, Deserialize)]
pub struct PcRegisterInput {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub registered_ip: Option<String>,
}

impl From<(String, String, String, i64, Option<String>, Option<i64>)> for Pc {
    fn from(value: (String, String, String, i64, Option<String>, Option<i64>)) -> Self {
        Self {
            id: value.0,
            name: value.1,
            status: value.2,
            remaining_seconds: value.3,
            guest_remaining_seconds: value.3,
            timer_mode: TimerMode::Guest,
            active_session: None,
            shop_status: None,
            power_state: PowerState::Active,
            shutdown_grace_seconds: crate::db::DEFAULT_SHUTDOWN_GRACE_SECONDS,
            last_seen_at: value.4,
            clock_difference_ms: value.5,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CoinTransaction {
    pub id: String,
    pub pc_id: String,
    pub pc_name: String,
    pub account_id: Option<String>,
    pub account_username: Option<String>,
    pub amount_cents: i64,
    pub seconds_added: i64,
    pub coin_promo_bonus_seconds: i64,
    pub points_earned_centi: i64,
    pub source: String,
    pub target_kind: String,
    pub created_at: String,
}

impl
    From<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        String,
        String,
        String,
    )> for CoinTransaction
{
    fn from(
        value: (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            String,
        ),
    ) -> Self {
        Self {
            id: value.0,
            pc_id: value.1,
            pc_name: value.2,
            account_id: value.3,
            account_username: value.4,
            amount_cents: value.5,
            seconds_added: value.6,
            coin_promo_bonus_seconds: value.7,
            points_earned_centi: value.8,
            source: value.9,
            target_kind: value.10,
            created_at: value.11,
        }
    }
}

/// Trust attestation a coin event carries (ADR-0033). Constructed once at
/// each request edge and flows unchanged through sessions into the DB commit.
pub enum CoinSourceClaim {
    /// Canonical id = LOCAL_SERIAL_DEVICE_ID ("central-terminal-serial").
    /// Constructed only by the local terminal selection when the source is
    /// the real serial acceptor.
    SerialTerminal,
    /// Canonical id = "dev-simulator". Constructed by the local terminal
    /// selection when the source is the simulator.
    Simulator,
    /// A coin event arriving over HTTP. The HTTP edge can construct only
    /// this variant — it can never claim the serial-terminal identity
    /// (type-level guarantee, with in-transaction defence-in-depth).
    RemoteDevice { device_id: String, secret: String },
}

impl std::fmt::Debug for CoinSourceClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SerialTerminal => f.debug_tuple("SerialTerminal").finish(),
            Self::Simulator => f.debug_tuple("Simulator").finish(),
            Self::RemoteDevice {
                device_id,
                secret: _,
            } => f
                .debug_struct("RemoteDevice")
                .field("device_id", device_id)
                .field("secret", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Payment facts only — device identity is carried by [`CoinSourceClaim`].
#[derive(Debug, Deserialize)]
pub struct CoinEventInput {
    pub pc_id: String,
    pub amount_cents: i64,
    pub pulse_count: Option<i64>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum CoinTarget<'a> {
    Guest,
    Account { account_id: &'a str },
}

#[derive(Debug, Clone, Serialize)]
pub struct EconomySettings {
    pub earn_cents_per_point: i64,
    pub rate_amount_cents: i64,
    pub rate_seconds: i64,
    pub effective_bonus_basis_points: i64,
    pub unsafe_bonus_threshold_basis_points: i64,
    pub unsafe_bonus: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EconomySettingsInput {
    pub earn_cents_per_point: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedemptionTier {
    pub id: i64,
    pub points: i64,
    pub seconds: i64,
}

impl RedemptionTier {
    /// The points→seconds exchange consumed by openpiso-core's safety math.
    pub fn rate(&self) -> openpiso_core::pricing::TierRate {
        openpiso_core::pricing::TierRate {
            points: self.points,
            seconds: self.seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedemptionTierMenu {
    pub version: i64,
    pub tiers: Vec<RedemptionTier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedemptionTierInput {
    pub points: i64,
    pub minutes: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedemptionTiersInput {
    pub tiers: Vec<RedemptionTierInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoinPromoTier {
    pub amount_cents: i64,
    pub bonus_seconds: i64,
    pub effective_bonus_basis_points: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoinPromoMenuTier {
    pub amount_cents: i64,
    pub bonus_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoinPromoMenu {
    pub enabled: bool,
    pub tiers: Vec<CoinPromoMenuTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoinPromoSettings {
    pub enabled: bool,
    pub tiers: Vec<CoinPromoTier>,
    pub account_worst_case_basis_points: i64,
    pub unsafe_bonus_threshold_basis_points: i64,
    pub unsafe_bonus: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShopHoursDayMode {
    OpenPeriod,
    ClosedAllDay,
    Open24Hours,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShopHoursDay {
    pub weekday: i64,
    pub mode: ShopHoursDayMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_minute: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_minute: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShopHoursOverride {
    pub mode: ShopOverrideMode,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShopHoursSettings {
    pub enabled: bool,
    pub last_call_lead_minutes: i64,
    pub days: Vec<ShopHoursDay>,
    #[serde(rename = "override")]
    pub shop_override: Option<ShopHoursOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShopStatus {
    pub open: bool,
    pub next_change_at: Option<String>,
    pub reason: ShopStatusReason,
    pub last_call_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShopHoursSettingsResponse {
    #[serde(flatten)]
    pub settings: ShopHoursSettings,
    pub status: ShopStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopHoursDayInput {
    pub weekday: i64,
    pub mode: ShopHoursDayMode,
    #[serde(default)]
    pub open_minute: Option<i64>,
    #[serde(default)]
    pub close_minute: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopHoursSettingsInput {
    pub enabled: bool,
    pub last_call_lead_minutes: i64,
    pub days: Vec<ShopHoursDayInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopOverrideInput {
    pub mode: ShopOverrideMode,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupScheduleSettings {
    pub enabled: bool,
    pub frequency: BackupFrequency,
    pub minute_of_day: i64,
    pub weekday: i64,
    pub day_of_month: i64,
    pub backup_sets: Vec<String>,
    pub retention_count: i64,
    pub mirror_directory: Option<String>,
    pub mirror_error: Option<String>,
    pub next_due_at: Option<String>,
    #[cfg(debug_assertions)]
    pub interval_seconds: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupScheduleSettingsInput {
    pub enabled: bool,
    pub frequency: BackupFrequency,
    pub minute_of_day: i64,
    pub weekday: i64,
    pub day_of_month: i64,
    pub backup_sets: Vec<String>,
    pub retention_count: i64,
    #[serde(default)]
    pub mirror_directory: Option<String>,
    #[cfg(debug_assertions)]
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: i64,
}

#[cfg(debug_assertions)]
fn default_interval_seconds() -> i64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoinPromoTierInput {
    pub amount_cents: i64,
    pub bonus_minutes: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoinPromoSettingsInput {
    pub enabled: bool,
    pub tiers: Vec<CoinPromoTierInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Voucher {
    pub id: String,
    pub code: String,
    pub kind: String,
    pub status: String,
    pub seconds_value: i64,
    pub points_value_centi: i64,
    pub sold_amount_cents: i64,
    pub created_at: String,
    pub sold_at: Option<String>,
    pub redeemed_by_account_id: Option<String>,
    pub redeemed_by_username: Option<String>,
    pub redeemed_at: Option<String>,
    pub expires_at: Option<String>,
    pub batch_id: Option<String>,
    pub batch_name: Option<String>,
    pub batch_prefix: Option<String>,
}

impl
    From<(
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> for Voucher
{
    fn from(
        value: (
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    ) -> Self {
        Self {
            id: value.0,
            code: value.1,
            kind: value.2,
            status: value.3,
            seconds_value: value.4,
            points_value_centi: value.5,
            sold_amount_cents: value.6,
            created_at: value.7,
            sold_at: value.8,
            redeemed_by_account_id: value.9,
            redeemed_by_username: value.10,
            redeemed_at: value.11,
            expires_at: value.12,
            batch_id: value.13,
            batch_name: value.14,
            batch_prefix: value.15,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateVoucherInput {
    pub prefix: Option<String>,
    pub suffix_length: Option<u8>,
    pub seconds_value: i64,
    pub points_value_centi: i64,
    pub sold_amount_cents: Option<i64>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VoucherRedeemInput {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoucherBatch {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub suffix_length: i64,
    pub count: i64,
    pub kind: String,
    pub seconds_value: i64,
    pub points_value_centi: i64,
    pub cash_amount_cents: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub voided_at: Option<String>,
    pub voided_audit_event_id: Option<String>,
}

impl
    From<(
        String,
        String,
        String,
        i64,
        i64,
        String,
        i64,
        i64,
        i64,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    )> for VoucherBatch
{
    fn from(
        value: (
            String,
            String,
            String,
            i64,
            i64,
            String,
            i64,
            i64,
            i64,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        ),
    ) -> Self {
        Self {
            id: value.0,
            name: value.1,
            prefix: value.2,
            suffix_length: value.3,
            count: value.4,
            kind: value.5,
            seconds_value: value.6,
            points_value_centi: value.7,
            cash_amount_cents: value.8,
            expires_at: value.9,
            created_at: value.10,
            voided_at: value.11,
            voided_audit_event_id: value.12,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VoucherBatchSummary {
    #[serde(flatten)]
    pub batch: VoucherBatch,
    pub redeemed_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoucherBatchDetail {
    #[serde(flatten)]
    pub batch: VoucherBatch,
    pub redeemed_count: i64,
    pub vouchers: Vec<Voucher>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVoucherBatchInput {
    pub prefix: String,
    pub suffix_length: Option<u8>,
    pub count: i64,
    pub seconds_value: i64,
    pub points_value_centi: i64,
    pub cash_amount_cents: Option<i64>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PointsRedeemInput {
    pub tier_id: i64,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct AccountBalanceMutation {
    pub account: Account,
}

/// Segments carry no probability weight (ADR-0047) — position in the list is
/// the only thing distinguishing one slice from another. Serde's default of
/// ignoring unknown fields lets an older client still sending `weight` degrade
/// rather than error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WheelSegmentRow {
    pub label: String,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OwnerWheelSegmentRow {
    pub label: String,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
    pub probability_basis_points: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WheelSettings {
    pub enabled: bool,
    pub bokya_rate_basis_points: Option<i64>,
    pub free_spin_cap: i64,
    pub spin_duration: i64,
    /// Spin Password Confirmation: spins require the Account's password.
    pub password_confirmation_required: bool,
    pub version: i64,
    pub segments: Vec<OwnerWheelSegmentRow>,
    pub expected_payout_seconds: i64,
    pub expected_payout_points_centi: i64,
    pub point_value_seconds: i64,
    pub house_edge_basis_points: i64,
    pub unsafe_house_edge: bool,
    /// Owner-only advisory against the paid-time value of one earned point
    /// under the active Shop Rate. The Wheel save guardrail above remains
    /// tied to the most generous Redemption Tier.
    pub shop_rate_point_value_seconds: i64,
    pub shop_rate_house_edge_basis_points: i64,
    pub unsafe_shop_rate_house_edge: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientWheelSettings {
    pub enabled: bool,
    pub free_spin_cap: i64,
    pub spin_duration: i64,
    pub password_confirmation_required: bool,
    pub version: i64,
    pub segments: Vec<WheelSegmentRow>,
    pub expected_payout_seconds: i64,
    pub expected_payout_points_centi: i64,
    pub point_value_seconds: i64,
    pub house_edge_basis_points: i64,
    pub unsafe_house_edge: bool,
}

#[derive(Debug, Deserialize)]
pub struct WheelSettingsInput {
    pub enabled: bool,
    pub bokya_rate_basis_points: Option<i64>,
    pub free_spin_cap: i64,
    pub spin_duration: i64,
    /// Defaults off so an older web-admin save body stays valid.
    #[serde(default)]
    pub password_confirmation_required: bool,
    pub segments: Vec<WheelSegmentRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WheelSpinStep {
    pub segment_index: usize,
    pub label: String,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WheelSpinResult {
    pub seconds_won: i64,
    pub points_won_centi: i64,
    pub free_spins_triggered: i64,
    pub segments: Vec<WheelSegmentRow>,
    pub steps: Vec<WheelSpinStep>,
}

#[derive(Debug, Clone)]
pub struct WheelSpinMutation {
    pub account: Account,
    pub result: WheelSpinResult,
}

#[derive(Debug, Serialize)]
pub struct WheelSpinResponse {
    pub token: String,
    pub account: Account,
    pub session: ActiveSession,
    pub result: WheelSpinResult,
}

#[derive(Debug, Clone, Serialize)]
pub struct TodayReport {
    pub sales_cents: i64,
    pub transaction_count: i64,
    pub seconds_sold: i64,
}

impl From<(i64, i64, i64)> for TodayReport {
    fn from(value: (i64, i64, i64)) -> Self {
        Self {
            sales_cents: value.0,
            transaction_count: value.1,
            seconds_sold: value.2,
        }
    }
}

/// PC Monitoring state on the dashboard (ADR-0076): the shop-wide toggle and
/// the durable open incidents whose badges stay visible independently of
/// Notification acknowledgement.
#[derive(Debug, Serialize)]
pub struct DashboardPcMonitoring {
    pub enabled: bool,
    /// Open incident kinds per PC id. Empty map: every PC is clear.
    pub active_incidents: BTreeMap<String, Vec<crate::db::monitoring::IncidentKind>>,
    /// Active Monitoring Suppression per PC id, holding the absolute RFC 3339
    /// UTC expiry instant (#342). PCs without an entry are monitored normally.
    pub suppressions: BTreeMap<String, String>,
    /// Whether Client Recovery Escalation is armed (ADR-0077). The dashboard
    /// carries a standing advisory while it is, because the owner should never
    /// be unaware that OpenPiso may power a customer's PC off.
    pub escalation_enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct DashboardSnapshot {
    pub shop_name: String,
    pub pcs: Vec<Pc>,
    pub recent_transactions: Vec<CoinTransaction>,
    pub today: TodayReport,
    pub pc_monitoring: DashboardPcMonitoring,
}

/// Owner-facing PC Monitoring toggle state (ADR-0076).
#[derive(Debug, Serialize)]
pub struct PcMonitoringStatus {
    pub enabled: bool,
    /// False on non-Windows Admin, which can never run the monitor.
    pub platform_supported: bool,
}

/// Body of `PUT /web-admin/pc-monitoring/enabled`. The current Owner PIN is
/// re-proved server-side so a stolen browser session cannot silently weaken
/// monitoring (ADR-0076).
#[derive(Debug, Deserialize)]
pub struct PcMonitoringEnabledInput {
    pub enabled: bool,
    pub current_pin: String,
}

/// Body of `PUT /web-admin/pc-monitoring/escalation` (ADR-0077). Powering a
/// customer's PC off is the most destructive thing PC Monitoring can do, so
/// the current Owner PIN is re-proved exactly like the monitoring toggle.
#[derive(Debug, Deserialize)]
pub struct ClientRecoveryEscalationInput {
    pub enabled: bool,
    pub delay_secs: i64,
    pub current_pin: String,
}

/// Body of `POST /web-admin/pc-monitoring/suppressions/{pc_id}` (#342). The
/// Owner Session alone cannot suppress; the current PIN is re-proved exactly
/// like the enable/disable step-up.
#[derive(Debug, Deserialize)]
pub struct PcMonitoringSuppressionInput {
    pub duration_minutes: i64,
    pub current_pin: String,
}

/// Body of `POST /web-admin/pc-monitoring/suppressions/{pc_id}/cancel`.
#[derive(Debug, Deserialize)]
pub struct PcMonitoringSuppressionCancelInput {
    pub current_pin: String,
}

/// One active per-PC Monitoring Suppression as returned by a mutation (#342).
/// `expires_at` is Admin's absolute expiry instant (RFC 3339 UTC) — display
/// never derives it from the request.
#[derive(Debug, Serialize)]
pub struct PcMonitoringSuppression {
    pub pc_id: String,
    pub pc_name: String,
    pub expires_at: String,
}

/// Outcome of cancelling a suppression: `false` means nothing was active
/// (already expired), which stays a success for repeated clicks.
#[derive(Debug, Serialize)]
pub struct PcMonitoringSuppressionCancelled {
    pub cancelled: bool,
}

/// The Guard coverage outcome a Client reports with each Heartbeat (ADR-0076).
/// A release Windows Client reports its real held pipe connection; debug and
/// non-Windows Clients report `not_applicable` because they can never hold
/// one. Omission (older Client) means unsupported or unknown. Strictly
/// diagnostic: it grants no identity, session, money, time, Points, or
/// routing authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardCoverageReport {
    /// The Client currently holds its Client Guard pipe connection.
    Covered,
    /// The release Windows Client cannot reach the Guard pipe.
    Unavailable,
    /// Debug and non-Windows builds can never hold a Guard connection.
    NotApplicable,
}

impl GuardCoverageReport {
    /// The single wire spelling of each outcome; storage and the monitoring
    /// diagnostic both read through here so they can never drift apart.
    pub fn as_str(self) -> &'static str {
        match self {
            GuardCoverageReport::Covered => "covered",
            GuardCoverageReport::Unavailable => "unavailable",
            GuardCoverageReport::NotApplicable => "not_applicable",
        }
    }

    /// Parse a stored or reported wire value. `None` means the value is not
    /// one of the typed outcomes (never reported, or corrupted) and reads as
    /// unsupported or unknown — never as a fabricated verdict.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "covered" => Some(Self::Covered),
            "unavailable" => Some(Self::Unavailable),
            "not_applicable" => Some(Self::NotApplicable),
            _ => None,
        }
    }
}

/// One Guard Poll body (ADR-0077). Every field is a fact about the Guard's own
/// process and the pipe it owns; none of them is authority. The Admin decides
/// what to do with them, and identifies the PC from the MAC header rather than
/// anything in here.
#[derive(Debug, Deserialize)]
pub struct GuardStateInput {
    /// Whether a verified Client currently holds the Guard pipe.
    pub client_connected: bool,
    /// Whether the Guard stood down for this boot after a PIN-gated owner quit.
    #[serde(default)]
    pub disarmed: bool,
    /// Relaunch attempts spent on the current absence. Zero means the Guard has
    /// not yet tried; the Admin never escalates against a Guard that has not.
    #[serde(default)]
    pub relaunch_attempts: i64,
    /// Forwarded Diagnostics riding this poll (ADR-0078). The Guard gets no
    /// second network call, so its batch travels inside the request it already
    /// makes. The source is always `guard` — the poll is the Guard's — and the
    /// PC identity comes from the MAC header, never from here.
    #[serde(default)]
    pub forwarded: Option<GuardForwardedLines>,
}

/// The wire form of the batch one Guard Poll carries. The body carries only
/// lines: the source is fixed by the endpoint and the PC comes from the MAC
/// header, so a poll line can never claim a different surface.
#[derive(Debug, Deserialize)]
pub struct GuardForwardedLines {
    pub lines: Vec<ForwardedDiagnostic>,
}

/// The desired state one Guard reads back. `power_off` is the whole command
/// surface, and the nonce makes acting on it idempotent.
#[derive(Debug, Serialize)]
pub struct GuardStateResponse {
    pub power_off: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// Seconds the Guard should wait before polling again.
    pub poll_interval_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatInput {
    pub hostname: String,
    pub client_version: String,
    /// Current Client wall-clock reading in Unix milliseconds. Older Clients
    /// may omit this measurement without breaking the Heartbeat.
    #[serde(default)]
    pub client_time_unix_ms: Option<i64>,
    /// Optional Tamper Kill report. The timestamp is the stable report name;
    /// Kill Report Age is the Client-measured elapsed duration used by the
    /// server to derive the kill instant. When present, the server writes a
    /// tamper_kill audit row and ends only the still-live Account or Guest
    /// session that matches the historical attribution.
    #[serde(default)]
    pub tamper_kill_timestamp: Option<String>,
    #[serde(default)]
    pub tamper_kill_attempt_count: Option<i64>,
    /// Client-clock elapsed milliseconds from the kill report's timestamp to
    /// the moment this heartbeat was sent. The server anchors the kill
    /// instant from receipt time and this age; the timestamp remains only the
    /// report's stable name. A non-integer value is retained as an invalid
    /// report so the Tamper Kill can still be audited as unknown.
    #[serde(default)]
    pub tamper_kill_report_age_ms: Option<serde_json::Value>,
    /// Optional Client Guard Coverage diagnostic (ADR-0076). Older Clients
    /// omit the field without failing the Heartbeat; a present but unknown
    /// value is rejected at this boundary. Strictly diagnostic — it grants no
    /// identity, session, money, time, Points, or routing authority.
    #[serde(default)]
    pub guard_coverage: Option<GuardCoverageReport>,
}

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportBucket {
    pub period_label: String,
    pub cash_revenue_cents: i64,
    pub free_time_dispensed_seconds: i64,
    pub points_minted_by_wheel_centi: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportSummary {
    pub period: ReportPeriod,
    pub from_date: String,
    pub to_date: String,
    pub cash_revenue_cents: i64,
    pub free_time_dispensed_seconds: i64,
    pub points_minted_by_wheel_centi: i64,
    pub points_liability_centi: i64,
    pub buckets: Vec<ReportBucket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PcReport {
    pub id: String,
    pub name: String,
    pub cash_revenue_cents: i64,
    pub seconds_sold: i64,
    pub free_time_dispensed_seconds: i64,
    pub from_date: String,
    pub to_date: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SalesReportPeriod {
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub cash_revenue_cents: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SalesReportPc {
    pub id: String,
    pub name: String,
    pub daily_sales_cents: i64,
    pub weekly_sales_cents: i64,
    pub monthly_sales_cents: i64,
    pub total_sales_cents: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SalesReportOverview {
    pub daily: SalesReportPeriod,
    pub weekly: SalesReportPeriod,
    pub monthly: SalesReportPeriod,
    pub total: SalesReportPeriod,
    pub pcs: Vec<SalesReportPc>,
}

/// One page of audit rows plus the total matching the same filters, so the
/// viewer can show "showing X–Y of Z" and a page count.
#[derive(Debug, Clone, Serialize)]
pub struct AuditPage {
    pub rows: Vec<AuditRow>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub id: String,
    pub event_type: String,
    pub outcome: String,
    pub actor: String,
    pub actor_account_id: Option<String>,
    pub actor_account_username: Option<String>,
    pub account_id: Option<String>,
    pub account_username: Option<String>,
    pub pc_id: Option<String>,
    pub pc_name: Option<String>,
    pub amount_cents: Option<i64>,
    pub seconds_delta: Option<i64>,
    pub attendance_seconds_delta: Option<i64>,
    pub points_delta_centi: Option<i64>,
    pub cash_flag: bool,
    pub category: String,
    pub summary: String,
    pub metadata_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationItem {
    pub id: String,
    pub severity: NotificationSeverity,
    pub event_type: String,
    pub outcome: String,
    pub actor: String,
    pub actor_account_id: Option<String>,
    pub actor_account_username: Option<String>,
    pub account_id: Option<String>,
    pub account_username: Option<String>,
    pub pc_id: Option<String>,
    pub pc_name: Option<String>,
    /// Present for PC Monitoring incident opening/closure Notifications.
    pub incident_kind: Option<String>,
    pub amount_cents: Option<i64>,
    pub seconds_delta: Option<i64>,
    pub points_delta_centi: Option<i64>,
    pub summary: String,
    pub created_at: String,
    pub read: bool,
    pub acknowledged_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationListResponse {
    pub items: Vec<NotificationItem>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuditFilterInput {
    pub limit: Option<i64>,
    /// Rows to skip before the current page. `None` and negatives mean "start
    /// at the newest row".
    #[serde(default)]
    pub offset: Option<i64>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub pc_id: Option<String>,
    pub account_id: Option<String>,
    pub actor: Option<Vec<String>>,
    pub outcome: Option<String>,
    pub category: Option<String>,
    pub event_type: Option<Vec<String>>,
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportSummaryInput {
    pub period: ReportPeriod,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AccountReportInput {
    pub account_id: String,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PcReportInput {
    pub pc_id: String,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ManualAdjustmentInput {
    pub pc_id: String,
    pub seconds_delta: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManualAdjustment {
    pub id: String,
    pub pc_id: String,
    pub pc_name: String,
    pub seconds_delta: i64,
    pub requested_seconds_delta: i64,
    pub reason: Option<String>,
    pub audit_event_id: String,
    pub created_at: String,
}

/// Status of a Points Transfer Request (ADR-0030). The customer creates a
/// `pending` request with a single-use 6-digit code; the owner confirms or
/// rejects, or the request expires.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PointsTransferStatus {
    Pending,
    Confirmed,
    Rejected,
    Expired,
}

impl PointsTransferStatus {
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "confirmed" => Some(Self::Confirmed),
            "rejected" => Some(Self::Rejected),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One row of the canonical `points_transfer_requests` table (ADR-0030).
/// The `code` is the single-use, time-boxed 6-digit code the customer reads
/// off the Client and reads to the owner; the owner enters that code in
/// web-admin to confirm.
#[derive(Debug, Clone, Serialize)]
pub struct PointsTransferRequest {
    pub id: String,
    pub from_account_id: String,
    pub to_account_id: String,
    pub points_centi: i64,
    pub code: String,
    pub status: PointsTransferStatus,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// What the Client receives when it asks the server to create a pending
/// Points Transfer Request. The `code` is the only thing the customer
/// needs to read aloud — the request id is for traceability in the
/// audit log.
#[derive(Debug, Clone, Serialize)]
pub struct PointsTransferRequestCreated {
    pub id: String,
    pub code: String,
}

/// One pending Points Transfer Request, enriched with both usernames for the
/// owner's approvals list (`GET /web-admin/transfer-requests`, plan §6). The
/// `code` is shown so the owner can match the number the customer reads aloud.
#[derive(Debug, Clone, Serialize)]
pub struct PendingPointsTransfer {
    pub id: String,
    pub from_account_id: String,
    pub from_username: String,
    pub to_account_id: String,
    pub to_username: String,
    pub points_centi: i64,
    pub code: String,
    pub created_at: String,
}

/// Body of `POST /api/accounts/transfer` (token-authenticated, immediate).
/// Time Transfer (ADR-0030) is the only currency.
#[derive(Debug, Deserialize)]
pub struct AccountTransferInput {
    pub to_username: String,
    pub seconds: i64,
}

/// Body of `POST /api/accounts/transfer-points` (token-authenticated,
/// owner-confirmed). Points Transfer (ADR-0030).
#[derive(Debug, Deserialize)]
pub struct AccountPointsTransferInput {
    pub to_username: String,
    pub points_centi: i64,
}

/// Body of `POST /api/accounts/change-password`. Customer Password Change;
/// the Account Session and Active Session survive (ADR-0037).
#[derive(Debug, Deserialize)]
pub struct ChangePasswordInput {
    pub old_password: String,
    pub new_password: String,
}

/// Body of `POST /api/owner/verify-pin`. The Client posts the typed PIN and
/// the server compares it against the hashed `owner_pin` setting (ADR-0027).
/// The plaintext PIN is never echoed back.
#[derive(Debug, Deserialize)]
pub struct OwnerPinVerifyInput {
    pub pin: String,
}

/// Response for `POST /api/owner/verify-pin`. `ok` is `false` for a wrong or
/// unconfigured PIN — the request itself still succeeds (HTTP 200) so the
/// Client can distinguish "wrong PIN" from a transport failure (ADR-0027).
///
/// `hash` carries the stored strong (Argon2) `owner_pin` hash and is
/// populated **only on a successful verify** (`ok == true`) so the Client can
/// cache it for the offline fallback (ADR-0027 §3). It is therefore never
/// handed to a caller who has not already proven the PIN, and stays `None` on
/// any failure or when no PIN is configured.
#[derive(Debug, Clone, Serialize)]
pub struct OwnerPinVerification {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Generic success acknowledgement for mutations whose only meaningful result
/// is "it happened" (e.g. change-password, set owner PIN). Never carries a
/// secret or a hash.
#[derive(Debug, Clone, Serialize)]
pub struct MutationAck {
    pub ok: bool,
}

/// Public read of the owner-editable `shop_name` setting. The web-admin
/// `GET /web-admin/settings` endpoint returns this shape; the canonical
/// settings table is the source of truth (ADR-0027).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSettings {
    pub shop_name: String,
    pub shutdown_grace_seconds: i64,
    pub offline_grace_seconds: i64,
}

/// Shop-wide owner-authored HTML + CSS fragment used only for owner preview
/// and printing. Defaults are returned so the editor can reset without
/// duplicating them in TypeScript.
#[derive(Debug, Clone, Serialize)]
pub struct VoucherTemplate {
    pub html: String,
    pub css: String,
    pub default_html: String,
    pub default_css: String,
}

#[derive(Debug, Deserialize)]
pub struct VoucherTemplateInput {
    pub html: String,
    pub css: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShopInfo {
    pub shop_name: String,
    pub shutdown_grace_seconds: i64,
    pub offline_grace_seconds: i64,
    pub theme_version: i64,
    pub wheel_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeSettings {
    pub accent_color: String,
    pub mode_web_admin: ThemeMode,
    pub mode_kiosk: ThemeMode,
    pub mode_client: ThemeMode,
    pub version: i64,
}

#[derive(Debug, Deserialize)]
pub struct ThemeSettingsInput {
    pub accent_color: String,
    pub mode_web_admin: ThemeMode,
    pub mode_kiosk: ThemeMode,
    pub mode_client: ThemeMode,
    pub version: i64,
}

/// Whether the server processed a tamper-kill report from this heartbeat.
/// Omitted entirely when no report was present (serde skip).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TamperKillAck {
    Recorded,
}

#[derive(Debug, Serialize)]
pub struct ClientPcResponse<'a> {
    #[serde(flatten)]
    pub pc: &'a Pc,
    pub attendance_supported: bool,
}

#[derive(Debug, Serialize)]
pub struct HeartbeatResponse {
    #[serde(flatten)]
    pub pc: Pc,
    pub attendance_supported: bool,
    pub theme_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tamper_kill_acknowledged: Option<TamperKillAck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tamper_kill_subject: Option<String>,
    /// The Diagnostic Request (ADR-0078): the single-use nonce of the owner's
    /// ask for one windowed, byte-capped log slice. Present only while an ask
    /// is armed and unfulfilled; the Client acts once per nonce. Strictly
    /// diagnostic — it grants no session, power, or routing authority, and is
    /// deliberately not a "Heartbeat order".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_request_nonce: Option<String>,
}

/// Body of `PUT /web-admin/settings`. The owner can change the shop name
/// here; the value is validated at the Rust boundary (non-empty, ≤80
/// characters) and persisted with a `shop_name_updated` audit event.
#[derive(Debug, Deserialize)]
pub struct AdminSettingsInput {
    pub shop_name: String,
    #[serde(default)]
    pub shutdown_grace_seconds: Option<i64>,
    #[serde(default)]
    pub offline_grace_seconds: Option<i64>,
}

/// Body of `PUT /web-admin/owner-pin`. Write-only: the new PIN is hashed
/// server-side and never read back (ADR-0027). Validated at the Rust boundary.
#[derive(Debug, Deserialize)]
pub struct OwnerPinInput {
    pub new_pin: String,
}

/// Owner-facing Diagnostics Mode state (ADR-0081): the shop-wide visibility
/// flag for the Diagnostics page. It changes visibility only — it is not a
/// security boundary and grants no authority.
#[derive(Debug, Serialize)]
pub struct DiagnosticsModeStatus {
    pub enabled: bool,
}

/// Body of `PUT /web-admin/diagnostics-mode` (ADR-0081). Owner Session only,
/// no PIN step-up: the flag reveals a redacted, read-only page, and step-up
/// is reserved for authority actions.
#[derive(Debug, Deserialize)]
pub struct DiagnosticsModeInput {
    pub enabled: bool,
}

/// Body of the owner's `POST /web-admin/transfer-requests/{confirm,reject}`
/// endpoints — the single-use 6-digit code the customer read aloud (ADR-0030).
#[derive(Debug, Deserialize)]
pub struct TransferCodeInput {
    pub code: String,
}

/// Owner-editable coin-device settings (ADR-0023 / migration 0007). Runtime
/// Coin Source configuration reads only `inter_coin_gap_ms`; automatic Coin
/// Batch authority is fixed at 100 cents per observed pulse. The retained
/// `pulse_map` supports physical calibration and historical Rejection Credit
/// denomination choices. `com_port` / `baud_rate` / line choices are consumed
/// when a real serial source is opened (the "test acceptor" action and M6
/// hardware bring-up). `com_port` is `None` while the shop runs on the
/// simulator. Money stays integer cents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoinDeviceSettings {
    pub com_port: Option<String>,
    pub baud_rate: i64,
    pub set_pin_line: String,
    pub coin_pin_line: String,
    pub coin_pin_polarity: String,
    pub inter_coin_gap_ms: i64,
    pub inactivity_timeout_seconds: i64,
    pub hard_cap_seconds: i64,
    pub pulse_map: Vec<PulseMapEntry>,
    /// Fresh-install ₱20 candidate remains diagnostic until Apply clears it.
    pub calibration_required: bool,
}

/// One retained pulse-count/denomination entry for physical calibration and
/// historical Rejection Credit. Automatic Coin Batch acceptance derives its
/// amount from the fixed 100-cents-per-pulse authority instead (ADR-0070).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulseMapEntry {
    pub pulse_count: i64,
    pub amount_cents: i64,
}

/// Coin-device settings are edited only through the `:8080` owner API (M4),
/// which authenticates via the session cookie — so unlike the desktop command
/// inputs this struct carries no `owner_token`.
#[derive(Debug, Deserialize)]
pub struct CoinDeviceSettingsInput {
    #[serde(default)]
    pub com_port: Option<String>,
    pub baud_rate: i64,
    pub set_pin_line: String,
    pub coin_pin_line: String,
    pub coin_pin_polarity: String,
    pub inter_coin_gap_ms: i64,
    #[serde(default)]
    pub inactivity_timeout_seconds: Option<i64>,
    #[serde(default)]
    pub hard_cap_seconds: Option<i64>,
    pub pulse_map: Vec<PulseMapEntry>,
}

/// Result of the owner-triggered "test acceptor" action. `simulator` is true
/// when no COM port is configured (the shop is on the simulator source); in
/// that case `ok` is true and no hardware is touched. With a port configured,
/// `ok` reflects whether the serial handle opened (set pin left inhibited).
#[derive(Debug, Clone, Serialize)]
pub struct CoinDeviceTestResult {
    pub ok: bool,
    pub simulator: bool,
    pub message: String,
}

/// The owner-facing S1 Acceptor Health contract. Inter-pulse timing remains
/// explicitly unavailable until filtered timing is implemented in S2.
#[derive(Debug, Clone, Serialize)]
pub struct AcceptorHealth {
    pub debug_window_start: String,
    pub timing_status: &'static str,
    pub implausible_timing_rate: Option<u32>,
}

/// Owner request for a non-transactional physical denomination capture.
/// Amount remains server-validated integer cents and is stored in the
/// in-memory capture session before any samples are accepted.
#[derive(Debug, Deserialize)]
pub struct CalibrationStartInput {
    pub target_amount_cents: i64,
}

/// Health snapshot for the Central Terminal kiosk (ADR-0023). The
/// simulator implementation is always `available`; the M4 serial source
/// will publish `out_of_order` whenever the COM port cannot be read. The
/// kiosk renders the matching screen from this and refuses to open a
/// `Selection Window` while `out_of_order` is the current state, so no
/// customer coin can ever be lost against a dead source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KioskStatusKind {
    Available,
    OutOfOrder,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KioskStatus {
    pub kind: KioskStatusKind,
    /// Short operator-facing reason. Empty when the source is healthy.
    /// Owners read this on the OOO screen; it must not contain secrets
    /// (the simulator never sets it; the M4 serial source can only ever
    /// set a stable platform error message).
    pub reason: String,
}

/// The owner-configured paid-time exchange. The API keeps the existing rates
/// table's amount/seconds vocabulary; the owner UI presents amount as pesos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShopRate {
    pub amount_cents: i64,
    pub seconds: i64,
}

/// Body of `PUT /web-admin/shop-rate`. The display name is derived from these
/// values on the server and is never accepted from the owner.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopRateInput {
    pub amount_cents: i64,
    pub seconds: i64,
}

/// The live time-purchase rate the kiosk needs to render its "equivalent
/// time" preview accurately (ADR-0008). The server is still authoritative
/// for the committed `seconds_added`; this is a read-only mirror so the
/// insert-screen preview matches the owner's configured rate instead of a
/// hard-coded constant. `seconds = inserted_cents * rate_seconds /
/// rate_amount_cents` (the same formula as `pricing::calculate_seconds`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KioskConfig {
    pub rate_amount_cents: i64,
    pub rate_seconds: i64,
}

/// Whether the Central Terminal window is pinned above every other window
/// (ADR-0056). Ephemeral: it is never persisted, and every launch starts
/// `locked: true`. The kiosk renders a header signal from this so the owner
/// can tell at a glance whether the pin is still on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct KioskLockState {
    pub locked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kiosk_lock_state_serializes_for_the_frontend() {
        let raw = serde_json::to_string(&KioskLockState { locked: true })
            .expect("kiosk lock state serializes");
        assert_eq!(raw, r#"{"locked":true}"#);
    }

    #[test]
    fn kiosk_status_serializes_in_snake_case_for_the_frontend() {
        // The Tauri event and command surface contract is snake_case JSON,
        // matching the M3 TypeScript types (`kind: "available" |
        // "out_of_order"`). Pin the wire format so a future rename cannot
        // silently break the kiosk UI.
        let available = KioskStatus {
            kind: KioskStatusKind::Available,
            reason: String::new(),
        };
        let raw = serde_json::to_string(&available).expect("available serializes");
        assert_eq!(raw, r#"{"kind":"available","reason":""}"#);

        let out_of_order = KioskStatus {
            kind: KioskStatusKind::OutOfOrder,
            reason: "coin acceptor offline".to_owned(),
        };
        let raw = serde_json::to_string(&out_of_order).expect("ooo serializes");
        assert_eq!(
            raw,
            r#"{"kind":"out_of_order","reason":"coin acceptor offline"}"#
        );
    }

    #[test]
    fn remote_device_claim_debug_redacts_secret() {
        let claim = CoinSourceClaim::RemoteDevice {
            device_id: "device-42".to_owned(),
            secret: "super-secret-key".to_owned(),
        };
        let debug_repr = format!("{claim:?}");
        assert!(
            debug_repr.contains("device-42"),
            "device_id should be visible"
        );
        assert!(
            !debug_repr.contains("super-secret-key"),
            "secret must not appear in debug output"
        );
        assert!(
            debug_repr.contains("[REDACTED]"),
            "debug output should contain [REDACTED]"
        );
    }
}
