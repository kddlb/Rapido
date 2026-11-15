use jmap_proto::{Id, SetError};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Jmap(#[from] jmap_http::Error),
    #[error("could not encode/decode stored data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("server rejected changes for {} message(s): {}", .0.len(), .0.first().map(|(_, e)| e.to_string()).unwrap_or_default())]
    Rejected(Vec<(Id, SetError)>),
    #[error("account has no mailbox with role {0}")]
    NoRoleMailbox(&'static str),
}

impl Error {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Jmap(e) if e.is_transient())
    }
}
