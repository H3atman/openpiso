//! Owner-initiated Admin App Update (ADR-0086).

mod installer;

pub use installer::TauriInstaller;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};

use crate::{
    db,
    errors::{AppError, AppResult},
    sessions::SessionStore,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub current_version: String,
    pub current_schema_version: i64,
    pub auto_check: bool,
    pub last_checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<Release>,
    pub rollbacks: Vec<Release>,
    pub running_session_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<Download>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<UpdateResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResult {
    pub from: String,
    pub to: String,
    pub ok: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub target_version: String,
    pub percent: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartMarker {
    pub from_version: String,
    pub target_version: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub schema_version: i64,
    pub notes: String,
    pub pub_date: DateTime<Utc>,
    pub latest_json_url: String,
    pub raises_schema: bool,
}

/// Lists stable releases without sending shop data to the release host.
pub trait ReleaseFeed: Send + Sync {
    fn releases(&self) -> BoxFuture<'_, AppResult<Vec<Release>>>;
}

/// Download progress is an integer percentage, reported by the Installer.
pub type DownloadProgress = Arc<dyn Fn(u8) + Send + Sync>;

/// Downloads and verifies the chosen release without starting its installer.
/// `rollback` is true only for a server-validated Compatible Rollback.
pub trait Installer: Send + Sync {
    fn prepare(
        &self,
        release: Release,
        rollback: bool,
        progress: DownloadProgress,
    ) -> BoxFuture<'_, AppResult<Box<dyn PreparedInstall>>>;
}

/// The service persists the restart marker and audit before calling this.
/// Production hides the kiosk and starts the verified passive NSIS installer.
pub trait PreparedInstall: Send {
    fn install(self: Box<Self>) -> BoxFuture<'static, AppResult<()>>;
}

pub const RELEASE_REPOSITORY: &str = "H3atman/openpiso";
const GITHUB_API: &str = "https://api.github.com";

pub struct GitHubReleaseFeed {
    client: reqwest::Client,
    base_url: reqwest::Url,
    fixture_assets: bool,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct ReleaseMetadata {
    version: String,
    schema_version: i64,
    notes: String,
    pub_date: DateTime<Utc>,
}

impl GitHubReleaseFeed {
    pub fn new() -> AppResult<Self> {
        // This is deliberately compiled out of release builds, not merely
        // disabled by configuration. No production shop can redirect the feed.
        #[cfg(debug_assertions)]
        let base = std::env::var("OPENPISO_RELEASE_FEED_BASE_URL")
            .unwrap_or_else(|_| GITHUB_API.to_owned());
        #[cfg(not(debug_assertions))]
        let base = GITHUB_API.to_owned();
        Self::from_base_url(&base)
    }

    #[cfg(test)]
    pub fn with_base_url(base: &str) -> AppResult<Self> {
        Self::from_base_url(base)
    }

    fn from_base_url(base: &str) -> AppResult<Self> {
        let base_url = reqwest::Url::parse(base)
            .map_err(|_| AppError::InvalidInput("invalid Release Feed base URL".to_owned()))?;
        let fixture_assets = cfg!(any(test, debug_assertions))
            && base_url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
        if (base_url.scheme() != "https" && !(fixture_assets && base_url.scheme() == "http"))
            || !base_url.username().is_empty()
            || base_url.password().is_some()
        {
            return Err(AppError::InvalidInput(
                "invalid Release Feed base URL".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .user_agent("OpenPiso-App-Update")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| AppError::Internal("Release Feed HTTP client unavailable".to_owned()))?;
        Ok(Self {
            client,
            base_url,
            fixture_assets,
        })
    }

    fn asset_url(&self, asset: &GitHubAsset) -> AppResult<reqwest::Url> {
        let url = reqwest::Url::parse(&asset.browser_download_url)
            .map_err(|_| AppError::Internal("invalid Release Feed asset URL".to_owned()))?;
        let github = url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url
                .path()
                .starts_with(&format!("/{RELEASE_REPOSITORY}/releases/download/"));
        let fixture = self.fixture_assets && url.origin() == self.base_url.origin();
        if (!github && !fixture) || !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::Internal(
                "untrusted Release Feed asset URL".to_owned(),
            ));
        }
        Ok(url)
    }

    async fn read_json<T: serde::de::DeserializeOwned>(&self, url: reqwest::Url) -> AppResult<T> {
        let mut response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| AppError::Internal("Release Feed network unavailable".to_owned()))?;
        if !response.status().is_success() {
            return Err(AppError::Internal(format!(
                "Release Feed request failed (HTTP {})",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AppError::Internal("Release Feed response interrupted".to_owned()))?
        {
            if body.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(AppError::Internal(
                    "Release Feed response too large".to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| AppError::Internal("invalid Release Feed JSON".to_owned()))
    }

    async fn fetch(&self) -> AppResult<Vec<Release>> {
        let mut releases = Vec::new();
        for page in 1..=20 {
            let mut url = self
                .base_url
                .join(&format!("/repos/{RELEASE_REPOSITORY}/releases"))
                .map_err(|_| AppError::Internal("invalid Release Feed URL".to_owned()))?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let listings: Vec<GitHubRelease> = self.read_json(url).await?;
            let last_page = listings.len() < 100;
            for listing in listings {
                if listing.draft || listing.prerelease {
                    continue;
                }
                let version = match semver::Version::parse(listing.tag_name.trim_start_matches('v'))
                {
                    Ok(version) if version.pre.is_empty() => version,
                    _ => continue,
                };
                // Releases predating App Update have no feed artifacts.
                let Some(metadata_asset) = listing
                    .assets
                    .iter()
                    .find(|asset| asset.name == "release.json")
                else {
                    continue;
                };
                let Some(updater_asset) = listing
                    .assets
                    .iter()
                    .find(|asset| asset.name == "latest.json")
                else {
                    continue;
                };
                let latest_json_url = self.asset_url(updater_asset)?.to_string();
                let metadata: ReleaseMetadata =
                    self.read_json(self.asset_url(metadata_asset)?).await?;
                let metadata_version = semver::Version::parse(&metadata.version)
                    .map_err(|_| AppError::Internal("invalid Release Feed version".to_owned()))?;
                if metadata_version != version || metadata.schema_version < 0 {
                    return Err(AppError::Internal(
                        "inconsistent Release Feed metadata".to_owned(),
                    ));
                }
                releases.push(Release {
                    version: metadata.version,
                    schema_version: metadata.schema_version,
                    notes: metadata.notes,
                    pub_date: metadata.pub_date,
                    latest_json_url,
                    raises_schema: false,
                });
            }
            if last_page {
                return Ok(releases);
            }
        }
        Err(AppError::Internal(
            "Release Feed pagination limit exceeded".to_owned(),
        ))
    }
}

impl ReleaseFeed for GitHubReleaseFeed {
    fn releases(&self) -> BoxFuture<'_, AppResult<Vec<Release>>> {
        Box::pin(self.fetch())
    }
}

#[derive(Default)]
struct CheckedFeed {
    releases: Vec<Release>,
    last_checked_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

/// Shared by Owner requests and the background check; reading state never
/// waits for an outbound request. A failed check clears stale targets.
#[derive(Default)]
pub struct UpdateService {
    checked: RwLock<CheckedFeed>,
    checking: Mutex<()>,
    installing: Mutex<()>,
    download: Arc<std::sync::RwLock<Option<Download>>>,
}

impl UpdateService {
    /// Called before serving Owner requests, after the current Run Record starts.
    pub async fn startup(
        &self,
        pool: &SqlitePool,
        records: &crate::run_record::RunRecordStore,
    ) -> AppResult<()> {
        if let Some(result) = db::app_update::resolve_restart(pool).await? {
            Self::record_result(records, result);
        }
        Ok(())
    }

    fn record_result(records: &crate::run_record::RunRecordStore, result: UpdateResult) {
        if !result.ok {
            tracing::error!(target: "openpiso_admin_lib::app_update", from = %result.from, target_version = %result.to, "App Update failed; running version is {}", env!("CARGO_PKG_VERSION"));
        }
        if let Err(error) = records.record_app_update_result(result) {
            tracing::warn!(target: "openpiso_admin_lib::app_update", "could not record App Update outcome in Run Record: {error}");
        }
    }

    pub async fn install(
        &self,
        pool: &SqlitePool,
        installer: &dyn Installer,
        records: &crate::run_record::RunRecordStore,
        release: Release,
        rollback: bool,
    ) -> AppResult<()> {
        let _installing = self
            .installing
            .try_lock()
            .map_err(|_| AppError::Conflict("App Update already in progress".to_owned()))?;
        if db::app_update::restart_marker(pool).await?.is_some() {
            return Err(AppError::Conflict(
                "App Update awaiting restart or acknowledgement".to_owned(),
            ));
        }
        let download = self.download.clone();
        let target_version = release.version.clone();
        let progress = Arc::new(move |percent: u8| {
            *download
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Download {
                target_version: target_version.clone(),
                percent: percent.min(100),
            });
        });
        let prepared = installer.prepare(release.clone(), rollback, progress).await;
        if prepared.is_err() {
            Self::record_result(
                records,
                UpdateResult {
                    from: env!("CARGO_PKG_VERSION").to_owned(),
                    to: release.version.clone(),
                    ok: false,
                },
            );
        }
        let result = async {
            let prepared = prepared?;
            let marker = RestartMarker {
                from_version: env!("CARGO_PKG_VERSION").to_owned(),
                target_version: release.version,
                started_at: Utc::now(),
            };
            db::app_update::begin_install(pool, &marker, rollback).await?;
            let result = prepared.install().await;
            if result.is_err() {
                self.startup(pool, records).await?;
            }
            result
        }
        .await;
        *self
            .download
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        result
    }

    pub async fn check(&self, feed: &dyn ReleaseFeed) -> Option<String> {
        let _checking = self.checking.lock().await;
        let result = feed.releases().await;
        let (releases, last_error) = match result {
            Ok(releases) => (releases, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        *self.checked.write().await = CheckedFeed {
            releases,
            last_checked_at: Some(Utc::now()),
            last_error: last_error.clone(),
        };
        last_error
    }

    pub async fn state(
        &self,
        pool: &SqlitePool,
        sessions: &SessionStore,
    ) -> AppResult<UpdateState> {
        let current_version = env!("CARGO_PKG_VERSION");
        let current = semver::Version::parse(current_version)
            .map_err(|error| AppError::Internal(format!("invalid binary version: {error}")))?;
        let schema = db::app_update::schema_version(pool).await?;
        let auto_check = db::app_update::auto_check(pool).await?;
        let pcs = sessions.enrich_pcs(db::pcs(pool).await?).await;
        let checked = self.checked.read().await;
        let mut releases: Vec<_> = checked
            .releases
            .iter()
            .filter_map(|release| {
                let version = semver::Version::parse(&release.version).ok()?;
                (version.pre.is_empty() && release.schema_version >= 0)
                    .then_some((version, release.clone()))
            })
            .collect();
        releases.sort_by(|(a, _), (b, _)| b.cmp_precedence(a));
        let available = releases
            .iter()
            .find(|(version, _)| version.cmp_precedence(&current).is_gt())
            .map(|(_, release)| {
                let mut release = release.clone();
                release.raises_schema = release.schema_version > schema;
                release
            });
        let rollbacks = releases
            .into_iter()
            .filter(|(version, release)| {
                version.cmp_precedence(&current).is_lt() && release.schema_version == schema
            })
            .map(|(_, mut release)| {
                release.raises_schema = false;
                release
            })
            .collect();
        Ok(UpdateState {
            current_version: current_version.to_owned(),
            current_schema_version: schema,
            auto_check,
            last_checked_at: checked.last_checked_at,
            last_error: checked.last_error.clone(),
            available,
            rollbacks,
            last_result: db::app_update::last_result(pool).await?,
            running_session_count: pcs.iter().filter(|pc| pc.remaining_seconds > 0).count(),
            download: self
                .download
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        })
    }
}

/// Startup and six-hour automatic checks run independently of session timers.
pub async fn run(pool: SqlitePool, service: Arc<UpdateService>, feed: Arc<dyn ReleaseFeed>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match db::app_update::auto_check(&pool).await {
            Ok(true) => {
                service.check(feed.as_ref()).await;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::error!(target: "openpiso_admin_lib::app_update", "App Update preference unavailable: {error}")
            }
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::errors::{AppError, AppResult};

    #[derive(Default)]
    pub struct FakeInstaller {
        pub fail_prepare: bool,
        pub fail_install: bool,
        pub download_gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl Installer for FakeInstaller {
        fn prepare(
            &self,
            _release: Release,
            _rollback: bool,
            progress: DownloadProgress,
        ) -> BoxFuture<'_, AppResult<Box<dyn PreparedInstall>>> {
            Box::pin(async move {
                progress(25);
                if let Some(gate) = &self.download_gate {
                    gate.acquire().await.expect("download gate").forget();
                }
                if self.fail_prepare {
                    return Err(AppError::Internal(
                        "signature verification failed".to_owned(),
                    ));
                }
                progress(100);
                Ok(Box::new(FakePreparedInstall(self.fail_install)) as Box<dyn PreparedInstall>)
            })
        }
    }

    struct FakePreparedInstall(bool);

    impl PreparedInstall for FakePreparedInstall {
        fn install(self: Box<Self>) -> BoxFuture<'static, AppResult<()>> {
            Box::pin(async move {
                if self.0 {
                    Err(AppError::Internal("installer launch failed".to_owned()))
                } else {
                    Ok(())
                }
            })
        }
    }

    pub struct FakeReleaseFeed(pub std::sync::Mutex<Result<Vec<Release>, String>>);

    impl Default for FakeReleaseFeed {
        fn default() -> Self {
            Self(std::sync::Mutex::new(Ok(Vec::new())))
        }
    }

    impl ReleaseFeed for FakeReleaseFeed {
        fn releases(&self) -> BoxFuture<'_, AppResult<Vec<Release>>> {
            Box::pin(async move {
                self.0
                    .lock()
                    .expect("fake feed lock")
                    .clone()
                    .map_err(AppError::Internal)
            })
        }
    }
}
