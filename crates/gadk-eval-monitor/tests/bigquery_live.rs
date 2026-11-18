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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Live test: list recent runs from a real agent-analytics table.
//!
//! ```sh
//! GADK_EVAL_MONITOR_LIVE_PROJECT=<project> GADK_EVAL_MONITOR_LIVE_DATASET=<dataset> \
//!   cargo test -p gadk-eval-monitor --test bigquery_live -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (BigQuery Data Viewer and Job User). Optional:
//! `GADK_EVAL_MONITOR_LIVE_TABLE` (default `agent_events`), `GADK_EVAL_MONITOR_LIVE_DAYS`
//! (default 7).

use chrono::{TimeDelta, Utc};
use gadk_eval_monitor::{BigQueryTranscriptSource, TranscriptQuery, TranscriptSource};

#[tokio::test]
#[ignore = "needs GADK_EVAL_MONITOR_LIVE_PROJECT, GADK_EVAL_MONITOR_LIVE_DATASET and ADC"]
async fn lists_recent_runs() {
    let (Ok(project), Ok(dataset)) = (
        std::env::var("GADK_EVAL_MONITOR_LIVE_PROJECT"),
        std::env::var("GADK_EVAL_MONITOR_LIVE_DATASET"),
    ) else {
        panic!("set GADK_EVAL_MONITOR_LIVE_PROJECT and GADK_EVAL_MONITOR_LIVE_DATASET");
    };
    let table =
        std::env::var("GADK_EVAL_MONITOR_LIVE_TABLE").unwrap_or_else(|_| "agent_events".into());
    let days: i64 = std::env::var("GADK_EVAL_MONITOR_LIVE_DAYS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(7);
    let source = BigQueryTranscriptSource::new(&project, &dataset, &table).unwrap();
    let now = Utc::now();
    let q = TranscriptQuery {
        from: now - TimeDelta::days(days),
        to: now,
        agent: None,
        limit: 5,
    };
    let runs = source.list(&q).await.unwrap();
    for r in &runs {
        println!(
            "{} {} {} turns={} complete={} notes={:?}",
            r.started_at,
            r.invocation_id,
            r.agent,
            r.transcript.turns.len(),
            r.content_complete,
            r.notes
        );
    }
    assert!(runs.len() <= 5);
    assert!(runs.windows(2).all(|w| w[0].started_at >= w[1].started_at));
    assert!(
        runs.iter()
            .all(|r| r.started_at >= q.from && r.started_at < q.to)
    );
}
