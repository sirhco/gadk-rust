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

//! Ports of Go internal/agent/compactionctx/compactionctx_test.go.

use std::sync::Arc;

use gadk_agent::InvocationContext;
use gadk_session::InMemorySessionService;
use gadk_session::compaction::Config;

use crate::compaction::ProgressGate;
use crate::compaction::runtime::{
    CompactionRuntime, MAX_FAILURES_PER_INVOCATION, already_compacted, attach, configured, enabled,
    from_ctx,
};

fn tail_cfg() -> Arc<Config> {
    Arc::new(Config {
        token_threshold: 1000,
        event_retention_size: 2,
        ..Default::default()
    })
}

// parity: adapted: Go's context.Background() is a context built without the slot, and Go's nil-receiver MarkCompacted is Option::None in Rust
#[test]
fn test_from_context_without_runtime() {
    let ctx = InvocationContext::builder().build_invocation();
    let rt = from_ctx(&ctx);
    assert!(
        rt.is_none(),
        "from_ctx() is Some on a context without a runtime"
    );
    // The nil receiver must answer, not panic.
    assert!(
        !(configured(&ctx) || enabled(&ctx) || already_compacted(&ctx)),
        "a missing runtime reported itself as usable"
    );
    if let Some(rt) = rt {
        rt.mark_compacted();
    }
}

#[test]
fn test_round_trip() {
    let want = CompactionRuntime::new(
        Arc::new(Config {
            compaction_interval: 2,
            ..Default::default()
        }),
        Some(Arc::new(InMemorySessionService::new())),
    );
    let ctx = attach(InvocationContext::builder(), want.clone()).build_invocation();
    let got = from_ctx(&ctx).expect("from_ctx() returned None");
    assert!(
        Arc::ptr_eq(&got, &want),
        "from_ctx() did not return the stored runtime"
    );
    assert!(
        got.configured(),
        "configured() = false for a runtime with a config"
    );
}

/// Sub-agents in a parallel workflow share one runtime, hence the atomic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_mark_compacted_is_safe_under_concurrency() {
    let rt = CompactionRuntime::new(
        Arc::new(Config {
            compaction_interval: 1,
            ..Default::default()
        }),
        None,
    );
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let rt = rt.clone();
        tasks.push(tokio::spawn(async move {
            rt.mark_compacted();
            let _ = rt.already_compacted();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert!(
        rt.already_compacted(),
        "already_compacted() = false after mark_compacted()"
    );
}

#[test]
fn test_progress_gate_re_arms_once_the_prompt_recovers() {
    let rt = CompactionRuntime::new(tail_cfg(), None);
    assert!(
        rt.gate_for("a", "", "").allow_at(),
        "allow_at() = false before any compaction, want true"
    );
    rt.gate_for("a", "", "").record_at(2000);
    assert!(
        !rt.gate_for("a", "", "").allow_at(),
        "allow_at() = true straight after a compaction, want false"
    );
    rt.gate_for("a", "", "").recovered();
    assert!(
        rt.gate_for("a", "", "").allow_at(),
        "allow_at() = false after the prompt recovered, want true: a turn that grows again must be able to compact again"
    );
}

#[test]
fn test_progress_gate_stays_closed_while_compaction_cannot_help() {
    let rt = CompactionRuntime::new(tail_cfg(), None);
    rt.gate_for("a", "", "").record_at(5000);
    for tokens in [4900, 5200, 12000] {
        assert!(
            !rt.gate_for("a", "", "").allow_at(),
            "allow_at() = true at {tokens} tokens, want false while the prompt has never come back under the threshold"
        );
    }
}

#[test]
fn test_progress_gate_record_at_keeps_a_zero_count_distinct() {
    let rt = CompactionRuntime::new(tail_cfg(), None);
    rt.gate_for("a", "", "").record_at(0);
    assert!(
        !rt.gate_for("a", "", "").allow_at(),
        "allow_at() = true after record_at(0), want false"
    );
}

#[test]
fn test_gate_is_scoped_per_agent() {
    let rt = CompactionRuntime::new(tail_cfg(), None);
    let (worker, critic) = (
        rt.gate_for("worker", "root", ""),
        rt.gate_for("critic", "root", ""),
    );
    assert!(
        worker.allow_at() && critic.allow_at(),
        "both agents should start allowed"
    );
    worker.record_at(2000);
    assert!(
        !worker.allow_at(),
        "the worker compacted, so it should be gated"
    );
    assert!(
        critic.allow_at(),
        "the critic did not compact, so the worker's compaction must not gate it"
    );
    for _ in 0..MAX_FAILURES_PER_INVOCATION - 1 {
        critic.failed();
        assert!(
            critic.allow_at(),
            "the critic gave up before its retry budget was spent"
        );
    }
    critic.failed();
    assert!(
        !critic.allow_at(),
        "the critic spent its retry budget and should stop attempting this invocation"
    );
    critic.recovered();
    assert!(
        !critic.allow_at(),
        "recovered reopened a gate closed by spent retries, which the prompt size cannot justify"
    );
    let bystander = rt.gate_for("bystander", "root", "");
    assert!(
        bystander.allow_at(),
        "an agent that has neither compacted nor failed was gated by another agent's failures"
    );
}
