// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The BigQuery Storage Write side of the analytics plugin: [`gapic`] (the call layer of the
//! BigQuery Write client), [`arrow_batch`] (Arrow record batches from rows) and
//! [`stream_writer`] (the persistent AppendRows stream writer).
//!
//! This module holds what they share: [`AppendRowsClient`], the client half of an AppendRows
//! stream, an end-of-stream error ([`eof`], [`is_eof`]) and the mapping of tonic statuses onto
//! [`Error::Status`].

pub mod arrow_batch;
pub mod gapic;
pub mod stream_writer;

use std::fmt;

use crate::error::{Code, Error};
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse};

/// The client half of an open AppendRows stream, as the stream writer uses it: `send`, `recv`
/// and `close_send`. Tests substitute their own (a mock stream, stream-wrapping interceptors).
#[async_trait::async_trait]
pub trait AppendRowsClient: Send {
    /// Sends one request. A stream that the server (or the stream's context) has already ended
    /// returns [`eof`], leaving the real status to [`recv`](Self::recv), as gRPC clients do.
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error>;

    /// Receives the next response; [`eof`] once the server ended the stream with OK, the
    /// stream's status ([`Error::Status`]) once it ended otherwise.
    async fn recv(&mut self) -> Result<AppendRowsResponse, Error>;

    /// Signals that no more requests follow (the error gRPC never returns here is not modeled).
    fn close_send(&mut self);
}

/// The end-of-stream error as an [`Error`] (`Display`: `EOF`). Match it with [`is_eof`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eof;

impl fmt::Display for Eof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EOF")
    }
}

impl std::error::Error for Eof {}

/// The end-of-stream error: the server ended the stream with OK.
pub fn eof() -> Error {
    Error::Other(Box::new(Eof))
}

/// Whether [`eof`] occurs anywhere in `err`'s source tree.
pub fn is_eof(err: &Error) -> bool {
    err.find(&mut |e| matches!(e, Error::Other(b) if b.is::<Eof>()))
        .is_some()
}

/// The gRPC [`Code`] of a tonic code (same numbers).
pub fn code_from_tonic(code: tonic::Code) -> Code {
    u32::try_from(code as i32)
        .ok()
        .and_then(Code::from_u32)
        .unwrap_or(Code::Unknown)
}

/// The status error (`rpc error: code = … desc = …`) of a tonic status: its code and message
/// (details and metadata are dropped, as the code and error text never show them).
pub fn status_error(status: &tonic::Status) -> Error {
    Error::Status {
        code: code_from_tonic(status.code()),
        message: status.message().to_owned(),
    }
}

/// The [`Code`] of an in-band `google.rpc.Status.code` (an `AppendRowsResponse.error`): `None`
/// for a number with no known status code, which none of the stream writer's error classes
/// matches.
pub fn code_from_rpc(n: i32) -> Option<Code> {
    u32::try_from(n).ok().and_then(Code::from_u32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::context::ContextError;

    #[test]
    fn eof_is_found_through_wrapping() {
        let err = eof();
        assert_eq!(err.to_string(), "EOF");
        assert!(is_eof(&err));
        let wrapped = Error::wrap("failed to send row batch", eof());
        assert_eq!(wrapped.to_string(), "failed to send row batch: EOF");
        assert!(is_eof(&wrapped));
        assert!(!is_eof(&Error::msg("EOF")));
        assert!(!is_eof(&Error::Context(ContextError::Canceled)));
    }

    #[test]
    fn tonic_statuses_map_to_grpc_go_errors() {
        let err = status_error(&tonic::Status::new(tonic::Code::Unavailable, "down"));
        assert_eq!(err.to_string(), "rpc error: code = Unavailable desc = down");
        assert_eq!(crate::error::status_code(&err), Code::Unavailable);
        assert_eq!(code_from_tonic(tonic::Code::Cancelled), Code::Canceled);
        assert_eq!(
            code_from_tonic(tonic::Code::Unauthenticated),
            Code::Unauthenticated
        );
        assert_eq!(code_from_tonic(tonic::Code::Ok), Code::Ok);
        assert_eq!(code_from_rpc(6), Some(Code::AlreadyExists));
        assert_eq!(code_from_rpc(17), None);
        assert_eq!(code_from_rpc(-1), None);
    }
}
