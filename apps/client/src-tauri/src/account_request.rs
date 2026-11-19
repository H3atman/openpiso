//! One Account Session request to the Admin: read the stored session, send
//! the request with its bearer token, and hand back the decoded answer or a
//! typed failure. Callers map [`AccountRequestError`] to their own copy.

use serde::{de::DeserializeOwned, Serialize};

use crate::state::{AccountAuthState, SharedState};

/// Why an Account Session request produced no usable answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AccountRequestError {
    NotLoggedIn,
    Unreachable,
    Rejected {
        status: reqwest::StatusCode,
        body: String,
    },
    Undecodable,
    SessionChanged,
}

const LOG_TARGET: &str = "openpiso_client::account_request";

/// The `body` of a body-less [`post`] or [`post_raw`].
pub(crate) const NO_BODY: Option<&()> = None;

/// `GET /api/<segments…>` with the stored Account Session. Returns the
/// decoded answer and the token the request was sent with, which callers
/// pass to [`replace_session`] or use to guard their own patch.
/// `operation` names the request in the local log.
pub(crate) async fn get<T: DeserializeOwned>(
    shared: &SharedState,
    segments: &[&str],
    operation: &str,
) -> Result<(T, String), AccountRequestError> {
    let (response, token) =
        send(shared, reqwest::Method::GET, segments, NO_BODY, operation).await?;
    Ok((decode(response, operation).await?, token))
}

/// `POST /api/<segments…>` with the stored Account Session and an optional
/// JSON body; a body-less POST passes [`NO_BODY`]. Same answer as [`get`].
pub(crate) async fn post<T: DeserializeOwned, B: Serialize>(
    shared: &SharedState,
    segments: &[&str],
    body: Option<&B>,
    operation: &str,
) -> Result<(T, String), AccountRequestError> {
    let (response, token) = send(shared, reqwest::Method::POST, segments, body, operation).await?;
    Ok((decode(response, operation).await?, token))
}

/// Like [`post`], but the success body comes back as raw bytes, undecoded,
/// for callers that ignore it or tolerate a body they cannot parse. A body
/// that cannot be read comes back empty.
pub(crate) async fn post_raw<B: Serialize>(
    shared: &SharedState,
    segments: &[&str],
    body: Option<&B>,
    operation: &str,
) -> Result<Vec<u8>, AccountRequestError> {
    let (response, _) = send(shared, reqwest::Method::POST, segments, body, operation).await?;
    Ok(response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default())
}

/// Store `auth` as the Account Session, but only if the stored session still
/// carries `sent_with`, the token the request went out with. A logout or a
/// different login during the request wins: nothing is written and the
/// result is [`AccountRequestError::SessionChanged`].
pub(crate) async fn replace_session(
    shared: &SharedState,
    sent_with: &str,
    auth: AccountAuthState,
) -> Result<AccountAuthState, AccountRequestError> {
    let mut account_auth = shared.account_auth.write().await;
    if account_auth
        .as_ref()
        .is_none_or(|current| current.token != sent_with)
    {
        return Err(AccountRequestError::SessionChanged);
    }
    *account_auth = Some(auth.clone());
    Ok(auth)
}

/// Send the request and hand back the successful response with the token it
/// went out with; everything up to the status check.
async fn send<B: Serialize>(
    shared: &SharedState,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<&B>,
    operation: &str,
) -> Result<(reqwest::Response, String), AccountRequestError> {
    let token = shared
        .account_auth
        .read()
        .await
        .as_ref()
        .map(|auth| auth.token.clone())
        .ok_or(AccountRequestError::NotLoggedIn)?;
    let server_url = shared.config.read().await.server_url.clone();
    let url = api_url(&server_url, segments).ok_or_else(|| {
        tracing::error!(target: LOG_TARGET, operation, "server URL cannot carry an API path");
        AccountRequestError::Unreachable
    })?;

    let mut request = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(&token);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().await.map_err(|error| {
        // `{error:?}` keeps the source chain; it also carries the Admin
        // address, which is why it goes to the log and never to the caller.
        tracing::error!(target: LOG_TARGET, operation, "could not reach admin server: {error:?}");
        AccountRequestError::Unreachable
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(AccountRequestError::Rejected {
            status,
            body: response.text().await.unwrap_or_default(),
        });
    }
    Ok((response, token))
}

async fn decode<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: &str,
) -> Result<T, AccountRequestError> {
    response.json::<T>().await.map_err(|error| {
        tracing::error!(target: LOG_TARGET, operation, "Admin returned an undecodable response: {error:?}");
        AccountRequestError::Undecodable
    })
}

