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

//! Go `internal/configurable/conformance/replayplugin/invocation_replay_state.go`: the
//! per-invocation replay state, isolating concurrent runs. Go's `mu` + `cond` are a
//! `parking_lot` `Mutex` + `Condvar` (waited on in `tokio::task::spawn_blocking`); the fields
//! Go guards with `mu` are [`ReplayInner`].

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;

use parking_lot::{Condvar, Mutex};

use crate::conformance::recording::Recordings;

/// Go `invocationReplayState`.
pub(crate) struct InvocationReplayState {
    /// Go `testCasePath`.
    pub(crate) test_case_path: String,
    /// Go `userMessageIndex`.
    pub(crate) user_message_index: i64,
    /// Go `recordings` (with each recording's `index` assigned at load).
    pub(crate) recordings: Recordings,
    /// Go `mu` and the fields it guards.
    pub(crate) mu: Mutex<ReplayInner>,
    /// Go `cond` (on `mu`).
    pub(crate) cond: Condvar,
}

/// The fields of Go's `invocationReplayState` guarded by `mu`.
#[derive(Default)]
pub(crate) struct ReplayInner {
    /// Go `agentReplayIndices`: agent name → its next replay index.
    pub(crate) agent_replay_indices: HashMap<String, i64>,
    /// Go `consumedRecordings`: indices into `recordings.recordings`.
    pub(crate) consumed_recordings: HashMap<usize, bool>,
    /// Go `curIndex`.
    pub(crate) cur_index: i64,
}

impl InvocationReplayState {
    /// Go `newInvocationReplayState`.
    pub(crate) fn new(test_case_path: String, user_message_index: i64, recs: Recordings) -> Self {
        // Go: invocation_replay_state.go newInvocationReplayState
        todo!("Task 10")
    }

    /// Go `GetTestCasePath`.
    pub(crate) fn get_test_case_path(&self) -> &str {
        // Go: invocation_replay_state.go GetTestCasePath
        todo!("Task 10")
    }

    /// Go `GetUserMessageIndex`.
    pub(crate) fn get_user_message_index(&self) -> i64 {
        // Go: invocation_replay_state.go GetUserMessageIndex
        todo!("Task 10")
    }

    /// Go `GetRecordings`.
    pub(crate) fn get_recordings(&self) -> &Recordings {
        // Go: invocation_replay_state.go GetRecordings
        todo!("Task 10")
    }

    /// Go `GetAgentReplayIndex`: `(index, found)`.
    pub(crate) fn get_agent_replay_index(&self, agent_name: &str) -> (i64, bool) {
        // Go: invocation_replay_state.go GetAgentReplayIndex
        todo!("Task 10")
    }
}
