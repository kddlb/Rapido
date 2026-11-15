//! JMAP protocol types: RFC 8620 (core) and RFC 8621 (mail).
//!
//! This crate has no IO. It describes what goes over the wire; the transport
//! lives in `jmap-http`.

pub mod error;
pub mod filter;
pub mod id;
pub mod mail;
pub mod method;
pub mod push;
pub mod request;
pub mod session;

pub use error::{MethodError, MethodErrorType, ProblemDetails, SetError};
pub use filter::{Comparator, Filter, FilterOperator, Operator};
pub use id::{Id, State};
pub use method::{
    AddedItem, Changes, ChangesResponse, Get, GetResponse, Method, PatchObject, Query,
    QueryChanges, QueryChangesResponse, QueryResponse, Set, SetResponse,
};
pub use push::StateChange;
pub use request::{Invocation, Request, Response, ResultReference};
pub use session::{Account, CoreCapability, Session};

/// Capability URNs.
pub mod capability {
    pub const CORE: &str = "urn:ietf:params:jmap:core";
    pub const MAIL: &str = "urn:ietf:params:jmap:mail";
    pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
}
