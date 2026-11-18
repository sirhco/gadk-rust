// Copyright 2026 Chris Olson
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

//! Transcript sources: where production runs come from.

pub mod bigquery;
pub mod mapping;

use chrono::{DateTime, Utc};
use gadk_eval::RunTranscript;
use serde::{Deserialize, Serialize};

use crate::error::MonitorError;

/// Which runs to list: those that started in `[from, to)`, optionally only those whose root agent
/// is `agent`, at most `limit` of them.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptQuery {
    /// The window start (inclusive).
    pub from: DateTime<Utc>,
    /// The window end (exclusive).
    pub to: DateTime<Utc>,
    /// Only runs of this root agent, when set.
    pub agent: Option<String>,
    /// The most runs to return.
    pub limit: usize,
}

/// One production run (an invocation) rebuilt as a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductionRun {
    /// The invocation id.
    pub invocation_id: String,
    /// The session the invocation belongs to.
    pub session_id: String,
    /// The Cloud Trace trace id, when the run recorded one.
    pub trace_id: Option<String>,
    /// The root agent's name.
    pub agent: String,
    /// When the invocation started.
    pub started_at: DateTime<Utc>,
    /// The turns, tool calls and run error rebuilt from the logged events.
    pub transcript: RunTranscript,
    /// `false` when the logged content could not be fully rebuilt (see `notes`); scorers that
    /// read text then report an error instead of grading a partial transcript.
    pub content_complete: bool,
    /// Why the content is incomplete, one note per problem.
    pub notes: Vec<String>,
}

/// A source of production runs.
#[async_trait::async_trait]
pub trait TranscriptSource: Send + Sync {
    /// The runs matching `q`, newest first (by `started_at`), at most `q.limit`.
    async fn list(&self, q: &TranscriptQuery) -> Result<Vec<ProductionRun>, MonitorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_run_round_trips_through_json() {
        let run = ProductionRun {
            invocation_id: "inv-1".into(),
            session_id: "s-1".into(),
            trace_id: None,
            agent: "a".into(),
            started_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            transcript: RunTranscript::default(),
            content_complete: false,
            notes: vec!["no USER_MESSAGE_RECEIVED row".into()],
        };
        let back: ProductionRun =
            serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
        assert_eq!(back, run);
    }
}
