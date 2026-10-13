use futures_util::future::BoxFuture;
use semver::Version;
use tauri::Manager;
use tauri_plugin_updater::UpdaterExt;

use super::{DownloadProgress, Installer, PreparedInstall, Release};
use crate::errors::{AppError, AppResult};

pub struct TauriInstaller {
    app: tauri::AppHandle,
}

impl TauriInstaller {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl Installer for TauriInstaller {
    fn prepare(
        &self,
        release: Release,
        rollback: bool,
        progress: DownloadProgress,
    ) -> BoxFuture<'_, AppResult<Box<dyn PreparedInstall>>> {
        Box::pin(async move {
            // Only Windows NSIS installers are published (ADR-0086).
            if !cfg!(windows) {
                return Err(AppError::Internal(
                    "App Update installation is supported only on Windows".to_owned(),
                ));
            }
            let selected = Version::parse(&release.version)
                .map_err(|_| AppError::Internal("invalid App Update target version".to_owned()))?;
            let endpoint = release
                .latest_json_url
                .parse()
                .map_err(|_| AppError::Internal("invalid App Update manifest URL".to_owned()))?;
            let app = self.app.clone();
            let updater = self
                .app
                .updater_builder()
                .endpoints(vec![endpoint])
                .map_err(|error| AppError::Internal(format!("App Update endpoint: {error}")))?
                .version_comparator(move |current, remote| {
                    accepts_release(&current, &remote.version, &selected, rollback)
                })
                .timeout(std::time::Duration::from_secs(300))
                .on_before_exit(move || {
                    // The plugin exits Windows directly, bypassing RunEvent::Exit.
                    crate::complete_current_run(
                        &app,
                        crate::run_record::ExitReason::ApplicationExit,
                    );
                    app.cleanup_before_exit();
                })
                .build()
                .map_err(|error| AppError::Internal(format!("App Update updater: {error}")))?;
            progress(0);
            let update = updater
                .check()
                .await
                .map_err(|error| AppError::Internal(format!("App Update manifest: {error}")))?
                .ok_or_else(|| {
                    AppError::Internal(
                        "App Update manifest does not match selected release".to_owned(),
                    )
                })?;
            let mut downloaded = 0_u128;
            let bytes = update
                .download(
                    |chunk, total| {
                        downloaded += chunk as u128;
                        if let Some(total) = total.filter(|total| *total > 0) {
                            progress((downloaded * 100 / u128::from(total)).min(99) as u8);
                        }
                    },
                    || {},
                )
                .await
                .map_err(|error| {
                    AppError::Internal(format!("App Update download/verification: {error}"))
                })?;
            // download() returns only after minisign verification, unlike its
            // on_download_finish callback. Never mark an unverified file ready.
            progress(100);
            Ok(Box::new(TauriPreparedInstall {
                app: self.app.clone(),
                update,
                bytes,
            }) as Box<dyn PreparedInstall>)
        })
    }
}

struct TauriPreparedInstall {
    app: tauri::AppHandle,
    update: tauri_plugin_updater::Update,
    bytes: Vec<u8>,
}

impl PreparedInstall for TauriPreparedInstall {
    fn install(self: Box<Self>) -> BoxFuture<'static, AppResult<()>> {
        Box::pin(async move {
            // UpdateService has already committed the restart marker and audit.
            let kiosk = self.app.get_webview_window("kiosk").ok_or_else(|| {
                AppError::Internal("App Update kiosk window unavailable".to_owned())
            })?;
            kiosk
                .hide()
                .map_err(|error| AppError::Internal(format!("App Update hide kiosk: {error}")))?;
            // NSIS passive supplies /P and /R: progress only, close and relaunch.
            if let Err(error) = self.update.install(&self.bytes) {
                if let Err(show_error) = kiosk.show() {
                    tracing::warn!("App Update could not restore kiosk: {show_error}");
                }
                return Err(AppError::Internal(format!("App Update install: {error}")));
            }
            Ok(())
        })
    }
}

fn accepts_release(
    current: &Version,
    remote: &Version,
    selected: &Version,
    rollback: bool,
) -> bool {
    remote == selected
        && if rollback {
            remote < current
        } else {
            remote > current
        }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    #[test]
    fn comparator_installs_only_the_selected_release_in_the_authorized_direction() {
        let current = Version::parse("0.12.0").unwrap();
        let newer = Version::parse("0.13.0").unwrap();
        let older = Version::parse("0.11.5").unwrap();
        let unrelated = Version::parse("0.10.0").unwrap();

        assert!(super::accepts_release(&current, &newer, &newer, false));
        assert!(super::accepts_release(&current, &older, &older, true));
        assert!(!super::accepts_release(&current, &older, &older, false));
        assert!(!super::accepts_release(&current, &newer, &newer, true));
        assert!(!super::accepts_release(&current, &current, &current, false));
        assert!(!super::accepts_release(&current, &current, &current, true));
        assert!(!super::accepts_release(&current, &newer, &older, true));
        assert!(!super::accepts_release(&current, &unrelated, &older, true));
        assert!(!super::accepts_release(&current, &older, &newer, false));
    }
}
