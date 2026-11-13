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
//! `std::sync` `Mutex` + `Condvar` (waited on inside `tokio::task::spawn_blocking`, so the
//! runtime's worker threads never block); the fields Go guards with `mu` are [`ReplayInner`].

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use crate::conformance::recording::Recordings;

/// Go `invocationReplayState`.
pub(crate) struct InvocationReplayState {
    /// Go `testCasePath` (read only by Go's unused `GetTestCasePath`).
    #[allow(dead_code)]
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
        InvocationReplayState {
            test_case_path,
            user_message_index,
            recordings: recs,
            mu: Mutex::new(ReplayInner::default()),
            cond: Condvar::new(),
        }
    }

    /// Go `GetTestCasePath`. Unused by Go's plugin too.
    #[allow(dead_code)]
    pub(crate) fn get_test_case_path(&self) -> &str {
        // Go: invocation_replay_state.go GetTestCasePath
        &self.test_case_path
    }

    /// Go `GetUserMessageIndex`. Unused by Go's plugin too.
    #[allow(dead_code)]
    pub(crate) fn get_user_message_index(&self) -> i64 {
        // Go: invocation_replay_state.go GetUserMessageIndex
        self.user_message_index
    }

    /// Go `GetRecordings`. Unused by Go's plugin too.
    #[allow(dead_code)]
    pub(crate) fn get_recordings(&self) -> &Recordings {
        // Go: invocation_replay_state.go GetRecordings
        &self.recordings
    }

    /// Go `GetAgentReplayIndex`: `(index, found)`.
    pub(crate) fn get_agent_replay_index(&self, agent_name: &str) -> (i64, bool) {
        // Go: invocation_replay_state.go GetAgentReplayIndex
        let inner = self.lock();
        match inner.agent_replay_indices.get(agent_name) {
            Some(v) => (*v, true),
            None => (0, false),
        }
    }

    /// Go `s.mu.Lock()`. A panic while holding the lock does not poison the state for the other
    /// agents (Go's `sync.Mutex` has no poisoning).
    pub(crate) fn lock(&self) -> MutexGuard<'_, ReplayInner> {
        self.mu.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::conformance::recording::Recording;

    #[test]
    fn new_state_starts_at_zero_with_no_indices() {
        let recs = Recordings {
            recordings: vec![Recording {
                agent_name: "a".into(),
                ..Default::default()
            }],
        };
        let s = InvocationReplayState::new("case".into(), 2, recs.clone());
        assert_eq!(s.get_test_case_path(), "case");
        assert_eq!(s.get_user_message_index(), 2);
        assert_eq!(s.get_recordings(), &recs);
        let inner = s.lock();
        assert_eq!(inner.cur_index, 0);
        assert!(inner.agent_replay_indices.is_empty());
        assert!(inner.consumed_recordings.is_empty());
    }

    #[test]
    fn get_agent_replay_index_reports_presence() {
        let s = InvocationReplayState::new(String::new(), 0, Recordings::default());
        assert_eq!(s.get_agent_replay_index("a"), (0, false));
        s.lock().agent_replay_indices.insert("a".into(), 0);
        assert_eq!(s.get_agent_replay_index("a"), (0, true));
        *s.lock().agent_replay_indices.get_mut("a").unwrap() += 3;
        assert_eq!(s.get_agent_replay_index("a"), (3, true));
    }
}
