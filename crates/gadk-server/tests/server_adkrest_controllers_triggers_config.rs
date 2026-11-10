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

//! Ports of ADK-Go `server/adkrest/controllers/triggers/config_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod triggers_support;

use std::io;
use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, Loader, SingleLoader};
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_server::rest::testutil::FakeSessionService;
use gadk_server::rest::triggers::{
    ControllerConfig, EventarcController, PubSubController, TriggerConfig,
};
use gadk_session::InMemorySessionService;
use gadk_session::compaction::Config as CompactionConfig;
use parking_lot::Mutex;
use triggers_support::{
    CountingSummarizer, EmptyLoader, Runs, StubSummarizer, build_pub_sub_request,
    controller_config, mock_agent, post, pubsub_body, read,
};

/// Go `TriggerConfig{MaxConcurrentRuns: 1}`.
const TC: TriggerConfig = TriggerConfig {
    max_concurrent_runs: 1,
    max_retries: 0,
    base_delay: std::time::Duration::ZERO,
    max_delay: std::time::Duration::ZERO,
};

/// Go `ControllerConfig{AgentLoader: loader, TriggerConfig: tc, Compaction: cmp}`: Go's nil
/// session service is an in-memory one here, and a nil loader is [`EmptyLoader`].
fn config(
    loader: Option<Arc<dyn Loader>>,
    compaction: Option<CompactionConfig>,
) -> ControllerConfig {
    ControllerConfig {
        trigger_config: TC,
        compaction,
        ..controller_config(
            Arc::new(InMemorySessionService::new()),
            loader.unwrap_or_else(|| Arc::new(EmptyLoader)),
        )
    }
}

// parity: skipped TestControllerConstructorTypesAreUnchanged: Go-deprecated NewPubSubController/NewEventarcController are not ported (spec Scope)

/// Go `TestControllerConfigCarriesCompaction`, per controller.
// parity: adapted: the runner's compaction field is private, so the config is observed reaching the runner through its summarizer being called on a delivery
#[tokio::test]
async fn test_controller_config_carries_compaction() {
    for name in ["pubsub", "eventarc"] {
        let summarizer = Arc::new(CountingSummarizer::default());
        let cfg = ControllerConfig {
            trigger_config: TC,
            compaction: Some(CompactionConfig {
                compaction_interval: 1,
                summarizer: Some(summarizer.clone()),
                ..Default::default()
            }),
            ..triggers_support::fake_config(mock_agent(vec![], Arc::new(Runs::default())))
        };
        let resp = match name {
            "pubsub" => {
                PubSubController::new(cfg)
                    .unwrap()
                    .pub_sub_trigger_handler(build_pub_sub_request("hello"))
                    .await
            }
            _ => {
                let req = post(
                    "/apps/test-agent/triggers/eventarc",
                    &[("ce-type", "google.cloud.pubsub.topic.v1.messagePublished")],
                    pubsub_body("hello", None),
                );
                EventarcController::new(cfg)
                    .unwrap()
                    .eventarc_trigger_handler(req)
                    .await
            }
        };
        assert_eq!(resp.status(), 200, "{name}");
        assert!(
            summarizer.count() > 0,
            "{name}: the compaction config passed in did not reach the runner"
        );
    }
}

/// Go `TestControllerCompactionDefaultsToNil`.
// parity: adapted: Go reads the private field via the deprecated constructor; here a delivery through a controller built without compaction leaves no compaction event in its session
#[tokio::test]
async fn test_controller_compaction_defaults_to_nil() {
    let service = Arc::new(FakeSessionService::default());
    let cfg = ControllerConfig {
        trigger_config: TC,
        ..controller_config(
            service.clone(),
            Arc::new(SingleLoader::new(mock_agent(
                vec![],
                Arc::new(Runs::default()),
            ))),
        )
    };
    assert!(cfg.compaction.is_none());
    let c = PubSubController::new(cfg).unwrap();
    let (code, _) = read(
        c.pub_sub_trigger_handler(build_pub_sub_request("hello"))
            .await,
    )
    .await;
    assert_eq!(code, 200);

    let sessions = service.sessions.lock();
    let session = sessions.values().next().unwrap();
    let events = session.session_events.0.lock();
    assert!(!events.is_empty());
    assert!(
        events.iter().all(|e| e.actions.compaction.is_none()),
        "a compaction event was written without a compaction config"
    );
}

