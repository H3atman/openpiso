//! Telegram connect-and-protect (ADR-0076 / GitHub #345).
//!
//! The owner connects exactly one BotFather bot to one private chat. Setup is
//! guided: enter the token, Admin verifies it against the Bot API, a recent
//! private `/start` message identifies the destination chat, and a successful
//! test send commits the configuration atomically. Manual chat-id entry is a
//! fallback that still requires the same successful test. A failed test never
//! touches the previously working configuration.
//!
//! Security shape:
//! - Outbound only. The integration polls `getUpdates` solely inside an
//!   explicit setup detection call; there is no ongoing poller, webhook,
//!   inbound port, bot-command surface, group recipient, or second recipient.
//! - The token is protected at rest with Windows DPAPI under the Admin
//!   Windows identity ([`TokenProtector`]) and is never returned by any API
//!   response, Audit event, diagnostic, log line, or error string.
//! - Non-Windows Admin reports unavailable instead of storing plaintext or
//!   pretending to work.
//!
//! Both external boundaries are seams: [`TelegramTransport`] (Bot API over
//! Admin's existing Rust HTTPS client) and [`TokenProtector`] (DPAPI). Tests
//! inject deterministic fakes; production wiring lives in [`production_transport`]
//! and [`production_protector`].

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    db,
    errors::{AppError, AppResult},
};

/// Fixed test message sent when connecting or replacing the configuration.
const TEST_MESSAGE: &str =
    "OpenPiso test message: PC Monitoring alerts for this shop will arrive here.";

// ---------------------------------------------------------------------------
// Transport seam
// ---------------------------------------------------------------------------

/// The bot identity Telegram reports for a valid token (`getMe`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotIdentity {
    pub username: String,
}

/// One private chat that recently messaged the bot. Group and channel chats
/// are filtered out before this type exists; OpenPiso can never address them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedChat {
    pub chat_id: String,
    /// Display name of the human who messaged the bot, for owner confirmation.
    pub title: String,
}

/// Confirmation that a message reached its chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentMessage {
    pub message_id: i64,
}

/// Seam over the Telegram Bot API. Production uses Admin's existing Rust
/// HTTPS client (reqwest + rustls); tests inject a deterministic fake. Every
/// error returned here is already sanitized — implementations must never
/// embed the token in any error string (reqwest error text can contain the
/// request URL, which includes the token).
pub trait TelegramTransport: Send + Sync {
    fn verify_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, AppResult<BotIdentity>>;

    /// Distinct private chats with recent messages. Called only while the
    /// owner actively completes setup — never on a schedule.
    fn recent_private_chats<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, AppResult<Vec<DetectedChat>>>;

    fn send_message<'a>(
        &'a self,
        token: &'a str,
        chat_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, AppResult<SentMessage>>;
}

/// Dyn-safe boxed future, so the transport can live behind an `Arc` in server
/// state without adding an async-trait dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Production Bot API transport.
pub struct HttpTelegramTransport {
    client: reqwest::Client,
    base_url: String,
}

impl HttpTelegramTransport {
    /// Fallsible so a client-builder failure can never silently drop the
    /// bounded timeout below: an unbounded request could pin an owner request
    /// forever, which is worse than surfacing the error at startup.
    pub fn new(base_url: String) -> AppResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|error| AppError::Io(std::io::Error::other(error.to_string())))?;
        Ok(Self { client, base_url })
    }

    /// POST one Bot API call and return its `result` payload. All failures map
    /// to sanitized errors: status-code classification plus the API's own
    /// `description` field, which never contains the token.
    async fn call(
        &self,
        token: &str,
        method: &str,
        body: serde_json::Value,
    ) -> AppResult<serde_json::Value> {
        let url = format!("{}/bot{token}/{method}", self.base_url);
        let response = self.client.post(url).json(&body).send().await;
        let response = match response {
            Ok(response) => response,
            // Deliberately opaque: reqwest error text may embed the URL, and
            // the URL contains the token.
            Err(_) => return Err(AppError::TelegramUnavailable(NETWORK_MESSAGE.to_owned())),
        };
        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::NOT_FOUND => {
                return Err(AppError::InvalidInput(
                    "Telegram rejected this bot token".to_owned(),
                ));
            }
            status => {
                return Err(AppError::InvalidInput(format!(
                    "Telegram refused the request (HTTP {status})"
                )));
            }
        }
        let payload: serde_json::Value = match response.json().await {
            Ok(payload) => payload,
            Err(_) => return Err(AppError::TelegramUnavailable(NETWORK_MESSAGE.to_owned())),
        };
        if payload["ok"].as_bool() != Some(true) {
            let description = payload["description"].as_str().unwrap_or("unknown error");
            return Err(AppError::InvalidInput(format!(
                "Telegram refused the request: {description}"
            )));
        }
        Ok(payload["result"].clone())
    }
}

const NETWORK_MESSAGE: &str = "could not reach Telegram; check this Admin's internet connection";

impl TelegramTransport for HttpTelegramTransport {
    fn verify_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, AppResult<BotIdentity>> {
        Box::pin(async move {
            let result = self.call(token, "getMe", serde_json::json!({})).await?;
            let username = result["username"]
                .as_str()
                .ok_or_else(|| AppError::TelegramUnavailable(NETWORK_MESSAGE.to_owned()))?
                .to_owned();
            Ok(BotIdentity { username })
        })
    }

