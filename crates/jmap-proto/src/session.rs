use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Id, State, capability};

/// The JMAP Session resource (RFC 8620 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub capabilities: HashMap<String, Value>,
    pub accounts: HashMap<Id, Account>,
    pub primary_accounts: HashMap<String, Id>,
    pub username: String,
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    pub event_source_url: String,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub name: String,
    pub is_personal: bool,
    pub is_read_only: bool,
    #[serde(default)]
    pub account_capabilities: HashMap<String, Value>,
}

/// Server limits from the `urn:ietf:params:jmap:core` capability (RFC 8620 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreCapability {
    pub max_size_upload: u64,
    pub max_concurrent_upload: u64,
    pub max_size_request: u64,
    pub max_concurrent_requests: u64,
    pub max_calls_in_request: u64,
    pub max_objects_in_get: u64,
    pub max_objects_in_set: u64,
    #[serde(default)]
    pub collation_algorithms: Vec<String>,
}

impl Default for CoreCapability {
    /// Conservative fallbacks for a server that omits values (it shouldn't).
    fn default() -> Self {
        Self {
            max_size_upload: 50_000_000,
            max_concurrent_upload: 4,
            max_size_request: 10_000_000,
            max_concurrent_requests: 4,
            max_calls_in_request: 16,
            max_objects_in_get: 500,
            max_objects_in_set: 500,
            collation_algorithms: Vec::new(),
        }
    }
}

impl Session {
    /// The parsed core capability. Falls back to [`CoreCapability::default`] if missing or malformed.
    pub fn core(&self) -> CoreCapability {
        self.capabilities
            .get(capability::CORE)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    pub fn has_capability(&self, urn: &str) -> bool {
        self.capabilities.contains_key(urn)
    }

    /// Primary account id for a capability (e.g. mail).
    pub fn primary_account(&self, urn: &str) -> Option<&Id> {
        self.primary_accounts.get(urn)
    }

    /// Expand the `downloadUrl` template (RFC 8620 §6.2).
    pub fn download_url_for(&self, account_id: &Id, blob_id: &Id, name: &str, ty: &str) -> String {
        expand(
            &self.download_url,
            &[
                ("accountId", account_id.as_str()),
                ("blobId", blob_id.as_str()),
                ("name", name),
                ("type", ty),
            ],
        )
    }

    /// Expand the `uploadUrl` template (RFC 8620 §6.1).
    pub fn upload_url_for(&self, account_id: &Id) -> String {
        expand(&self.upload_url, &[("accountId", account_id.as_str())])
    }

    /// Expand the `eventSourceUrl` template (RFC 8620 §7.3).
    pub fn event_source_url_for(&self, types: &str, close_after: &str, ping: u32) -> String {
        let ping = ping.to_string();
        expand(
            &self.event_source_url,
            &[
                ("types", types),
                ("closeafter", close_after),
                ("ping", &ping),
            ],
        )
    }
}

/// Minimal RFC 6570 level-1 expansion: `{var}` is replaced by the percent-encoded value.
fn expand(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &rest[start + 1..start + len];
        match vars.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(&percent_encode(v)),
            None => out.push_str(&rest[start..=start + len]),
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_expansion_encodes_values() {
        let t = "https://x/download/{accountId}/{blobId}/{name}?accept={type}";
        let got = expand(
            t,
            &[
                ("accountId", "u1"),
                ("blobId", "B2"),
                ("name", "my file.txt"),
                ("type", "text/plain"),
            ],
        );
        assert_eq!(
            got,
            "https://x/download/u1/B2/my%20file.txt?accept=text%2Fplain"
        );
    }

    #[test]
    fn unknown_vars_are_left_alone() {
        assert_eq!(expand("a{b}c{", &[]), "a{b}c{");
    }
}
