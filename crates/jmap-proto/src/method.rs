//! Generic standard-method arguments and responses (RFC 8620 §5).

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Comparator, Filter, Id, ResultReference, SetError, State};

/// A typed JMAP method call.
pub trait Method: Serialize {
    /// Method name, e.g. `"Email/get"`.
    const NAME: &'static str;
    /// Capability URN this method needs in `using`.
    const CAPABILITY: &'static str;
    type Response: DeserializeOwned;
}

/// `/set` update patch: JSON-pointer-ish paths to values (RFC 8620 §5.3).
pub type PatchObject = serde_json::Map<String, Value>;

/// `Foo/get` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Get<P> {
    pub account_id: Id,
    /// `None` (with no `#ids`) means "all", which only small types allow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<Id>>,
    #[serde(rename = "#ids", default, skip_serializing_if = "Option::is_none")]
    pub ids_ref: Option<ResultReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<P>>,
}

impl<P> Get<P> {
    pub fn new(account_id: Id) -> Self {
        Self {
            account_id,
            ids: None,
            ids_ref: None,
            properties: None,
        }
    }
    pub fn ids(mut self, ids: Vec<Id>) -> Self {
        self.ids = Some(ids);
        self.ids_ref = None;
        self
    }
    pub fn ids_ref(mut self, r: ResultReference) -> Self {
        self.ids_ref = Some(r);
        self.ids = None;
        self
    }
    pub fn properties(mut self, p: Vec<P>) -> Self {
        self.properties = Some(p);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResponse<T> {
    pub account_id: Id,
    pub state: State,
    pub list: Vec<T>,
    #[serde(default)]
    pub not_found: Vec<Id>,
}

/// `Foo/changes` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    pub account_id: Id,
    pub since_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResponse {
    pub account_id: Id,
    pub old_state: State,
    pub new_state: State,
    pub has_more_changes: bool,
    #[serde(default)]
    pub created: Vec<Id>,
    #[serde(default)]
    pub updated: Vec<Id>,
    #[serde(default)]
    pub destroyed: Vec<Id>,
    /// Only on `Mailbox/changes` (RFC 8621 §2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_properties: Option<Vec<String>>,
}

/// `Foo/set` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Set<T> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_in_state: Option<State>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<HashMap<String, T>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<HashMap<Id, PatchObject>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destroy: Option<Vec<Id>>,
}

impl<T> Set<T> {
    pub fn new(account_id: Id) -> Self {
        Self {
            account_id,
            if_in_state: None,
            create: None,
            update: None,
            destroy: None,
        }
    }
    pub fn update(mut self, id: Id, patch: PatchObject) -> Self {
        self.update
            .get_or_insert_with(HashMap::new)
            .insert(id, patch);
        self
    }
    pub fn destroy(mut self, ids: Vec<Id>) -> Self {
        self.destroy = Some(ids);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetResponse<T> {
    pub account_id: Id,
    #[serde(default)]
    pub old_state: Option<State>,
    pub new_state: State,
    #[serde(default)]
    pub created: Option<HashMap<String, T>>,
    /// Values are `null` or the server-changed properties.
    #[serde(default)]
    pub updated: Option<HashMap<Id, Option<Value>>>,
    #[serde(default)]
    pub destroyed: Option<Vec<Id>>,
    #[serde(default)]
    pub not_created: Option<HashMap<String, SetError>>,
    #[serde(default)]
    pub not_updated: Option<HashMap<Id, SetError>>,
    #[serde(default)]
    pub not_destroyed: Option<HashMap<Id, SetError>>,
}

/// `Foo/query` arguments. `C` is the filter condition, `S` the sort property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query<C, S> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter<C>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Vec<Comparator<S>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calculate_total: Option<bool>,
}

impl<C, S> Query<C, S> {
    pub fn new(account_id: Id) -> Self {
        Self {
            account_id,
            filter: None,
            sort: None,
            position: None,
            anchor: None,
            anchor_offset: None,
            limit: None,
            calculate_total: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponse {
    pub account_id: Id,
    pub query_state: State,
    pub can_calculate_changes: bool,
    pub position: u64,
    pub ids: Vec<Id>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}

/// `Foo/queryChanges` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryChanges<C, S> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter<C>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Vec<Comparator<S>>>,
    pub since_query_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calculate_total: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryChangesResponse {
    pub account_id: Id,
    pub old_query_state: State,
    pub new_query_state: State,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub removed: Vec<Id>,
    #[serde(default)]
    pub added: Vec<AddedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedItem {
    pub id: Id,
    pub index: u64,
}
