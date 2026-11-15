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

//! gadk-session: sessions, events, and the in-memory session service.

pub mod compaction;
mod error;
mod event;
mod inmemory;
mod legacy;
mod requested_auth;
mod service;
pub mod state_util;
mod tool_confirmation;
pub mod toolauth;

/// Reusable conformance suite for [`SessionService`] implementations.
#[cfg(feature = "testsuite")]
pub mod testsuite;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use error::SessionError;
pub use event::{
    Event, EventActions, EventCompaction, EventRef, NodeInfo, RequestInput, zero_time,
};
pub use inmemory::InMemorySessionService;
pub use service::{
    CreateRequest, DeleteRequest, GetRequest, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
    ListRequest, ReadonlyState, Session, SessionService, State, compare_by_last_update,
};
pub use tool_confirmation::ToolConfirmation;