    fn recent_private_chats<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, AppResult<Vec<DetectedChat>>> {
        Box::pin(async move {
            let result = self
                .call(token, "getUpdates", serde_json::json!({}))
                .await?;
            Ok(private_chats_from_updates(&result, Utc::now().timestamp()))
        })
    }

    fn send_message<'a>(
        &'a self,
        token: &'a str,
        chat_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, AppResult<SentMessage>> {
        Box::pin(async move {
            let result = self
                .call(
                    token,
                    "sendMessage",
                    serde_json::json!({ "chat_id": chat_id, "text": text }),
                )
                .await?;
            let message_id = result["message_id"]
                .as_i64()
                .ok_or_else(|| AppError::TelegramUnavailable(NETWORK_MESSAGE.to_owned()))?;
            Ok(SentMessage { message_id })
        })
    }
}

fn number_to_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_i64()
        .map(|number| number.to_string())
        .or_else(|| value.as_str().map(str::to_owned))
}

/// Setup only accepts a `/start` sent in the last ten minutes. This keeps an
/// old conversation, or an unrelated private message, from becoming the
/// destination when the owner asks Admin to detect a chat.
const CHAT_DETECTION_RECENCY_SECS: i64 = 10 * 60;

/// Map one parsed `getUpdates` result onto distinct recent private `/start`
/// chats, keeping arrival order. Other messages, groups, channels, and
/// non-message updates are dropped before setup can see them.
fn private_chats_from_updates(result: &serde_json::Value, now_timestamp: i64) -> Vec<DetectedChat> {
    let mut chats: Vec<DetectedChat> = Vec::new();
    for update in result.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(message) = update.get("message") else {
            continue;
        };
        if message["chat"]["type"].as_str() != Some("private")
            || message["text"]
                .as_str()
                .and_then(|text| text.split_whitespace().next())
                != Some("/start")
        {
            continue;
        }
        let Some(sent_at) = message["date"].as_i64() else {
            continue;
        };
        let age_seconds = now_timestamp.saturating_sub(sent_at);
        if !(0..=CHAT_DETECTION_RECENCY_SECS).contains(&age_seconds) {
            continue;
        }
        let Some(chat_id) = number_to_string(&message["chat"]["id"]) else {
            continue;
        };
        if chats.iter().any(|chat| chat.chat_id == chat_id) {
            continue;
        }
        let from = &message["from"];
        let title = ["first_name", "last_name", "username"]
            .iter()
            .find_map(|field| from[field].as_str())
            .unwrap_or("Unknown sender")
            .to_owned();
        chats.push(DetectedChat { chat_id, title });
    }
    chats
}

/// Production transport selection. Fallsible for the same reason as
/// [`HttpTelegramTransport::new`]; Admin startup surfaces the error instead
/// of quietly losing the bounded timeout.
pub fn production_transport() -> AppResult<Arc<dyn TelegramTransport>> {
    Ok(Arc::new(HttpTelegramTransport::new(
        "https://api.telegram.org".to_owned(),
    )?))
}

// ---------------------------------------------------------------------------
// DPAPI protector seam
// ---------------------------------------------------------------------------

/// Seam over OS token protection. Production uses Windows DPAPI under the
/// Admin Windows identity; tests use a reversible fake. `supported()` reports
/// platform availability so a non-Windows Admin refuses setup instead of
/// storing readable secrets.
pub trait TokenProtector: Send + Sync {
    fn protect(&self, plaintext: &[u8]) -> AppResult<Vec<u8>>;
    fn unprotect(&self, ciphertext: &[u8]) -> AppResult<Vec<u8>>;
    fn supported(&self) -> bool;
}

/// Production adapter selection: Windows DPAPI on Windows, explicit
/// unavailability everywhere else.
pub fn production_protector() -> Arc<dyn TokenProtector> {
    #[cfg(windows)]
    {
        Arc::new(WindowsDpapiProtector)
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnsupportedProtector)
    }
}

/// Non-Windows stand-in: protection is impossible, so setup must refuse
/// rather than store a plaintext token.
#[cfg(any(not(windows), test))]
pub struct UnsupportedProtector;

#[cfg(any(not(windows), test))]
impl TokenProtector for UnsupportedProtector {
    fn protect(&self, _plaintext: &[u8]) -> AppResult<Vec<u8>> {
        Err(AppError::InvalidInput(
            "Connecting Telegram requires a Windows Admin".to_owned(),
        ))
    }

    fn unprotect(&self, _ciphertext: &[u8]) -> AppResult<Vec<u8>> {
        Err(AppError::InvalidInput(
            "Connecting Telegram requires a Windows Admin".to_owned(),
        ))
    }

    fn supported(&self) -> bool {
        false
    }
}

/// Windows DPAPI adapter. No optional entropy: scope is the current Windows
/// user by default, which is exactly "under the Admin identity".
#[cfg(windows)]
pub struct WindowsDpapiProtector;

