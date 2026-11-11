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

//! A runner that yields a scripted event sequence (plan 5b D7), for the executor's tests.

use gadk_agent::AgentError;
#[cfg(feature = "server")]
use gadk_agent::{EventStream, RunConfig};
use gadk_genai::Content;
use gadk_session::Event;

/// Implements [`crate::server::Runner`] (feature `server`): the first run yields the scripted
/// events in order (later runs yield none, as `AgentError` cannot be cloned) and every run is
/// recorded.
pub struct ScriptedRunner {
    // Read only by the `server::Runner` impl.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    events: parking_lot::Mutex<Vec<Result<Event, AgentError>>>,
    calls: parking_lot::Mutex<Vec<(String, String, Content)>>,
}

impl ScriptedRunner {
    /// A runner yielding `events`.
    pub fn new(events: Vec<Result<Event, AgentError>>) -> Self {
        Self {
            events: parking_lot::Mutex::new(events),
            calls: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// The `(user_id, session_id, msg)` of every run so far.
    pub fn calls(&self) -> Vec<(String, String, Content)> {
        self.calls.lock().clone()
    }
}

#[cfg(feature = "server")]
impl crate::server::Runner for ScriptedRunner {
    fn run(&self, user_id: &str, session_id: &str, msg: Content, _cfg: RunConfig) -> EventStream {
        self.calls
            .lock()
            .push((user_id.to_owned(), session_id.to_owned(), msg));
        let events = std::mem::take(&mut *self.events.lock());
        Box::pin(futures::stream::iter(events))
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use futures::StreamExt;
    use gadk_genai::{Part, ROLE_USER};

    use super::*;
    use crate::server::Runner;

    #[tokio::test]
    async fn scripted_runner_yields_its_events_once_and_records_every_call() {
        let event = Event {
            author: "agent".into(),
            ..Default::default()
        };
        let runner = ScriptedRunner::new(vec![
            Ok(event.clone()),
            Err(AgentError::Config("boom".into())),
        ]);
        let msg = Content {
            parts: vec![Part {
                text: "hi".into(),
                ..Default::default()
            }],
            role: ROLE_USER.into(),
        };

        let got: Vec<_> = runner
            .run("u", "s", msg.clone(), RunConfig::default())
            .collect()
            .await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].as_ref().unwrap(), &event);
        assert_eq!(got[1].as_ref().unwrap_err().to_string(), "boom");

        let again: Vec<_> = runner
            .run("u2", "s2", Content::default(), RunConfig::default())
            .collect()
            .await;
        assert!(again.is_empty());
        assert_eq!(
            runner.calls(),
            vec![
                ("u".into(), "s".into(), msg),
                ("u2".into(), "s2".into(), Content::default()),
            ]
        );
    }
}
