use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::ser::{Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pricing error: {0}")]
    Pricing(#[from] openpiso_core::pricing::PricingError),
    #[error("password processing error: {0}")]
    Password(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("shop_closed")]
    ShopClosed,
    #[error("too many attempts; try again in {retry_after_secs}s")]
    Throttled { retry_after_secs: i64 },
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("attendance settings changed; current version is {current_version}")]
    AttendanceSettingsConflict { current_version: i64 },
    #[error("unauthorized")]
    AttendanceUnauthorized,
    #[error("invalid input: {0}")]
    AttendanceInvalidInput(String),
    #[error("attendance month not found")]
    AttendanceNotFound,
    #[error("attendance claim denied: {reason}")]
    AttendanceClaimDenied { reason: String },
    #[error(
        "unsupported migration history: {0}; run the matching newer Admin or restore its matching Pre-Migration Backup"
    )]
    UnsupportedMigrationHistory(String),
    #[error("restore rejected: {message}")]
    RestoreRejected {
        gate: String,
        message: String,
        blocking_pcs: Vec<String>,
    },
    #[error("not found: {0}")]
    NotFound(String),
    /// A window operation on the kiosk webview failed (ADR-0056). Desktop-only:
    /// it can never originate from an HTTP request, but `AppError` is the one
    /// error type for both surfaces so it still needs a status mapping.
    #[error("window error: {0}")]
    Window(String),
    /// The Telegram Bot API could not be reached (ADR-0076 / #345). The
    /// message is deliberately static: transport error text may embed the
    /// request URL, which contains the bot token, so nothing from the HTTP
    /// client is ever formatted into this string.
    #[error("telegram unavailable: {0}")]
    TelegramUnavailable(String),
    #[error("telemetry unavailable: {0}")]
    TelemetryUnavailable(String),
}

impl AppError {
    pub(crate) fn for_attendance_route(self) -> Self {
        match self {
            Self::Unauthorized(_) => Self::AttendanceUnauthorized,
            Self::InvalidInput(message) => Self::AttendanceInvalidInput(message),
            Self::NotFound(_) => Self::AttendanceNotFound,
            error => error,
        }
    }
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::InvalidInput(_) | AppError::AttendanceInvalidInput(_) => {
                StatusCode::BAD_REQUEST
            }
            AppError::Unauthorized(_) | AppError::AttendanceUnauthorized => {
                StatusCode::UNAUTHORIZED
            }
            AppError::ShopClosed
            | AppError::Conflict(_)
            | AppError::AttendanceSettingsConflict { .. }
            | AppError::AttendanceClaimDenied { .. }
            | AppError::UnsupportedMigrationHistory(_) => StatusCode::CONFLICT,
            AppError::Throttled { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::RestoreRejected { gate, .. } if gate == "precondition" => {
                StatusCode::CONFLICT
            }
            AppError::RestoreRejected { .. } => StatusCode::BAD_REQUEST,
            AppError::NotFound(_) | AppError::AttendanceNotFound => StatusCode::NOT_FOUND,
            AppError::TelegramUnavailable(_) | AppError::TelemetryUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            AppError::Internal(_)
            | AppError::Io(_)
            | AppError::Database(_)
            | AppError::Pricing(_)
            | AppError::Password(_)
            | AppError::Window(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if let AppError::RestoreRejected {
            gate,
            message,
            blocking_pcs,
        } = self
        {
            return (
                status,
                axum::Json(serde_json::json!({
                    "gate": gate,
                    "error": message,
                    "blocking_pcs": blocking_pcs,
                })),
            )
                .into_response();
        }
        if let AppError::AttendanceSettingsConflict { current_version } = self {
            return (
                status,
                axum::Json(serde_json::json!({
                    "error": "attendance_settings_stale",
                    "current_version": current_version,
                })),
            )
                .into_response();
        }
        if matches!(&self, AppError::AttendanceUnauthorized) {
            return (
                status,
                axum::Json(serde_json::json!({ "error": "unauthorized" })),
            )
                .into_response();
        }
        if let AppError::AttendanceInvalidInput(message) = &self {
            return (
                status,
                axum::Json(serde_json::json!({
                    "error": "invalid_input",
                    "message": message,
                })),
            )
                .into_response();
        }
        if matches!(&self, AppError::AttendanceNotFound) {
            return (
                status,
                axum::Json(serde_json::json!({ "error": "not_found" })),
            )
                .into_response();
        }
        if let AppError::AttendanceClaimDenied { reason } = &self {
            return (
                status,
                axum::Json(serde_json::json!({
                    "error": "attendance_claim_denied",
                    "reason": reason,
                })),
            )
                .into_response();
        }
        let retry_after_secs = match &self {
            AppError::Throttled { retry_after_secs } => Some((*retry_after_secs).max(1)),
            _ => None,
        };
        let mut response = (status, self.to_string()).into_response();
        if let Some(retry_after_secs) = retry_after_secs {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("1")),
            );
        }
        response
    }
}

impl From<AppError> for Response {
    fn from(error: AppError) -> Self {
        error.into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_status_mapping() {
        let cases: Vec<(AppError, StatusCode)> = vec![
            (
                AppError::InvalidInput("bad".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                AppError::Internal("clock date overflow".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::Unauthorized("no".into()),
                StatusCode::UNAUTHORIZED,
            ),
            (AppError::ShopClosed, StatusCode::CONFLICT),
            (
                AppError::AttendanceSettingsConflict { current_version: 2 },
                StatusCode::CONFLICT,
            ),
            (AppError::AttendanceUnauthorized, StatusCode::UNAUTHORIZED),
            (
                AppError::AttendanceInvalidInput("bad".into()),
                StatusCode::BAD_REQUEST,
            ),
            (AppError::AttendanceNotFound, StatusCode::NOT_FOUND),
            (
                AppError::AttendanceClaimDenied {
                    reason: "expired".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                AppError::Throttled {
                    retry_after_secs: 60,
                },
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (AppError::Conflict("dup".into()), StatusCode::CONFLICT),
            (
                AppError::UnsupportedMigrationHistory("newer version".into()),
                StatusCode::CONFLICT,
            ),
            (AppError::NotFound("gone".into()), StatusCode::NOT_FOUND),
            (
                AppError::Database(sqlx::Error::PoolClosed),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::Io(std::io::Error::other("disk")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::Pricing(openpiso_core::pricing::PricingError::InvalidAmount),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::Password("hash fail".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::TelegramUnavailable("offline".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                AppError::TelemetryUnavailable("offline".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];

        for (error, expected_status) in cases {
            let response = error.into_response();
            assert_eq!(response.status(), expected_status);
        }
    }

    #[tokio::test]
    async fn attendance_claim_denial_response_includes_reason() {
        use http_body_util::BodyExt;

        let response = AppError::AttendanceClaimDenied {
            reason: "expired".to_owned(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("claim denial body should be JSON");
        assert_eq!(
            body,
            serde_json::json!({
                "error": "attendance_claim_denied",
                "reason": "expired"
            })
        );
    }

    #[tokio::test]
    async fn body_matches_error_display_string() {
        use bytes::Bytes;
        use http_body_util::BodyExt;

        let error = AppError::InvalidInput("bad input".into());
        let expected_body = error.to_string();
        let response = error.into_response();
        let (_, body) = response.into_parts();
        let collected = body.collect().await.expect("body should collect");
        let body_bytes: Bytes = collected.to_bytes();
        assert_eq!(
            std::str::from_utf8(&body_bytes).expect("valid utf-8"),
            expected_body
        );
    }
}