#[cfg(windows)]
impl TokenProtector for WindowsDpapiProtector {
    fn protect(&self, plaintext: &[u8]) -> AppResult<Vec<u8>> {
        use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len().min(u32::MAX as usize) as u32,
            pbData: plaintext.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: `input` borrows `plaintext` for the duration of the call and
        // `output` is written by the API; both blobs are valid.
        unsafe { CryptProtectData(&input, None, None, None, None, 0, &mut output) }
            .map_err(dpapi_error)?;
        take_blob(output)
    }

    fn unprotect(&self, ciphertext: &[u8]) -> AppResult<Vec<u8>> {
        use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len().min(u32::MAX as usize) as u32,
            pbData: ciphertext.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: same blob discipline as `protect`.
        unsafe { CryptUnprotectData(&input, None, None, None, None, 0, &mut output) }
            .map_err(dpapi_error)?;
        take_blob(output)
    }

    fn supported(&self) -> bool {
        true
    }
}

/// Move a DPAPI-owned output blob into a Rust Vec, freeing the LocalAlloc
/// buffer either way. An empty blob means the API produced nothing usable.
#[cfg(windows)]
fn take_blob(
    blob: windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> AppResult<Vec<u8>> {
    use windows::Win32::Foundation::LocalFree;
    let bytes = if blob.pbData.is_null() || blob.cbData == 0 {
        Vec::new()
    } else {
        // SAFETY: `pbData` points at `cbData` bytes allocated by DPAPI for us.
        unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec()
    };
    // SAFETY: the buffer was allocated by the API above and freed exactly once.
    unsafe {
        LocalFree(Some(windows::Win32::Foundation::HLOCAL(blob.pbData.cast())));
    }
    if bytes.is_empty() {
        return Err(AppError::TelegramUnavailable(
            "Windows could not protect the bot token".to_owned(),
        ));
    }
    Ok(bytes)
}

#[cfg(windows)]
fn dpapi_error(error: windows::core::Error) -> AppError {
    // The OS error text never contains the token; it names an API failure.
    let _ = error;
    AppError::TelegramUnavailable("Windows token protection failed".to_owned())
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn hex_decode(text: &str) -> AppResult<Vec<u8>> {
    let decode_failure = || {
        AppError::InvalidInput(
            "Stored Telegram token is not readable on this installation".to_owned(),
        )
    };
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hex = std::str::from_utf8(pair).map_err(|_| decode_failure())?;
            u8::from_str_radix(hex, 16).map_err(|_| decode_failure())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Owner-facing status types (never contain the token)
// ---------------------------------------------------------------------------

/// Full integration state for Settings. Serializes without any secret field
/// by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramStatus {
    pub platform_supported: bool,
    pub connected: bool,
    /// Present while a verified token awaits confirmation, so the UI can say
    /// which bot is being connected. Contains no token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_bot_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<TelegramDestination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_test: Option<TelegramLastTest>,
    /// Incident-delivery health (#346): last success, last error, pending
    /// queue state for Settings' inline display. Present while connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<db::telegram::DeliveryHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramDestination {
    pub bot_username: String,
    pub chat_title: String,
    pub chat_id: String,
    pub connected_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramLastTest {
    /// When the stored configuration last passed a real test send.
    pub verified_at: String,
}

/// A token verified against the Bot API but not yet confirmed with a test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupCandidate {
    pub bot_username: String,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Verified-token candidate held in memory only. Never persisted: if Admin
/// restarts mid-setup the owner simply starts setup again, and no unreadable
/// half-configuration can ever exist at rest. Holds the raw token, so it has
/// no `Debug`/`Serialize` impls by design.
struct PendingSetup {
    token: String,
    bot_username: String,
    /// Private chats found by the latest explicit detection call.
    detected: Vec<DetectedChat>,
}

pub struct TelegramService {
    pool: SqlitePool,
    transport: Arc<dyn TelegramTransport>,
    protector: Arc<dyn TokenProtector>,
    pending: Mutex<Option<PendingSetup>>,
}

impl TelegramService {
    /// Lock the pending-setup state, recovering from a poisoned mutex instead
    /// of panicking: the guarded candidate is plain data a previous request
    /// left behind mid-update, so the worst case is a stale half-candidate
    /// the owner replaces by finishing setup again (repo convention: no
    /// `expect` in production paths).
    fn lock_pending(&self) -> std::sync::MutexGuard<'_, Option<PendingSetup>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn new(
        pool: SqlitePool,
        transport: Arc<dyn TelegramTransport>,
        protector: Arc<dyn TokenProtector>,
    ) -> Self {
        Self {
            pool,
            transport,
            protector,
            pending: Mutex::new(None),
        }
    }

    /// Current state for Settings. Reads storage only; never decrypts, so it
    /// cannot leak the token through any path.
    pub async fn status(&self) -> AppResult<TelegramStatus> {
        let stored = db::telegram::config(&self.pool).await?;
        let pending_bot_username = self
            .lock_pending()
            .as_ref()
            .map(|setup| setup.bot_username.clone());
        Ok(match stored {
            Some(config) => TelegramStatus {
                platform_supported: self.protector.supported(),
                connected: true,
                pending_bot_username,
                destination: Some(TelegramDestination {
                    bot_username: config.bot_username,
                    chat_title: config.chat_title,
                    chat_id: config.chat_id,
                    connected_at: config.connected_at,
                }),
                last_test: Some(TelegramLastTest {
                    verified_at: config.verified_at,
                }),
                // Read-only outbox aggregates; cannot leak the token.
                delivery: Some(db::telegram::delivery_health(&self.pool).await?),
            },
            None => TelegramStatus {
                platform_supported: self.protector.supported(),
                connected: false,
                pending_bot_username,
                destination: None,
                last_test: None,
                delivery: None,
            },
        })
    }

    /// Verify a candidate token against the Bot API and hold it in memory for
    /// confirmation. Nothing is persisted; a previous working configuration
    /// stays fully active meanwhile.
    pub async fn submit_token(&self, raw_token: &str) -> AppResult<SetupCandidate> {
        if !self.protector.supported() {
            return Err(AppError::InvalidInput(
                "Connecting Telegram requires a Windows Admin".to_owned(),
            ));
        }
        let token = raw_token.trim();
        if token.is_empty() {
            return Err(AppError::InvalidInput("A bot token is required".to_owned()));
        }
        // Shape check before spending a network call. BotFather tokens are
        // `<digits>:<secret>` with no internal whitespace.
        let looks_like_token = token.contains(':')
            && !token.split(':').next_back().unwrap_or("").is_empty()
            && !token.chars().any(char::is_whitespace);
        if !looks_like_token {
            return Err(AppError::InvalidInput(
                "That does not look like a BotFather token".to_owned(),
            ));
        }
        let identity = self.transport.verify_token(token).await?;
        *self.lock_pending() = Some(PendingSetup {
            token: token.to_owned(),
            bot_username: identity.username.clone(),
            detected: Vec::new(),
        });
        Ok(SetupCandidate {
            bot_username: identity.username,
        })
    }

    /// Look for recent private `/start`-style messages once, using the pending
    /// token. This explicit call during setup is the only polling the
    /// integration ever does; groups and channels were already filtered out by
    /// the transport seam.
    pub async fn detect_chat(&self) -> AppResult<Vec<DetectedChat>> {
        let token = {
            let mut pending = self.lock_pending();
            let Some(setup) = pending.as_mut() else {
                return Err(AppError::InvalidInput(
                    "Enter a verified bot token before detecting the chat".to_owned(),
                ));
            };
            setup.token.clone()
        };
        let chats = self.transport.recent_private_chats(&token).await?;
        // Distinctness is an invariant at this boundary: even a transport that
        // repeats a chat id must never produce two candidates for one chat.
        let mut distinct: Vec<DetectedChat> = Vec::new();
        for chat in chats {
            if !distinct.iter().any(|known| known.chat_id == chat.chat_id) {
                distinct.push(chat);
            }
        }
        let chats = distinct;
        // Store results only if the same candidate is still pending; a
        // restart of setup during detection invalidates them.
        let mut pending = self.lock_pending();
        match pending.as_mut() {
            Some(setup) if setup.token == token => setup.detected = chats.clone(),
            _ => {
                return Err(AppError::InvalidInput(
                    "Start setup again by entering your bot token".to_owned(),
                ));
            }
        }
        Ok(chats)
    }

    /// Confirm the destination and make it active: send the test message, and
    /// only on success protect the token with DPAPI and commit configuration +
    /// Audit in one transaction. Failure leaves the existing working
    /// configuration untouched and keeps the pending token available for a
    /// retry.
    ///
    /// `manual_chat_id` is the fallback path: a decimal private-chat id typed
    /// by the owner. It still goes through the same successful-test gate.
    pub async fn connect(&self, manual_chat_id: Option<&str>) -> AppResult<TelegramStatus> {
        if !self.protector.supported() {
            return Err(AppError::InvalidInput(
                "Connecting Telegram requires a Windows Admin".to_owned(),
            ));
        }
        let (token, bot_username, chat_id, chat_title) =
            self.resolve_destination(manual_chat_id).await?;

        // Test first. Only a delivered message may advance anything.
        self.transport
            .send_message(&token, &chat_id, TEST_MESSAGE)
            .await?;

        let protected = self.protector.protect(token.as_bytes())?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        db::telegram::connect_with_audit(
            &self.pool,
            &db::telegram::TelegramConfigRow {
                bot_username: bot_username.clone(),
                chat_id: chat_id.clone(),
                chat_title: chat_title.clone(),
                token_protected: hex_encode(&protected),
                connected_at: now.clone(),
                verified_at: now,
            },
        )
        .await?;
        *self.lock_pending() = None;
        self.status().await
    }

    /// Delete the protected token and destination. Local PC Monitoring keeps
    /// running unchanged: incidents, settings, and history are other tables.
    /// Returns whether a stored configuration existed.
    pub async fn disconnect(&self) -> AppResult<bool> {
        *self.lock_pending() = None;
        db::telegram::disconnect_with_audit(&self.pool).await
    }

    /// Resolve `(token, bot_username, chat_id, chat_title)`. Manual ids are
    /// validated here at the boundary: a positive decimal integer only,
    /// because Telegram private chat ids are positive and everything else
    /// (groups are negative, channels differ) is not a private chat.
    async fn resolve_destination(
        &self,
        manual_chat_id: Option<&str>,
    ) -> AppResult<(String, String, String, String)> {
        let pending = self.lock_pending();
        let Some(setup) = pending.as_ref() else {
            return Err(AppError::InvalidInput(
                "Start setup again by entering your bot token".to_owned(),
            ));
        };
        if let Some(manual) = manual_chat_id.map(str::trim).filter(|id| !id.is_empty()) {
            let parsed = manual.parse::<i64>().map_err(|_| invalid_chat_id_error())?;
            if parsed <= 0 {
                return Err(invalid_chat_id_error());
            }
            return Ok((
                setup.token.clone(),
                setup.bot_username.clone(),
                parsed.to_string(),
                format!("Manually entered chat {parsed}"),
            ));
        }
        match setup.detected.len() {
            1 => Ok((
                setup.token.clone(),
                setup.bot_username.clone(),
                setup.detected[0].chat_id.clone(),
                setup.detected[0].title.clone(),
            )),
            0 => Err(AppError::InvalidInput(
                "No recent private /start detected. Send /start to the bot, \
                 or enter the chat ID manually."
                    .to_owned(),
            )),
            _ => Err(AppError::InvalidInput(
                "More than one private chat messaged this bot. Select one chat.".to_owned(),
            )),
        }
    }
}

fn invalid_chat_id_error() -> AppError {
    AppError::InvalidInput(
        "The chat ID must be the numeric ID of a private chat (positive digits)".to_owned(),
    )
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{
        AppError, AppResult, BotIdentity, BoxFuture, DetectedChat, SentMessage, TelegramTransport,
        TokenProtector,
    };

    /// Deterministic Bot API double. Scripts per-token identities, private
    /// chats, send outcomes, and network availability. Records every send.
    pub struct FakeTransport {
        inner: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        identities: HashMap<String, String>,
        chats: HashMap<String, Vec<DetectedChat>>,
        sends: Vec<(String, String, String)>,
        /// Chat ids whose `sendMessage` fails with a rejection.
        failing_chats: Vec<String>,
        network_down: bool,
    }

    impl FakeTransport {
        pub fn new() -> Self {
            Self {
                inner: Mutex::new(FakeState::default()),
            }
        }

        pub fn add_bot(&self, token: &str, username: &str) -> &Self {
            self.inner
                .lock()
                .expect("fake transport")
                .identities
                .insert(token.to_owned(), username.to_owned());
            self
        }

        /// Script the private chats detection reports for one token.
        pub fn set_private_chats(&self, token: &str, chats: Vec<DetectedChat>) -> &Self {
            self.inner
                .lock()
                .expect("fake transport")
                .chats
                .insert(token.to_owned(), chats);
            self
        }

        pub fn fail_sends_to_chat(&self, chat_id: &str) -> &Self {
            self.inner
                .lock()
                .expect("fake transport")
                .failing_chats
                .push(chat_id.to_owned());
            self
        }

        pub fn fail_sends_to_chat_clear(&self, chat_id: &str) {
            self.inner
                .lock()
                .expect("fake transport")
                .failing_chats
                .retain(|failing| failing != chat_id);
        }

        pub fn set_network_down(&self, down: bool) -> &Self {
            self.inner.lock().expect("fake transport").network_down = down;
            self
        }

        pub fn sends(&self) -> Vec<(String, String, String)> {
            self.inner.lock().expect("fake transport").sends.clone()
        }
    }

    impl TelegramTransport for FakeTransport {
        fn verify_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, AppResult<BotIdentity>> {
            Box::pin(async move {
                let state = self.inner.lock().expect("fake transport");
                if state.network_down {
                    return Err(AppError::TelegramUnavailable(
                        "could not reach Telegram; check this Admin's internet connection"
                            .to_owned(),
                    ));
                }
                match state.identities.get(token) {
                    Some(username) => Ok(BotIdentity {
                        username: username.clone(),
                    }),
                    None => Err(AppError::InvalidInput(
                        "Telegram rejected this bot token".to_owned(),
                    )),
                }
            })
        }

        fn recent_private_chats<'a>(
            &'a self,
            token: &'a str,
        ) -> BoxFuture<'a, AppResult<Vec<DetectedChat>>> {
            Box::pin(async move {
                let state = self.inner.lock().expect("fake transport");
                if state.network_down {
                    return Err(AppError::TelegramUnavailable("offline".to_owned()));
                }
                Ok(state.chats.get(token).cloned().unwrap_or_default())
            })
        }

        fn send_message<'a>(
            &'a self,
            token: &'a str,
            chat_id: &'a str,
            text: &'a str,
        ) -> BoxFuture<'a, AppResult<SentMessage>> {
            Box::pin(async move {
                let mut state = self.inner.lock().expect("fake transport");
                if state.network_down {
                    return Err(AppError::TelegramUnavailable("offline".to_owned()));
                }
                if state.failing_chats.iter().any(|failing| failing == chat_id) {
                    return Err(AppError::InvalidInput(
                        "Telegram refused the request: chat not found".to_owned(),
                    ));
                }
                let message_id = state.sends.len() as i64 + 1;
                state
                    .sends
                    .push((token.to_owned(), chat_id.to_owned(), text.to_owned()));
                Ok(SentMessage { message_id })
            })
        }
    }

    /// Reversible protection double: prefixes plaintext so tests can prove
    /// what is stored is not the plaintext while still recovering it.
    pub struct FakeProtector {
        supported: std::sync::atomic::AtomicBool,
    }

    impl FakeProtector {
        pub fn new(supported: bool) -> Self {
            Self {
                supported: std::sync::atomic::AtomicBool::new(supported),
            }
        }
    }

    const FAKE_PREFIX: u8 = 0xA7;

    impl TokenProtector for FakeProtector {
        fn protect(&self, plaintext: &[u8]) -> AppResult<Vec<u8>> {
            if !self.supported() {
                return Err(AppError::InvalidInput(
                    "Connecting Telegram requires a Windows Admin".to_owned(),
                ));
            }
            let mut protected = vec![FAKE_PREFIX];
            protected.extend_from_slice(plaintext);
            Ok(protected)
        }

        fn unprotect(&self, ciphertext: &[u8]) -> AppResult<Vec<u8>> {
            ciphertext
                .strip_prefix(&[FAKE_PREFIX][..])
                .map(<[u8]>::to_vec)
                .ok_or_else(|| {
                    AppError::InvalidInput("stored Telegram token is not readable".to_owned())
                })
        }

        fn supported(&self) -> bool {
            self.supported.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::testing::{FakeProtector, FakeTransport};
    use super::*;
    use crate::db::monitoring::SETTINGS_KEY_ENABLED;

    const TOKEN: &str = "123456:ABC-DEF_secret-token-value";
    const CHAT_ID: &str = "424242";

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn happy_path_setup(service: &TelegramService, transport: &FakeTransport) {
        transport.add_bot(TOKEN, "shop_alerts_bot");
        transport.set_private_chats(
            TOKEN,
            vec![DetectedChat {
                chat_id: CHAT_ID.to_owned(),
                title: "Owner".to_owned(),
            }],
        );
        service.submit_token(TOKEN).await.expect("token verified");
        service.detect_chat().await.expect("chat detected");
    }

    #[tokio::test]
    async fn fresh_installation_reads_disconnected_and_platform_aware() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );

        let status = service.status().await?;
        assert!(!status.connected);
        assert!(status.platform_supported);
        assert_eq!(status.destination, None);
        assert_eq!(status.last_test, None);
        Ok(())
    }

    #[tokio::test]
    async fn non_windows_admin_reports_unavailable_and_refuses_setup() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(UnsupportedProtector),
        );

        let status = service.status().await?;
        assert!(!status.platform_supported);

        let error = service.submit_token(TOKEN).await.unwrap_err();
        assert!(error.to_string().contains("Windows Admin"));
        // Nothing pending, nothing stored.
        let status = service.status().await?;
        assert_eq!(status.pending_bot_username, None);
        assert!(!status.connected);
        Ok(())
    }

    #[tokio::test]
    async fn malformed_tokens_are_refused_before_any_network_call() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );

        for attempt in ["", "   ", "no-colon-token", "has space:in-it"] {
            let result = service.submit_token(attempt).await;
            assert!(result.is_err(), "'{attempt}' should be refused");
        }
        assert!(transport.sends().is_empty());
        assert_eq!(service.status().await?.pending_bot_username, None);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_token_error_is_sanitized_and_names_no_token() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new()); // knows no tokens
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );

        let error = service.submit_token(TOKEN).await.unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("rejected this bot token"));
        assert!(!rendered.contains(TOKEN));
        assert!(!rendered.contains("ABC-DEF"));
        Ok(())
    }

    #[tokio::test]
    async fn detection_lists_only_distinct_private_chats() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        transport.add_bot(TOKEN, "shop_alerts_bot");
        // The fake returns private rows including a duplicate; group and
        // channel filtering belongs to the production transport's update
        // mapping and is exercised there by the raw-update parsing test.
        transport.set_private_chats(
            TOKEN,
            vec![
                DetectedChat {
                    chat_id: "111".to_owned(),
                    title: "Owner".to_owned(),
                },
                DetectedChat {
                    chat_id: "111".to_owned(),
                    title: "Owner Again".to_owned(),
                },
                DetectedChat {
                    chat_id: "222".to_owned(),
                    title: "Second Phone".to_owned(),
                },
            ],
        );
        service.submit_token(TOKEN).await?;

        let chats = service.detect_chat().await?;
        assert_eq!(
            chats,
            vec![
                DetectedChat {
                    chat_id: "111".to_owned(),
                    title: "Owner".to_owned()
                },
                DetectedChat {
                    chat_id: "222".to_owned(),
                    title: "Second Phone".to_owned()
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn detection_requires_a_verified_pending_token() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );

        let error = service.detect_chat().await.unwrap_err();
        assert!(error.to_string().contains("verified bot token"));
        Ok(())
    }

    #[test]
    fn update_parsing_keeps_only_recent_private_start_messages() {
        const NOW: i64 = 10_000;
        let result = serde_json::json!([
            { "update_id": 1, "message": {
                "message_id": 10, "date": NOW - 30, "text": "/start",
                "from": { "first_name": "Rafa", "username": "owner" },
                "chat": { "id": 4242, "type": "private" } } },
            // An unrelated private message cannot select a destination.
            { "update_id": 2, "message": {
                "message_id": 11, "date": NOW - 10, "text": "hello",
                "from": { "first_name": "Other" },
                "chat": { "id": 5000, "type": "private" } } },
            // A stale /start cannot revive an old conversation.
            { "update_id": 3, "message": {
                "message_id": 12, "date": NOW - CHAT_DETECTION_RECENCY_SECS - 1,
                "text": "/start", "from": { "first_name": "Old" },
                "chat": { "id": 6000, "type": "private" } } },
            // A group message: never a candidate destination.
            { "update_id": 4, "message": {
                "message_id": 13, "date": NOW, "text": "/start",
                "from": { "first_name": "Stranger" },
                "chat": { "id": -100200, "type": "group" } } },
            // A channel post: no from, wrong type.
            { "update_id": 5, "channel_post": {
                "chat": { "id": -100300, "type": "channel" } } },
            // Duplicate private chat: deduplicated by id.
            { "update_id": 6, "message": {
                "message_id": 14, "date": NOW, "text": "/start payload",
                "from": { "first_name": "Rafa" },
                "chat": { "id": 4242, "type": "private" } } }
        ]);

        assert_eq!(
            private_chats_from_updates(&result, NOW),
            vec![DetectedChat {
                chat_id: "4242".to_owned(),
                title: "Rafa".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn detected_chat_connect_sends_test_then_persists_protected_token() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        happy_path_setup(&service, &transport).await;

        let status = service.connect(None).await?;

        // The test message went to the detected chat before anything persisted.
        assert_eq!(
            transport.sends(),
            vec![(
                TOKEN.to_owned(),
                CHAT_ID.to_owned(),
                TEST_MESSAGE.to_owned()
            )]
        );
        assert!(status.connected);
        let destination = status.destination.expect("destination");
        assert_eq!(destination.bot_username, "shop_alerts_bot");
        assert_eq!(destination.chat_id, CHAT_ID);
        assert_eq!(destination.chat_title, "Owner");
        assert!(status.last_test.is_some());

        // Stored ciphertext is not the token, and only the protector can read it.
        let stored = db::telegram::config(&pool).await?.expect("config row");
        assert_ne!(stored.token_protected, TOKEN);
        assert!(!stored.token_protected.contains("ABC-DEF"));
        let protector = FakeProtector::new(true);
        let recovered = protector
            .unprotect(&hex_decode(&stored.token_protected)?)
            .expect("recoverable");
        assert_eq!(String::from_utf8(recovered).expect("utf8"), TOKEN);

        // Pending setup cleared; connect audited once with identity metadata only.
        assert_eq!(service.status().await?.pending_bot_username, None);
        let metadata: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'telegram_connected'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(metadata.contains("shop_alerts_bot"));
        assert!(!metadata.contains(TOKEN));
        Ok(())
    }

    #[tokio::test]
    async fn manual_fallback_chat_id_connects_after_successful_test() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        transport.add_bot(TOKEN, "shop_alerts_bot");
        service.submit_token(TOKEN).await?;

        let status = service.connect(Some("987654321")).await?;

        assert!(status.connected);
        assert_eq!(
            status.destination.expect("destination").chat_id,
            "987654321"
        );
        assert_eq!(
            transport.sends()[0].1,
            "987654321",
            "the test must target the manual destination too"
        );
        let stored = db::telegram::config(&pool).await?.expect("config row");
        let protector = FakeProtector::new(true);
        let recovered = protector
            .unprotect(&hex_decode(&stored.token_protected)?)
            .expect("recoverable");
        assert_eq!(String::from_utf8(recovered).expect("utf8"), TOKEN);
        Ok(())
    }

    #[tokio::test]
    async fn manual_fallback_rejects_non_private_chat_ids() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool,
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        transport.add_bot(TOKEN, "shop_alerts_bot");
        service.submit_token(TOKEN).await?;

        // Negative ids are groups; zero and garbage are not private chats.
        for attempt in ["-100200", "0", "abc", "-5"] {
            let result = service.connect(Some(attempt)).await;
            assert!(result.is_err(), "'{attempt}' must be refused");
            assert!(
                transport.sends().is_empty(),
                "nothing may be sent to '{attempt}'"
            );
        }
        assert!(!service.status().await?.connected);
        Ok(())
    }

    #[tokio::test]
    async fn successful_replacement_replaces_the_working_configuration() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        happy_path_setup(&service, &transport).await;
        service.connect(None).await?;

        // A different bot + chat replaces everything after its own passed test.
        const NEW_TOKEN: &str = "987654:NEW-token-value";
        transport.add_bot(NEW_TOKEN, "replacement_bot");
        transport.set_private_chats(
            NEW_TOKEN,
            vec![DetectedChat {
                chat_id: "777".to_owned(),
                title: "New Owner".to_owned(),
            }],
        );
        service.submit_token(NEW_TOKEN).await?;
        service.detect_chat().await?;
        let status = service.connect(None).await?;

        let stored = db::telegram::config(&pool).await?.expect("config row");
        assert_eq!(stored.bot_username, "replacement_bot");
        assert_eq!(stored.chat_id, "777");
        let protector = FakeProtector::new(true);
        let recovered = protector
            .unprotect(&hex_decode(&stored.token_protected)?)
            .expect("recoverable");
        assert_eq!(String::from_utf8(recovered).expect("utf8"), NEW_TOKEN);
        assert_eq!(
            status.destination.expect("destination").chat_title,
            "New Owner"
        );

        // Exactly one row remains.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_config")
            .fetch_one(&pool)
            .await?;
        assert_eq!(rows, 1);
        assert!(status.last_test.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn failed_replacement_leaves_the_working_configuration_untouched() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        happy_path_setup(&service, &transport).await;
        service.connect(None).await?;
        let working = db::telegram::config(&pool).await?.expect("working config");

        // A new candidate fails its test: nothing may change at rest.
        const BAD_TOKEN: &str = "555:broken-replacement";
        transport.add_bot(BAD_TOKEN, "replacement_bot");
        transport.set_private_chats(
            BAD_TOKEN,
            vec![DetectedChat {
                chat_id: CHAT_ID.to_owned(),
                title: "Owner".to_owned(),
            }],
        );
        transport.fail_sends_to_chat(CHAT_ID);
        service.submit_token(BAD_TOKEN).await?;
        service.detect_chat().await?;

        let error = service.connect(None).await.unwrap_err();
        assert!(error.to_string().contains("refused the request"));

        let still_working = db::telegram::config(&pool)
            .await?
            .expect("untouched config");
        assert_eq!(still_working, working);

        // The old configuration remains active; the pending candidate survives
        // for a retry without re-entering the token.
        let status = service.status().await?;
        assert!(status.connected);
        assert_eq!(
            status.destination.expect("old destination").bot_username,
            "shop_alerts_bot"
        );
        assert_eq!(
            status.pending_bot_username.as_deref(),
            Some("replacement_bot")
        );

        // Fixing the failure completes the replacement on retry.
        transport.fail_sends_to_chat_clear(CHAT_ID);
        let status = service.connect(None).await?;
        assert_eq!(
            status.destination.expect("new destination").chat_id,
            CHAT_ID
        );
        let replaced = db::telegram::config(&pool).await?.expect("replaced config");
        let protector = FakeProtector::new(true);
        let recovered = protector
            .unprotect(&hex_decode(&replaced.token_protected)?)
            .expect("recoverable");
        assert_eq!(String::from_utf8(recovered).expect("utf8"), BAD_TOKEN);
        Ok(())
    }

    #[tokio::test]
    async fn network_failure_is_reported_sanitized_without_touching_storage() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        transport.add_bot(TOKEN, "shop_alerts_bot");
        service.submit_token(TOKEN).await?;

        transport.set_network_down(true);
        let error = service.connect(Some(CHAT_ID)).await.unwrap_err();
        assert!(matches!(error, AppError::TelegramUnavailable(_)));
        assert!(!error.to_string().contains(TOKEN));
        transport.set_network_down(false);

        assert_eq!(db::telegram::config(&pool).await?, None);
        let status = service.status().await?;
        assert!(!status.connected);
        assert_eq!(
            status.pending_bot_username.as_deref(),
            Some("shop_alerts_bot")
        );
        Ok(())
    }

    #[tokio::test]
    async fn redaction_status_errors_and_audit_never_carry_the_token() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        happy_path_setup(&service, &transport).await;
        service.connect(None).await?;

        let status_json = serde_json::to_string(&service.status().await?).expect("serializable");
        assert!(!status_json.contains(TOKEN));
        assert!(!status_json.contains("ABC-DEF"));

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM audit_log WHERE event_type LIKE 'telegram%'")
                .fetch_all(&pool)
                .await?;
        assert!(!summaries.is_empty());
        assert!(summaries.iter().all(|summary| !summary.contains(TOKEN)));
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_deletes_integration_but_leaves_monitoring_alone() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let service = TelegramService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            Arc::new(FakeProtector::new(true)),
        );
        happy_path_setup(&service, &transport).await;
        service.connect(None).await?;

        // Local monitoring has its own durable state that must survive.
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, 'true')
             ON CONFLICT(key) DO UPDATE SET value = 'true'",
        )
        .bind(SETTINGS_KEY_ENABLED)
        .execute(&pool)
        .await?;

        assert!(service.disconnect().await?);
        assert!(
            !service.disconnect().await?,
            "double disconnect stays success"
        );
        let status = service.status().await?;
        assert!(!status.connected);
        assert_eq!(status.destination, None);

        let enabled: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(SETTINGS_KEY_ENABLED)
                .fetch_one(&pool)
                .await?;
        assert_eq!(enabled.as_deref(), Some("true"));

        let disconnect_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'telegram_disconnected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(disconnect_audits, 1);
        Ok(())
    }

    /// Windows-only guard for the real DPAPI adapter: a protect/unprotect
    /// roundtrip under the current user identity. Mirrors the ICMP loopback
    /// adapter guard.
    #[cfg(windows)]
    #[test]
    fn windows_dpapi_roundtrips_under_admin_identity() {
        let protector = WindowsDpapiProtector;
        assert!(protector.supported());
        let secret = b"123456:roundtrip-secret";
        let protected = protector.protect(secret).expect("protect");
        assert_ne!(protected, secret.to_vec());
        let recovered = protector.unprotect(&protected).expect("unprotect");
        assert_eq!(recovered, secret);
    }
}
