use std::collections::HashMap;

use serde::de::{self, DeserializeOwned, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{Id, MethodError, State};

/// A JMAP API request (RFC 8620 §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub using: Vec<String>,
    pub method_calls: Vec<Invocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<HashMap<Id, Id>>,
}

/// A JMAP API response (RFC 8620 §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub method_responses: Vec<Invocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<HashMap<Id, Id>>,
    pub session_state: State,
}

/// `[name, arguments, callId]` (RFC 8620 §3.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub name: String,
    pub arguments: Value,
    pub call_id: String,
}

impl Invocation {
    pub fn new(name: impl Into<String>, arguments: Value, call_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            arguments,
            call_id: call_id.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.name == "error"
    }

    /// Interpret this response invocation: a method error or the typed result.
    pub fn parse<R: DeserializeOwned>(&self) -> Result<R, InvocationError> {
        if self.is_error() {
            let err: MethodError =
                serde_json::from_value(self.arguments.clone()).map_err(InvocationError::Decode)?;
            return Err(InvocationError::Method(err));
        }
        serde_json::from_value(self.arguments.clone()).map_err(InvocationError::Decode)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvocationError {
    #[error("method error: {0}")]
    Method(MethodError),
    #[error("could not decode method response: {0}")]
    Decode(#[source] serde_json::Error),
}

impl Serialize for Invocation {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(3)?;
        t.serialize_element(&self.name)?;
        t.serialize_element(&self.arguments)?;
        t.serialize_element(&self.call_id)?;
        t.end()
    }
}

impl<'de> Deserialize<'de> for Invocation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Invocation;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a [name, arguments, callId] array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Invocation, A::Error> {
                let name = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let arguments = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let call_id = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(2, &self))?;
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(4, &self));
                }
                Ok(Invocation {
                    name,
                    arguments,
                    call_id,
                })
            }
        }
        d.deserialize_seq(V)
    }
}

/// A back-reference to a previous call's result (RFC 8620 §3.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultReference {
    pub result_of: String,
    pub name: String,
    pub path: String,
}

impl ResultReference {
    pub fn new(
        result_of: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            result_of: result_of.into(),
            name: name.into(),
            path: path.into(),
        }
    }
}
