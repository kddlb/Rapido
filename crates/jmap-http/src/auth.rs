use std::fmt;
use std::future::Future;
use std::pin::Pin;

use reqwest::header::HeaderValue;

use crate::{Error, Result};

pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

/// Supplies the `Authorization` header. Implementations: [`TokenAuth`] now,
/// OAuth (refreshable) later.
pub trait Auth: Send + Sync + fmt::Debug {
    /// The current `Authorization` header value.
    fn authorization(&self) -> Result<HeaderValue>;

    /// Called after a 401. Return `Ok(true)` if credentials changed and the
    /// request should be retried once.
    fn refresh(&self) -> AuthFuture<'_> {
        Box::pin(async { Ok(false) })
    }
}

/// A static bearer token, e.g. a Fastmail API token.
#[derive(Clone)]
pub struct TokenAuth {
    token: String,
}

impl TokenAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl fmt::Debug for TokenAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenAuth(<redacted>)")
    }
}

impl Auth for TokenAuth {
    fn authorization(&self) -> Result<HeaderValue> {
        let mut v = HeaderValue::from_str(&format!("Bearer {}", self.token.trim()))
            .map_err(|_| Error::Auth("token contains invalid characters".into()))?;
        v.set_sensitive(true);
        Ok(v)
    }
}
