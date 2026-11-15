use jmap_proto::{MethodError, ProblemDetails};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("authentication failed (HTTP 401)")]
    Unauthorized,
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("server rejected the request: {0}")]
    Problem(ProblemDetails),
    #[error("unexpected HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("{method} ({call_id}) failed: {error}")]
    Method {
        call_id: String,
        method: &'static str,
        error: MethodError,
    },
    #[error("could not decode response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("no response for call {0}")]
    MissingResponse(String),
    #[error("request has {calls} calls but the server allows {max}")]
    TooManyCalls { calls: usize, max: u64 },
    #[error("server does not support {0}")]
    MissingCapability(String),
}

impl Error {
    /// The method error, if this is one.
    pub fn method_error(&self) -> Option<&MethodError> {
        match self {
            Self::Method { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Whether retrying later might succeed (network trouble, 5xx, server busy).
    pub fn is_transient(&self) -> bool {
        use jmap_proto::MethodErrorType as T;
        match self {
            Self::Http(e) => e.is_connect() || e.is_timeout() || e.is_request() || e.is_body(),
            Self::Status { status, .. } => *status >= 500 || *status == 429,
            Self::Method { error, .. } => {
                matches!(error.kind, T::ServerUnavailable | T::ServerFail)
            }
            _ => false,
        }
    }
}