/// `<server_url>/api/<segments…>`, each segment percent-encoded so caller
/// input can never alter the path.
fn api_url(server_url: &str, segments: &[&str]) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(server_url).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .push("api")
        .extend(segments);
    Some(url)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{http::HeaderMap, http::StatusCode, routing::get as route_get, Json, Router};

    use super::*;
    use crate::state::{AccountSummary, ActiveSession};

    fn auth(token: &str, remaining_seconds: i64) -> AccountAuthState {
        AccountAuthState {
            token: token.to_owned(),
            account: AccountSummary {
                id: "account-1".to_owned(),
                username: "customer".to_owned(),
                remaining_seconds,
                points_balance_centi: 0,
                status: "active".to_owned(),
            },
            session: ActiveSession {
                id: "session-1".to_owned(),
                account_id: "account-1".to_owned(),
                username: "customer".to_owned(),
                pc_id: "pc-01".to_owned(),
                remaining_seconds,
                points_balance_centi: 0,
                claimable_guest_seconds: 0,
            },
        }
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub admin");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move { axum::serve(listener, router).await });
        url
    }

    async fn shared_for(server_url: &str, token: Option<&str>) -> Arc<SharedState> {
        let shared = Arc::new(SharedState::default());
        shared.config.write().await.server_url = server_url.to_owned();
        *shared.account_auth.write().await = token.map(|token| auth(token, 300));
        shared
    }

    /// A stub that always answers `status` with `body`.
    fn answering(status: StatusCode, body: &'static str) -> Router {
        Router::new().route(
            "/api/thing",
            route_get(move || async move { (status, body) }),
        )
    }

    #[tokio::test]
    async fn not_logged_in_sends_nothing() {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let url = serve(Router::new().route(
            "/api/thing",
            route_get(move || async move {
                counter.fetch_add(1, Ordering::SeqCst);
                "{}"
            }),
        ))
        .await;
        let shared = shared_for(&url, None).await;

        let result = get::<serde_json::Value>(&shared, &["thing"], "test read").await;

        assert_eq!(result.err(), Some(AccountRequestError::NotLoggedIn));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn closed_port_is_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        drop(listener);
        let shared = shared_for(&url, Some("tok")).await;

        let result = get::<serde_json::Value>(&shared, &["thing"], "test read").await;

        assert_eq!(result.err(), Some(AccountRequestError::Unreachable));
    }

    #[tokio::test]
    async fn rejection_carries_status_and_body() {
        let url = serve(answering(StatusCode::CONFLICT, "voucher already used")).await;
        let shared = shared_for(&url, Some("tok")).await;

        let result = get::<serde_json::Value>(&shared, &["thing"], "test read").await;

        assert_eq!(
            result.err(),
            Some(AccountRequestError::Rejected {
                status: reqwest::StatusCode::CONFLICT,
                body: "voucher already used".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn rejection_with_empty_body_carries_empty_body() {
        let url = serve(answering(StatusCode::INTERNAL_SERVER_ERROR, "")).await;
        let shared = shared_for(&url, Some("tok")).await;

        let result = get::<serde_json::Value>(&shared, &["thing"], "test read").await;

        assert_eq!(
            result.err(),
            Some(AccountRequestError::Rejected {
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                body: String::new(),
            })
        );
    }

    #[tokio::test]
    async fn success_with_wrong_shape_is_undecodable() {
        let url = serve(answering(StatusCode::OK, "not json")).await;
        let shared = shared_for(&url, Some("tok")).await;

        let result = get::<AccountAuthState>(&shared, &["thing"], "test read").await;

        assert_eq!(result.err(), Some(AccountRequestError::Undecodable));
    }

    #[tokio::test]
    async fn success_sends_bearer_and_returns_decoded_value_and_token() {
        let url = serve(Router::new().route(
            "/api/thing",
            route_get(|headers: HeaderMap| async move {
                let authorization = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                Json(serde_json::json!({ "authorization": authorization }))
            }),
        ))
        .await;
        let shared = shared_for(&url, Some("tok-1")).await;

        let (value, token) = get::<serde_json::Value>(&shared, &["thing"], "test read")
            .await
            .expect("request succeeds");

        assert_eq!(value["authorization"], "Bearer tok-1");
        assert_eq!(token, "tok-1");
    }

    #[tokio::test]
    async fn body_less_post_sends_no_body() {
        let url = serve(Router::new().route(
            "/api/thing",
            axum::routing::post(|headers: HeaderMap, body: String| async move {
                Json(serde_json::json!({
                    "content_type": headers.contains_key("content-type"),
                    "body": body,
                }))
            }),
        ))
        .await;
        let shared = shared_for(&url, Some("tok")).await;

        let (value, _) = post::<serde_json::Value, _>(&shared, &["thing"], NO_BODY, "test write")
            .await
            .expect("request succeeds");

        assert_eq!(value["content_type"], false);
        assert_eq!(value["body"], "");
    }

    #[tokio::test]
    async fn raw_post_returns_the_success_body_undecoded() {
        let url =
            serve(Router::new().route("/api/thing", axum::routing::post(|| async { "not json" })))
                .await;
        let shared = shared_for(&url, Some("tok")).await;

        let body = post_raw(&shared, &["thing"], NO_BODY, "test write")
            .await
            .expect("an undecodable success is still a success");

        assert_eq!(body, b"not json");
    }

    /// Answers `POST /api/session` with a refreshed session for `tok-1`,
    /// after running `before_reply` (used to simulate a mid-request change).
    fn session_admin(before_reply: impl Fn() + Clone + Send + Sync + 'static) -> Router {
        Router::new().route(
            "/api/session",
            axum::routing::post(move || {
                let before_reply = before_reply.clone();
                async move {
                    before_reply();
                    Json(auth("tok-1", 900))
                }
            }),
        )
    }

    #[tokio::test]
    async fn guarded_replacement_stores_when_session_unchanged() {
        let url = serve(session_admin(|| {})).await;
        let shared = shared_for(&url, Some("tok-1")).await;

        let (fresh, sent_with) = post::<AccountAuthState, _>(
            &shared,
            &["session"],
            Some(&serde_json::json!({ "code": "ABC" })),
            "test write",
        )
        .await
        .expect("request succeeds");
        let stored = replace_session(&shared, &sent_with, fresh)
            .await
            .expect("session unchanged");

        assert_eq!(stored.account.remaining_seconds, 900);
        let current = shared.account_auth.read().await.clone().expect("logged in");
        assert_eq!(current.account.remaining_seconds, 900);
    }

    #[tokio::test]
    async fn guarded_replacement_refuses_when_session_changed_mid_request() {
        let shared = shared_for("http://placeholder", Some("tok-1")).await;
        let during = Arc::clone(&shared);
        let url = serve(session_admin(move || {
            // Another customer logs in while the request is in flight.
            let mut account_auth = during
                .account_auth
                .try_write()
                .expect("uncontended during the request");
            *account_auth = Some(auth("tok-2", 60));
        }))
        .await;
        shared.config.write().await.server_url = url;

        let (fresh, sent_with) =
            post::<AccountAuthState, _>(&shared, &["session"], NO_BODY, "test write")
                .await
                .expect("request succeeds");
        let result = replace_session(&shared, &sent_with, fresh).await;

        assert_eq!(result.err(), Some(AccountRequestError::SessionChanged));
        let current = shared.account_auth.read().await.clone().expect("logged in");
        assert_eq!(current.token, "tok-2");
        assert_eq!(current.account.remaining_seconds, 60);
    }
}
