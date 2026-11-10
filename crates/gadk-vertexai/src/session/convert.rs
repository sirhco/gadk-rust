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

//! Event ↔ `SessionEvent` conversions (Go `vertexai_client.go`'s `createAiplatformpb*`,
//! `aiplatformTo*`, raw-event and struct helpers). Every error carries Go's text, already
//! wrapped with the prefixes Go's `appendEvent` / `listSessionEvents` add (e.g. `failed to
//! convert event actions: …`).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_genai::{Content, GroundingMetadata};
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value};

use crate::error::BoxError;

/// An event as the `SessionEvent` JSON Go's `appendEvent` sends.
pub(crate) fn event_to_session_event(event: &Event) -> Result<Value, BoxError> {
    todo!()
}

/// A `SessionEvent` JSON as the event Go's `listSessionEvents` builds, including the event ID
/// from the last segment of `name`; `rawEvent` wins when present.
pub(crate) fn session_event_to_event(v: &Value) -> Result<Event, BoxError> {
    todo!()
}

/// Go `eventNeedsRawEvent`.
pub(crate) fn event_needs_raw_event(event: &Event) -> bool {
    todo!()
}

/// Go `eventToRawEvent`.
pub(crate) fn event_to_raw_event(event: &Event) -> Result<Value, BoxError> {
    todo!()
}

/// Go `eventFromRawEvent`.
pub(crate) fn event_from_raw_event(raw: &Value) -> Result<Event, BoxError> {
    todo!()
}

/// Go `toStructPB` on a JSON value: `null` → `{}`; numbers become protobuf doubles.
pub(crate) fn to_struct(value: &Value) -> Result<Value, BoxError> {
    todo!()
}

/// Go `filterNilValues`.
pub(crate) fn filter_nil_values(m: &Map<String, Value>) -> Map<String, Value> {
    todo!()
}

/// Go `createAiplatformpbContent`.
pub(crate) fn content_to_wire(content: &Content) -> Result<Value, BoxError> {
    todo!()
}

/// Go `aiplatformToGenaiContent`.
pub(crate) fn content_from_wire(v: &Value) -> Content {
    todo!()
}

/// Go `createAiplatformpbEventActions` (`None` when both deltas are empty).
pub(crate) fn actions_to_wire(event: &Event) -> Result<Option<Value>, BoxError> {
    todo!()
}

/// Go `aiplatformToSessionEventActions`.
pub(crate) fn actions_from_wire(v: Option<&Value>) -> EventActions {
    todo!()
}

/// Go `createAiplatformpbMetadata`.
pub(crate) fn metadata_to_wire(event: &Event) -> Result<Value, BoxError> {
    todo!()
}

/// Go `createGroundingMetadata`.
pub(crate) fn grounding_from_wire(v: &Value) -> GroundingMetadata {
    todo!()
}
