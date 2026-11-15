use std::fmt;

use serde::{Deserialize, Serialize};

/// A JMAP object id (RFC 8620 §1.2): 1–255 URL-safe base64 characters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub String);

/// An opaque state string (RFC 8620 §1.6.4 style; used by `/get`, `/changes`, `/query`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State(pub String);

macro_rules! string_newtype {
    ($t:ty) => {
        impl $t {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $t {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
        impl From<String> for $t {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl AsRef<str> for $t {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_newtype!(Id);
string_newtype!(State);

impl Id {
    /// Whether this is a syntactically valid JMAP id (RFC 8620 §1.2).
    pub fn is_valid(&self) -> bool {
        let len = self.0.len();
        (1..=255).contains(&len)
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validity() {
        assert!(Id::from("Mf8a6c5f1-_x").is_valid());
        assert!(!Id::from("").is_valid());
        assert!(!Id::from("a/b").is_valid());
        assert!(!Id::from("x".repeat(256)).is_valid());
    }
}
