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

//! Port of Go agent/llmagent/llmagent_tail_retention_test.go (package llmagent_test, mapped
//! external→internal because it reads compactioninternal).

#![cfg(feature = "testutil")]

use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_agent::{BeforeModelCallback, Context};
use gadk_genai::Content;
use gadk_model::{Backend, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use gadk_session::compaction::{Config, LlmSummarizer, LlmSummarizerConfig};
use parking_lot::Mutex;

use super::agent_llmagent_llmagent_compaction::{compaction_model, session_events_for, text_of};
use crate::compaction::apply::has_usable_summary;
use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::testutil::{TestAgentRunner, collect_text_parts};

/// Port of Go recordingModel: records the text of every prompt and delegates.
struct RecordingModel {
    inner: Arc<dyn Llm>,
    prompts: Mutex<Vec<String>>,
}

impl RecordingModel {
    fn seen(&self) -> Vec<String> {
        self.prompts.lock().clone()
    }
}

impl Llm for RecordingModel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let text: String = req
            .contents
            .iter()
            .flat_map(|c| c.parts.iter())
            .map(|p| p.text.as_str())
            .collect();
        self.prompts.lock().push(text);
        self.inner.generate_content(req, stream)
    }

    fn google_variant(&self) -> Option<Backend> {
        self.inner.google_variant()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go agentPrompt: one agent prompt and how many summarizations had run before it.
#[derive(Clone)]
struct AgentPrompt {
    contents: Vec<Content>,
    summarizations: usize,
}

#[tokio::test]
async fn test_tail_retention_e2_e() {
    let model = compaction_model("TestTailRetentionE2E").await;
    let summarizer_model = Arc::new(RecordingModel {
        inner: model.clone(),
        prompts: Mutex::default(),
    });
    let summarizer = LlmSummarizer::new(LlmSummarizerConfig {
        model: Some(summarizer_model.clone()),
        ..Default::default()
    })
    .unwrap();

    let prompts: Arc<Mutex<Vec<AgentPrompt>>> = Arc::default();
    let capture: BeforeModelCallback = {
        let (prompts, sm) = (prompts.clone(), summarizer_model.clone());
        Arc::new(move |_ctx: Context, req: LlmRequest| {
            let summarizations = sm.seen().len();
            prompts.lock().push(AgentPrompt {
                contents: req.contents.clone(),
                summarizations,
            });
            Box::pin(async move { (req, Ok(None)) })
        })
    };
    let a = llm_agent::new(LlmAgentConfig {
        name: "tail_retention_agent".into(),
        description: "agent used to exercise tail-retention compaction".into(),
        model: Some(model.clone()),
        instruction: "You are a concise assistant. Answer in one short sentence.".into(),
        before_model_callbacks: vec![capture],
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        ..Default::default()
    })
    .unwrap();
    let cfg = Config {
        token_threshold: 400,
        event_retention_size: 2,
        summarizer: Some(Arc::new(summarizer)),
        ..Default::default()
    };
    let r = TestAgentRunner::with_compaction(a, cfg.clone()).await;

    let turns = [
        "The deployment region for this project is europe-west4.",
        "Explain in one sentence why connection pooling matters.",
        "In one sentence, when is a circuit breaker worth adding?",
        "One sentence: what is the point of a canary deploy?",
        "Briefly, why do idempotency keys matter for retries?",
        "One sentence on structured logging versus plain text.",
        "Why might p99 latency matter more than the mean? One sentence.",
        "One sentence: what does backpressure mean in a queue?",
    ];
    let mut run_err = None;
    for (i, turn) in turns.iter().enumerate() {
        if let Err(e) = collect_text_parts(r.run(turn)).await {
            run_err = Some((i, e));
            break;
        }
    }

    let captured = prompts.lock().clone();
    let max_contents = (cfg.event_retention_size + 2) as usize;
    let (mut after_summary, mut ran) = (0, 0);
    for (i, p) in captured.iter().enumerate() {
        if p.summarizations == ran {
            continue;
        }
        ran = p.summarizations;
        after_summary += 1;
        assert!(
            p.contents.len() <= max_contents,
            "prompt {}, the first after summarization {ran}, carries {} contents, want at most {max_contents} (summary, retained tail of {}, new message): history is not being replaced by a summary",
            i + 1,
            p.contents.len(),
            cfg.event_retention_size
        );
    }
    assert!(
        after_summary > 0,
        "none of {} agent prompts followed a summarization, so compaction never ran",
        captured.len()
    );

    let events = session_events_for(&r).await;
    let summaries: Vec<&Event> = events.iter().filter(|e| has_usable_summary(e)).collect();
    let calls = summarizer_model.seen();
    let mut k = 1;
    while k < calls.len() && k <= summaries.len() {
        let prev = text_of(
            summaries[k - 1]
                .actions
                .compaction
                .as_ref()
                .unwrap()
                .compacted_content
                .as_ref(),
        );
        let prev = prev.trim();
        assert!(!prev.is_empty(), "summary {k} is empty");
        let rendered = prev.replace('\n', "\\n");
        assert!(
            calls[k].contains(&rendered),
            "summarization {} was not handed summary {k} ({} chars as rendered), so it was not built on its predecessor",
            k + 1,
            rendered.len()
        );
        k += 1;
    }

    if let Some((i, e)) = run_err {
        panic!("turn {} ({:?}) failed: {e}", i + 1, turns[i]);
    }
    assert!(
        summaries.len() >= 2,
        "got {} compaction records over {} turns, need at least 2 for one summary to be built on another; this test exercised nothing",
        summaries.len(),
        turns.len()
    );
    assert_eq!(
        calls.len(),
        summaries.len(),
        "summarizer called {} times for {} stored records, so calls cannot be paired with the records they produced",
        calls.len(),
        summaries.len()
    );

    let base = summaries[0]
        .actions
        .compaction
        .as_ref()
        .unwrap()
        .start_timestamp;
    for (i, s) in summaries[1..].iter().enumerate() {
        let c = s.actions.compaction.as_ref().unwrap();
        assert_eq!(
            c.start_timestamp,
            base,
            "summary {} starts at {:?}, want {base:?}: records are accumulating rather than rolling",
            i + 1,
            c.start_timestamp
        );
        let prev = summaries[i].actions.compaction.as_ref().unwrap();
        assert!(
            c.end_timestamp > prev.end_timestamp,
            "summary {} ends at {:?}, not after its predecessor's {:?}: the window did not advance",
            i + 1,
            c.end_timestamp,
            prev.end_timestamp
        );
    }
}
