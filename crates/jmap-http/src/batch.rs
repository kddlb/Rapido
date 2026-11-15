use std::marker::PhantomData;

use jmap_proto::request::InvocationError;
use jmap_proto::{Invocation, Method, Request, ResultReference, State, capability};

use crate::{Error, JmapClient, Result};

/// A set of method calls sent in one HTTP request. Calls can reference earlier
/// calls' results via [`CallHandle::result_ref`].
pub struct Batch<'c> {
    client: &'c JmapClient,
    using: Vec<String>,
    calls: Vec<Invocation>,
}

/// Typed handle to one call in a [`Batch`].
#[derive(Debug)]
pub struct CallHandle<M> {
    call_id: String,
    _m: PhantomData<fn() -> M>,
}

impl<M> Clone for CallHandle<M> {
    fn clone(&self) -> Self {
        Self {
            call_id: self.call_id.clone(),
            _m: PhantomData,
        }
    }
}

impl<M: Method> CallHandle<M> {
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// A back-reference to this call's result at `path` (e.g. `"/ids"`).
    pub fn result_ref(&self, path: &str) -> ResultReference {
        ResultReference::new(self.call_id.clone(), M::NAME, path)
    }
}

impl<'c> Batch<'c> {
    pub(crate) fn new(client: &'c JmapClient) -> Self {
        Self {
            client,
            using: vec![capability::CORE.to_owned()],
            calls: Vec::new(),
        }
    }

    /// Add a call; its capability is added to `using` automatically.
    pub fn call<M: Method>(&mut self, method: M) -> Result<CallHandle<M>> {
        let call_id = format!("c{}", self.calls.len());
        let args = serde_json::to_value(&method)?;
        if !self.using.iter().any(|u| u == M::CAPABILITY) {
            self.using.push(M::CAPABILITY.to_owned());
        }
        self.calls
            .push(Invocation::new(M::NAME, args, call_id.clone()));
        Ok(CallHandle {
            call_id,
            _m: PhantomData,
        })
    }

    pub fn len(&self) -> usize {
        self.calls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    pub fn into_request(self) -> Request {
        Request {
            using: self.using,
            method_calls: self.calls,
            created_ids: None,
        }
    }

    pub async fn send(self) -> Result<BatchResponse> {
        let client = self.client;
        let max = client.session().core().max_calls_in_request;
        if self.calls.len() as u64 > max {
            return Err(Error::TooManyCalls {
                calls: self.calls.len(),
                max,
            });
        }
        let resp = client.execute(&self.into_request()).await?;
        Ok(BatchResponse {
            invocations: resp.method_responses,
            session_state: resp.session_state,
        })
    }
}

/// The responses to a [`Batch`].
#[derive(Debug, Clone)]
pub struct BatchResponse {
    pub invocations: Vec<Invocation>,
    pub session_state: State,
}

impl BatchResponse {
    /// The typed response for a call, or the method error it produced.
    pub fn get<M: Method>(&self, handle: &CallHandle<M>) -> Result<M::Response> {
        let inv = self
            .invocations
            .iter()
            .find(|i| i.call_id == handle.call_id && (i.name == M::NAME || i.is_error()))
            .ok_or_else(|| Error::MissingResponse(handle.call_id.clone()))?;
        inv.parse().map_err(|e| match e {
            InvocationError::Method(error) => Error::Method {
                call_id: handle.call_id.clone(),
                method: M::NAME,
                error,
            },
            InvocationError::Decode(e) => Error::Decode(e),
        })
    }
}
