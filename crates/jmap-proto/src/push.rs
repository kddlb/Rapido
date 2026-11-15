//! Push notifications (RFC 8620 §7).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Id, State};

/// `{"@type": "StateChange", "changed": {accountId: {TypeName: state}}}` (RFC 8620 §7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateChange {
    #[serde(rename = "@type")]
    pub kind: String,
    pub changed: HashMap<Id, HashMap<String, State>>,
}

impl StateChange {
    /// The data types that changed for one account (e.g. `"Email"`, `"Mailbox"`).
    pub fn types_for(&self, account: &Id) -> impl Iterator<Item = (&str, &State)> {
        self.changed
            .get(account)
            .into_iter()
            .flat_map(|m| m.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc_example() {
        let s: StateChange = serde_json::from_str(
            r#"{"@type":"StateChange","changed":{"a456":{"Mailbox":"0af7a512ce70"},"a123":{"Email":"d35ecb040aab","EmailDelivery":"428d565f2440"}}}"#,
        )
        .unwrap();
        let mut types: Vec<_> = s.types_for(&"a123".into()).map(|(t, _)| t).collect();
        types.sort();
        assert_eq!(types, ["Email", "EmailDelivery"]);
        assert_eq!(s.types_for(&"nope".into()).count(), 0);
    }
}
