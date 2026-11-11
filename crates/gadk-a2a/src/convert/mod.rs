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

//! ADK ↔ A2A conversions shared by the server and the remote agent (port of ADK-Go
//! `server/adka2a/v2` `parts.go`, `events.go`, `metadata.go`, `extension.go` and the
//! `WithoutPartialArtifacts` helper of `utils.go`). Pure functions; no I/O.

mod events;
mod extension;
mod metadata;
mod parts;

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_genai::Part;
use gadk_session::Event;

pub use events::*;
pub use extension::*;
pub use metadata::*;
pub use parts::*;

use crate::Error;

/// Go `A2APartConverter`: converts one A2A part to a genai part; `Ok(None)` drops the part (Go's
/// `nil` return).
pub type A2APartConverter = Arc<
    dyn Fn(StreamResponseRef<'_>, &a2a::Part) -> BoxFuture<'static, Result<Option<Part>, Error>>
        + Send
        + Sync,
>;

/// Go `GenAIPartConverter`: converts one genai part (of `event`, when there is one) to an A2A
/// part; `Ok(None)` drops the part (Go's `nil` return).
pub type GenAIPartConverter = Arc<
    dyn Fn(Option<&Event>, &Part) -> BoxFuture<'static, Result<Option<a2a::Part>, Error>>
        + Send
        + Sync,
>;

/// The A2A event a part came from (Go passes the `a2a.Event`); `None` for a bare message
/// conversion.
#[derive(Debug, Clone, Copy)]
pub enum StreamResponseRef<'a> {
    /// No source event.
    None,
    /// A streamed or returned A2A event.
    Event(&'a a2a::StreamResponse),
    /// A bare A2A message.
    Message(&'a a2a::Message),
}
