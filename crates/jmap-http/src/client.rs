use std::sync::{Arc, RwLock};

use bytes::Bytes;
use jmap_proto::{Id, ProblemDetails, Request, Response, Session};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{RequestBuilder, StatusCode};

use crate::{Auth, Batch, Error, EventStream, Result};

/// Fastmail's JMAP session endpoint.
pub const FASTMAIL_SESSION_URL: &str = "https://api.fastmail.com/jmap/session";

/// A connected JMAP client. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct JmapClient {
    http: reqwest::Client,
    auth: Arc<dyn Auth>,
    session_url: String,
    session: RwLock<Arc<Session>>,
}

impl JmapClient {
    /// Fetch the session and return a ready client.
    pub async fn connect(session_url: impl Into<String>, auth: Arc<dyn Auth>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Rapido/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Self::connect_with(http, session_url, auth).await
    }

    pub async fn connect_with(
        http: reqwest::Client,
        session_url: impl Into<String>,
        auth: Arc<dyn Auth>,
    ) -> Result<Self> {
        let session_url = session_url.into();
        let session = fetch_session(&http, &*auth, &session_url).await?;
        Ok(Self {
            http,
            auth,
            session_url,
            session: RwLock::new(Arc::new(session)),
        })
    }

    pub fn session(&self) -> Arc<Session> {
        self.session.read().expect("session lock poisoned").clone()
    }

    /// Re-fetch the session resource.
    pub async fn refresh_session(&self) -> Result<Arc<Session>> {
        let s = Arc::new(fetch_session(&self.http, &*self.auth, &self.session_url).await?);
        *self.session.write().expect("session lock poisoned") = s.clone();
        Ok(s)
    }

    /// Start a batch of method calls.
    pub fn batch(&self) -> Batch<'_> {
        Batch::new(self)
    }

    /// Send a raw request to the API endpoint.
    pub async fn execute(&self, request: &Request) -> Result<Response> {
        let api_url = self.session().api_url.clone();
        let body = serde_json::to_vec(request)?;
        let resp = self
            .send_authed(|| {
                self.http
                    .post(&api_url)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .body(body.clone())
            })
            .await?;
        let resp: Response = serde_json::from_slice(&resp.bytes().await?)?;
        if resp.session_state != self.session().state {
            tracing::debug!("session state changed; refreshing session");
            if let Err(e) = self.refresh_session().await {
                tracing::warn!("session refresh failed: {e}");
            }
        }
        Ok(resp)
    }

    /// Download a blob (RFC 8620 §6.2).
    pub async fn download(&self, account: &Id, blob: &Id, name: &str, ty: &str) -> Result<Bytes> {
        let url = self.session().download_url_for(account, blob, name, ty);
        let resp = self.send_authed(|| self.http.get(&url)).await?;
        Ok(resp.bytes().await?)
    }

    /// Open an EventSource push connection (RFC 8620 §7.3) for all types.
    pub async fn event_source(&self, ping_secs: u32) -> Result<EventStream> {
        let url = self.session().event_source_url_for("*", "no", ping_secs);
        let resp = self
            .send_authed(|| self.http.get(&url).header(ACCEPT, "text/event-stream"))
            .await?;
        Ok(EventStream::new(resp))
    }

    /// Send with auth; on 401 ask the auth provider to refresh and retry once.
    async fn send_authed(&self, build: impl Fn() -> RequestBuilder) -> Result<reqwest::Response> {
        let mut retried = false;
        loop {
            let resp = build()
                .header(AUTHORIZATION, self.auth.authorization()?)
                .send()
                .await?;
            if resp.status() == StatusCode::UNAUTHORIZED {
                if !retried && self.auth.refresh().await? {
                    retried = true;
                    continue;
                }
                return Err(Error::Unauthorized);
            }
            return check_status(resp).await;
        }
    }
}

async fn fetch_session(http: &reqwest::Client, auth: &dyn Auth, url: &str) -> Result<Session> {
    let resp = http
        .get(url)
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, auth.authorization()?)
        .send()
        .await?;
    if resp.status() == StatusCode::UNAUTHORIZED {
        return Err(Error::Unauthorized);
    }
    let resp = check_status(resp).await?;
    Ok(serde_json::from_slice(&resp.bytes().await?)?)
}

async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    if let Ok(problem) = serde_json::from_str::<ProblemDetails>(&body) {
        return Err(Error::Problem(problem));
    }
    Err(Error::Status {
        status: status.as_u16(),
        body: body.chars().take(500).collect(),
    })
}
