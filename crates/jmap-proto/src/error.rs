use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request-level error: an RFC 7807 problem details object (RFC 8620 §3.6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
}

impl ProblemDetails {
    pub const UNKNOWN_CAPABILITY: &str = "urn:ietf:params:jmap:error:unknownCapability";
    pub const NOT_JSON: &str = "urn:ietf:params:jmap:error:notJSON";
    pub const NOT_REQUEST: &str = "urn:ietf:params:jmap:error:notRequest";
    pub const LIMIT: &str = "urn:ietf:params:jmap:error:limit";
}

impl fmt::Display for ProblemDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(d) = &self.detail {
            write!(f, ": {d}")?;
        }
        if let Some(l) = &self.limit {
            write!(f, " (limit: {l})")?;
        }
        Ok(())
    }
}

/// Method-level error (RFC 8620 §3.6.2, plus method-specific types).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodError {
    #[serde(rename = "type")]
    pub kind: MethodErrorType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl fmt::Display for MethodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind.as_str())?;
        if let Some(d) = &self.description {
            write!(f, ": {d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for MethodError {}

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)*
            /// A value this client does not know.
            Other(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $s,)*
                    Self::Other(s) => s,
                }
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                match s {
                    $($s => Self::$variant,)*
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <String as serde::Deserialize>::deserialize(d)?;
                Ok(Self::from(s.as_str()))
            }
        }
    };
}
pub(crate) use string_enum;

string_enum!(
    /// Method error type names.
    MethodErrorType {
        ServerUnavailable => "serverUnavailable",
        ServerFail => "serverFail",
        ServerPartialFail => "serverPartialFail",
        UnknownMethod => "unknownMethod",
        InvalidArguments => "invalidArguments",
        InvalidResultReference => "invalidResultReference",
        Forbidden => "forbidden",
        AccountNotFound => "accountNotFound",
        AccountNotSupportedByMethod => "accountNotSupportedByMethod",
        AccountReadOnly => "accountReadOnly",
        RequestTooLarge => "requestTooLarge",
        CannotCalculateChanges => "cannotCalculateChanges",
        StateMismatch => "stateMismatch",
        AnchorNotFound => "anchorNotFound",
        UnsupportedSort => "unsupportedSort",
        UnsupportedFilter => "unsupportedFilter",
        TooManyChanges => "tooManyChanges",
        FromAccountNotFound => "fromAccountNotFound",
        FromAccountNotSupportedByMethod => "fromAccountNotSupportedByMethod",
    }
);

/// Per-object error in a `/set` response (RFC 8620 §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetError {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<String>>,
    /// Extra error-specific fields (e.g. `existingId`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(d) = &self.description {
            write!(f, ": {d}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_error_type_round_trips() {
        let e: MethodError =
            serde_json::from_str(r#"{"type":"somethingNew","description":"x"}"#).unwrap();
        assert_eq!(e.kind, MethodErrorType::Other("somethingNew".into()));
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"somethingNew","description":"x"}"#
        );
    }

    #[test]
    fn known_error_type() {
        let e: MethodError = serde_json::from_str(r#"{"type":"cannotCalculateChanges"}"#).unwrap();
        assert_eq!(e.kind, MethodErrorType::CannotCalculateChanges);
    }
}
