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

//! Live test: list recent runs from a real project's Cloud Trace (read-only).
//!
//! ```sh
//! GADK_EVAL_MONITOR_LIVE_TRACE_PROJECT=<project> \
//!   cargo test -p gadk-eval-monitor --test cloudtrace_live -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (Cloud Trace User). Optional:
//! `GADK_EVAL_MONITOR_LIVE_DAYS` (default 7).

use chrono::{TimeDelta, Utc};
use gadk_eval_monitor::{CloudTraceTranscriptSource, TranscriptQuery, TranscriptSource};

#[tokio::test]
#[ignore = "needs GADK_EVAL_MONITOR_LIVE_TRACE_PROJECT and ADC"]
async fn lists_recent_runs_from_cloud_trace() {
    let Ok(project) = std::env::var("GADK_EVAL_MONITOR_LIVE_TRACE_PROJECT") else {
        panic!("set GADK_EVAL_MONITOR_LIVE_TRACE_PROJECT");
    };
    let days: i64 = std::env::var("GADK_EVAL_MONITOR_LIVE_DAYS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(7);
    let source = CloudTraceTranscriptSource::new(&project).unwrap();
    let now = Utc::now();
    let q = TranscriptQuery {
        from: now - TimeDelta::days(days),
        to: now,
        agent: None,
        limit: 20,
    };
    let runs = source.list(&q).await.unwrap();
    for r in &runs {
        println!(
            "{} {} {} trace={:?} calls={} complete={} tools_complete={} notes={:?}",
            r.started_at,
            r.invocation_id,
            r.agent,
            r.trace_id,
            r.transcript.turns[0].calls.len(),
            r.content_complete,
            r.tools_complete,
            r.notes
        );
    }
    assert!(runs.len() <= 20);
    assert!(runs.windows(2).all(|w| w[0].started_at >= w[1].started_at));
    for r in &runs {
        assert!(!r.invocation_id.is_empty(), "{r:?}");
        assert!(!r.agent.is_empty(), "{r:?}");
        assert!(r.started_at >= q.from && r.started_at < q.to, "{r:?}");
        let trace = r.trace_id.as_deref().unwrap_or_default();
        assert!(
            trace.len() == 32 && trace.bytes().all(|b| b.is_ascii_hexdigit()),
            "{trace:?}"
        );
        if r.content_complete {
            assert!(!r.transcript.turns[0].user.is_empty(), "{r:?}");
        }
    }
}