/// A `tracing` writer capturing formatted records (Go `log.SetOutput(&buf)`).
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Go `TestTriggerCompactionWarnsAboutSlidingWindows`.
#[test]
fn test_trigger_compaction_warns_about_sliding_windows() {
    let cases: [(&str, Option<CompactionConfig>, &str); 5] = [
        (
            "interval above one cannot reach its interval in one attempt",
            Some(CompactionConfig {
                compaction_interval: 2,
                ..Default::default()
            }),
            "will not reach its interval",
        ),
        (
            // It does fire here, on every delivery; the waste is the summary, written into a
            // session that is discarded when the delivery ends.
            "interval of one fires and is wasted",
            Some(CompactionConfig {
                compaction_interval: 1,
                ..Default::default()
            }),
            "discarded when the delivery ends",
        ),
        (
            "tail retention works here and must not warn",
            Some(CompactionConfig {
                token_threshold: 1000,
                event_retention_size: 2,
                ..Default::default()
            }),
            "",
        ),
        (
            // The sliding window is just as inert alongside tail retention.
            "a sliding window still warns when tail retention is also set",
            Some(CompactionConfig {
                compaction_interval: 2,
                token_threshold: 1000,
                event_retention_size: 2,
                ..Default::default()
            }),
            "will not reach its interval",
        ),
        ("no config at all", None, ""),
    ];

    for (name, cfg, want) in cases {
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        // The warning is emitted while the runner is built, before any validation, so the
        // controller itself does not matter here.
        tracing::subscriber::with_default(subscriber, || {
            let _ = PubSubController::new(config(None, cfg));
        });

        let got = String::from_utf8(buf.0.lock().clone()).unwrap();
        if want.is_empty() {
            assert!(
                !got.contains("adk: sliding-window compaction"),
                "{name}: warned about a configuration that works here; log was {got:?}"
            );
        } else {
            assert!(
                got.contains(want),
                "{name}: warning does not mention {want:?}; log was {got:?}"
            );
        }
    }
}

/// Go's exact warning texts (`log.Printf` in `newRetriableRunner`).
#[test]
fn trigger_compaction_warning_texts() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_level(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for interval in [1, 3] {
            let _ = EventarcController::new(config(
                None,
                Some(CompactionConfig {
                    compaction_interval: interval,
                    ..Default::default()
                }),
            ));
        }
    });
    assert_eq!(
        String::from_utf8(buf.0.lock().clone()).unwrap(),
        "adk: sliding-window compaction is configured on a trigger controller with \
         CompactionInterval 1, so it fires on every delivery and writes a summary into a session \
         that is discarded when the delivery ends. Use TokenThreshold and EventRetentionSize to \
         compact within a single run.\n\
         adk: sliding-window compaction is configured on a trigger controller, but a delivery \
         handled in one attempt runs a single invocation, so the window will not reach its \
         interval. Use TokenThreshold and EventRetentionSize to compact within a single run.\n"
    );
}

/// Go `mustWorkflowAgent`: an agent with no model of its own.
fn must_workflow_agent() -> Arc<dyn Agent> {
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "wf_app".into(),
            ..Default::default()
        },
    })
    .unwrap()
}

/// Name, compaction config, loader (`None` is Go's nil) and whether construction succeeds.
type RefusalCase = (
    &'static str,
    CompactionConfig,
    Option<Arc<dyn Loader>>,
    bool,
);

/// Go `TestControllerRefusesACompactionConfigItCannotServe`.
#[test]
fn test_controller_refuses_a_compaction_config_it_cannot_serve() {
    let cases: [RefusalCase; 3] = [
        (
            // Enables no strategy at all.
            "a config that enables nothing",
            CompactionConfig::default(),
            None,
            false,
        ),
        (
            "a config with no summarizer over an agent with no model",
            CompactionConfig {
                compaction_interval: 2,
                ..Default::default()
            },
            Some(Arc::new(SingleLoader::new(must_workflow_agent()))),
            false,
        ),
        (
            "a usable config",
            CompactionConfig {
                token_threshold: 1000,
                event_retention_size: 2,
                summarizer: Some(Arc::new(StubSummarizer)),
                ..Default::default()
            },
            Some(Arc::new(SingleLoader::new(must_workflow_agent()))),
            true,
        ),
    ];

    for (name, cmp, loader, want_ok) in cases {
        let result = PubSubController::new(config(loader, Some(cmp)));
        assert_eq!(
            result.is_ok(),
            want_ok,
            "{name}: PubSubController::new() error = {:?}, want an error: {}",
            result.as_ref().err(),
            !want_ok
        );
    }
}

/// The refusal carries Go's text, from both constructors.
#[test]
fn controller_refusal_text_is_gos() {
    let cmp = CompactionConfig {
        compaction_interval: 2,
        ..Default::default()
    };
    let loader: Arc<dyn Loader> = Arc::new(SingleLoader::new(must_workflow_agent()));
    let want = "the Compaction config cannot serve app \"wf_app\": the Compaction config needs a \
                Summarizer: root agent \"wf_app\" is not an LLM agent, so no default model is \
                available";
    let pubsub = PubSubController::new(config(Some(loader.clone()), Some(cmp.clone())));
    assert_eq!(pubsub.err().unwrap().to_string(), want);
    let eventarc = EventarcController::new(config(Some(loader), Some(cmp)));
    assert_eq!(eventarc.err().unwrap().to_string(), want);
}
