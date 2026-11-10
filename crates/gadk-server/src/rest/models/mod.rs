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

//! The REST API's wire DTOs (port of ADK-Go `server/adkrest/internal/models`), with Go's JSON
//! names, field order and `omitempty` rules. Request bodies decode with serde (Go's
//! case-insensitive key matching is not reproduced); a JSON `null` for a non-pointer Go field
//! leaves its zero value, as Go's decoder does.

mod event;
mod runtime;
mod session;
mod triggers;

pub use event::Event;
#[cfg(test)]
pub use event::EventActions;
pub use runtime::{LiveRequest, RunAgentRequest};
pub use session::{
    CreateSessionRequest, Session, SessionId, UpdateSessionRequest, session_id_from_http_parameters,
};
#[cfg(test)]
pub use triggers::PubSubMessage;
pub use triggers::{EventarcTriggerRequest, PubSubTriggerRequest, TriggerResponse};

use serde::{Deserialize, Deserializer};

/// Decodes `null` as `T::default()` (Go leaves a non-pointer field at its zero value on `null`).
pub(crate) fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
