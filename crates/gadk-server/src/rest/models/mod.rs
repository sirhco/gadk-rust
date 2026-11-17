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

//! The REST API's wire DTOs, with the JSON names, field order and omit-when-empty rules ADK REST
//! clients expect. Request bodies decode with serde (keys match case-sensitively); a JSON `null`
//! for a non-optional field leaves its default value.

mod event;
mod redact;
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

/// Decodes `null` as `T::default()`, so a `null` leaves a non-optional field at its default.
pub(crate) fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
