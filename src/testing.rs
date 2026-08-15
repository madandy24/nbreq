//! Deterministic helpers for downstream contract tests.
//!
//! This module is available to NBReq's own tests and through the opt-in `test-support` feature. It
//! creates only backend-neutral values; no backend implementation type is public.

use crate::{Client, Completion, PendingRequest, RequestHandle, RequestId};

/// Creates an already-terminal waiter for adapter and FFI tests.
#[must_use]
pub fn completed(client: &Client, sequence: u64, completion: Completion) -> PendingRequest {
    let id = RequestId {
        engine: client.shared.id,
        sequence,
    };
    PendingRequest::completed(RequestHandle::new(client.clone(), id), completion)
}
