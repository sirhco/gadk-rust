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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures::FutureExt;
use gadk_artifact::SaveResponse;
use gadk_genai::Part;
use gadk_session::EventActions;
use parking_lot::Mutex;

use crate::context::builder::test_support::SaveOnlyArtifacts;
use crate::context::invocation::TrackedArtifacts;
use crate::context::{ContextDelta, InvocationContext};
use crate::error::AgentError;
use crate::services::Artifacts;

// parity: adapted: Go's NewToolContext-wrapping-branchOverride chain becomes a tool-call delta followed by a branch delta on the same Context
#[tokio::test]
async fn test_common_context_context_fallback_delegation() {
    let want_path = "wf/child@123".to_string();
    let want_ancestors = vec!["wf/root".to_string(), "wf/parent".to_string()];
    let dyn_ctx = InvocationContext::builder()
        .build_invocation()
        .into_context()
        .with_delta(ContextDelta {
            path: Some(want_path.clone()),
            output_for_ancestors: Some(want_ancestors.clone()),
            run_id: Some("123".into()),
            sub_scheduler: Some(None),
            ..Default::default()
        });
    let wrapped = dyn_ctx
        .with_delta(ContextDelta {
            function_call_id: Some("call-id-1".into()),
            ..Default::default()
        })
        .with_delta(ContextDelta {
            branch: Some("parallel-branch".into()),
            ..Default::default()
        });
    for (name, got) in [("direct", dyn_ctx), ("tool + branch override", wrapped)] {
        assert_eq!(got.path(), want_path, "{name}");
        assert_eq!(got.output_for_ancestors(), want_ancestors, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_tracked_artifacts_concurrent_save() {
    const SAVES: usize = 16;
    for kind in ["tool", "callback"] {
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let rel = release.clone();
        let backend = SaveOnlyArtifacts(Arc::new(move |_name: String| {
            let (tx, rel) = (entered_tx.clone(), rel.clone());
            async move {
                let _ = tx.send(());
                rel.acquire()
                    .await
                    .map_err(|e| AgentError::other(e.to_string()))?
                    .forget();
                Ok(SaveResponse { version: 1 })
            }
            .boxed()
        }));
        let actions = Arc::new(Mutex::new(EventActions::default()));
        let base = InvocationContext::builder()
            .artifacts(Arc::new(backend))
            .build();
        let ctx = if kind == "tool" {
            base.with_delta(ContextDelta {
                function_call_id: Some("call".into()),
                actions: Some(actions.clone()),
                ..Default::default()
            })
        } else {
            base.with_delta(ContextDelta {
                actions: Some(actions.clone()),
                ..Default::default()
            })
        };
        let mut saves = Vec::new();
        for i in 0..SAVES {
            let c = ctx.clone();
            saves.push(tokio::spawn(async move {
                c.artifacts()
                    .unwrap()
                    .save(&format!("report-{i}.txt"), Part::from_text("synthetic"))
                    .await
                    .unwrap();
            }));
        }
        for _ in 0..SAVES {
            tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
                .await
                .expect("backend saves did not run concurrently");
        }
        release.add_permits(SAVES);
        for s in saves {
            s.await.unwrap();
        }
        let delta = actions.lock().artifact_delta.clone().unwrap();
        assert_eq!(delta.len(), SAVES, "{kind}");
        for i in 0..SAVES {
            assert_eq!(
                delta.get(&format!("report-{i}.txt")),
                Some(&1),
                "{kind}: save {i}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_tracked_artifacts_out_of_order_save() {
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let rel = release.clone();
    let calls = Arc::new(AtomicI64::new(0));
    let backend = SaveOnlyArtifacts(Arc::new(move |_name: String| {
        let (tx, rel, calls) = (entered_tx.clone(), rel.clone(), calls.clone());
        async move {
            let version = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if version == 1 {
                let _ = tx.send(());
                rel.acquire()
                    .await
                    .map_err(|e| AgentError::other(e.to_string()))?
                    .forget();
            }
            Ok(SaveResponse { version })
        }
        .boxed()
    }));
    let tool_ctx = InvocationContext::builder()
        .artifacts(Arc::new(backend))
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("call".into()),
            ..Default::default()
        });
    let first_ctx = tool_ctx.clone();
    let first = tokio::spawn(async move {
        first_ctx
            .artifacts()
            .unwrap()
            .save("report.txt", Part::from_text("first"))
            .await
            .expect("first save failed");
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
        .await
        .expect("first save never entered");
    tool_ctx
        .artifacts()
        .unwrap()
        .save("report.txt", Part::from_text("second"))
        .await
        .expect("second save failed");
    release.add_permits(1);
    first.await.unwrap();
    assert_eq!(
        tool_ctx.actions().artifact_delta.unwrap().get("report.txt"),
        Some(&2),
        "recorded version"
    );
}

#[tokio::test]
async fn test_tracked_artifacts_save_delta() {
    let cases: [(&str, EventActions, i64, BTreeMap<String, i64>); 2] = [
        (
            "zero version",
            EventActions::default(),
            0,
            [("report.txt".to_string(), 0)].into(),
        ),
        (
            "newer version",
            EventActions {
                artifact_delta: Some([("report.txt".to_string(), 1)].into()),
                ..Default::default()
            },
            2,
            [("report.txt".to_string(), 2)].into(),
        ),
    ];
    for (name, actions, version, want) in cases {
        let actions = Arc::new(Mutex::new(actions));
        let tracked = TrackedArtifacts::new(
            Arc::new(SaveOnlyArtifacts::version(version)),
            actions.clone(),
        );
        let resp = tracked
            .save("report.txt", Part::from_text("x"))
            .await
            .unwrap();
        assert_eq!(resp, SaveResponse { version }, "{name}");
        assert_eq!(
            actions.lock().artifact_delta.clone().unwrap(),
            want,
            "{name}"
        );
    }
}
