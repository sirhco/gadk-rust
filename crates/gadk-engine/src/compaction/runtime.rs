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

//! The per-invocation compaction runtime. It travels in `gadk_agent`'s opaque per-invocation slot;
//! [`from_ctx`] is the only downcast.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gadk_agent::{ContextBuilder, InvocationContext};
use gadk_session::SessionService;
use gadk_session::compaction::Config;

use super::{ProgressGate, has_tail_retention};

/// Failed tail-retention attempts one agent may make per invocation.
pub(crate) const MAX_FAILURES_PER_INVOCATION: u32 = 3;

/// The progress state behind one [`Gate`].
#[derive(Debug, Default)]
struct GateState {
    /// Prompt size the last compaction ran at; 0 = none since the prompt last recovered.
    last_compaction_tokens: i64,
    failures: u32,
}

/// The compaction config and session service of one invocation, whether it has compacted, and
/// the per-agent progress gates.
pub(crate) struct CompactionRuntime {
    config: Arc<Config>,
    session_service: Option<Arc<dyn SessionService>>,
    compacted: AtomicBool,
    gates: parking_lot::Mutex<HashMap<String, GateState>>,
}

impl CompactionRuntime {
    /// Creates the runtime (a disabled config is "no runtime": none is attached).
    pub(crate) fn new(
        cfg: Arc<Config>,
        svc: Option<Arc<dyn SessionService>>,
    ) -> Arc<CompactionRuntime> {
        Arc::new(CompactionRuntime {
            config: cfg,
            session_service: svc,
            compacted: AtomicBool::new(false),
            gates: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    /// The compaction config.
    pub(crate) fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// The session service the compaction reads from and writes to, if any.
    pub(crate) fn session_service(&self) -> Option<&Arc<dyn SessionService>> {
        self.session_service.as_ref()
    }

    /// Records that tail retention compacted in this invocation.
    pub(crate) fn mark_compacted(&self) {
        self.compacted.store(true, Ordering::SeqCst);
    }

    /// Whether tail retention has compacted in this invocation.
    pub(crate) fn already_compacted(&self) -> bool {
        self.compacted.load(Ordering::SeqCst)
    }

    /// Whether a config is present: a runtime always carries one.
    pub(crate) fn configured(&self) -> bool {
        true
    }

    /// Whether tail retention can run (a session service and a threshold).
    pub(crate) fn enabled(&self) -> bool {
        self.session_service.is_some() && has_tail_retention(Some(&self.config))
    }

    /// The progress gate of one agent in one branch and isolation scope.
    pub(crate) fn gate_for(
        self: &Arc<Self>,
        agent_name: &str,
        branch: &str,
        isolation_scope: &str,
    ) -> Arc<Gate> {
        Arc::new(Gate {
            rt: self.clone(),
            scope: format!("{agent_name}\x00{branch}\x00{isolation_scope}"),
        })
    }

    fn with_state<R>(&self, scope: &str, f: impl FnOnce(&mut GateState) -> R) -> R {
        let mut gates = self.gates.lock();
        f(gates.entry(scope.to_owned()).or_default())
    }
}

/// The [`ProgressGate`] of one agent in one branch and isolation scope.
pub(crate) struct Gate {
    rt: Arc<CompactionRuntime>,
    scope: String,
}

impl ProgressGate for Gate {
    /// Allows a compaction when none has run since the prompt last recovered and fewer than
    /// [`MAX_FAILURES_PER_INVOCATION`] attempts have failed.
    fn allow_at(&self) -> bool {
        self.rt.with_state(&self.scope, |st| {
            st.last_compaction_tokens == 0 && st.failures < MAX_FAILURES_PER_INVOCATION
        })
    }

    /// Records a compaction at `tokens`; a zero count is kept distinct from "nothing recorded".
    fn record_at(&self, tokens: i64) {
        self.rt
            .with_state(&self.scope, |st| st.last_compaction_tokens = tokens.max(1));
    }

    /// Re-arms after a success; spent retries stay spent.
    fn recovered(&self) {
        self.rt
            .with_state(&self.scope, |st| st.last_compaction_tokens = 0);
    }

    /// Counts a failed or declined attempt.
    fn failed(&self) {
        self.rt.with_state(&self.scope, |st| st.failures += 1);
    }
}

/// The compaction runtime attached to the invocation, if any.
pub(crate) fn from_ctx(ctx: &InvocationContext) -> Option<Arc<CompactionRuntime>> {
    ctx.compaction_runtime()?
        .downcast::<CompactionRuntime>()
        .ok()
}

/// Attaches the runtime to the builder of the invocation context.
pub(crate) fn attach(b: ContextBuilder, rt: Arc<CompactionRuntime>) -> ContextBuilder {
    b.compaction_runtime(rt)
}

/// Whether the invocation has a runtime with a config (false without a runtime).
pub(crate) fn configured(ctx: &InvocationContext) -> bool {
    from_ctx(ctx).is_some_and(|rt| rt.configured())
}

/// Whether the invocation's runtime can run tail retention (false without a runtime).
#[cfg(test)]
pub(crate) fn enabled(ctx: &InvocationContext) -> bool {
    from_ctx(ctx).is_some_and(|rt| rt.enabled())
}

/// Whether the invocation's runtime has already compacted (false without a runtime).
pub(crate) fn already_compacted(ctx: &InvocationContext) -> bool {
    from_ctx(ctx).is_some_and(|rt| rt.already_compacted())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt;
    use gadk_agent::{AgentConfig, ContextDelta, EventStream, InvocationContext, new_agent};
    use gadk_session::InMemorySessionService;
    use parking_lot::Mutex;

    use super::*;
    use crate::agents::parallel_agent::{self, ParallelAgentConfig};

    fn runtime() -> Arc<CompactionRuntime> {
        CompactionRuntime::new(
            Arc::new(Config {
                token_threshold: 10,
                event_retention_size: 1,
                ..Default::default()
            }),
            Some(Arc::new(InMemorySessionService::new())),
        )
    }

    #[test]
    fn slot_round_trips_and_absent_slot_is_not_configured() {
        let rt = runtime();
        let ctx = attach(InvocationContext::builder(), rt.clone()).build_invocation();
        assert!(Arc::ptr_eq(&from_ctx(&ctx).unwrap(), &rt));
        assert!(configured(&ctx) && enabled(&ctx) && !already_compacted(&ctx));
        rt.mark_compacted();
        assert!(already_compacted(&ctx));

        let bare = InvocationContext::builder().build_invocation();
        assert!(from_ctx(&bare).is_none());
        assert!(!configured(&bare) && !enabled(&bare) && !already_compacted(&bare));

        // A slot holding something else is not a runtime.
        let other = InvocationContext::builder()
            .compaction_runtime(Arc::new(5u8))
            .build_invocation();
        assert!(from_ctx(&other).is_none() && !configured(&other));
    }

    #[test]
    fn enabled_needs_a_session_service_and_a_threshold() {
        let no_svc = CompactionRuntime::new(
            Arc::new(Config {
                token_threshold: 10,
                event_retention_size: 1,
                ..Default::default()
            }),
            None,
        );
        assert!(no_svc.configured() && !no_svc.enabled());
        let sliding_only = CompactionRuntime::new(
            Arc::new(Config {
                compaction_interval: 2,
                ..Default::default()
            }),
            Some(Arc::new(InMemorySessionService::new())),
        );
        assert!(!sliding_only.enabled());
        assert!(runtime().enabled());
    }

    #[test]
    fn derived_contexts_share_the_root_runtime() {
        let rt = runtime();
        let ctx = attach(InvocationContext::builder(), rt.clone()).build_invocation();
        let derived = [
            ctx.with_delta(ContextDelta {
                branch: Some("root.child".into()),
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                fresh_invocation: true,
                ..Default::default()
            }),
            ctx.clone()
                .into_context()
                .with_delta(ContextDelta {
                    function_call_id: Some("fc-1".into()),
                    ..Default::default()
                })
                .as_invocation()
                .clone(),
            ctx.clone()
                .into_context()
                .with_agent_cancel()
                .0
                .as_invocation()
                .clone(),
        ];
        for d in &derived {
            assert!(
                Arc::ptr_eq(&from_ctx(d).unwrap(), &rt),
                "a derived context lost the runtime"
            );
        }
    }

    #[tokio::test]
    async fn parallel_sub_agents_share_the_root_runtime() {
        let seen: Arc<Mutex<Vec<Option<Arc<CompactionRuntime>>>>> = Arc::default();
        let probe = |name: &str| {
            let seen = seen.clone();
            new_agent(AgentConfig {
                name: name.into(),
                run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
                    seen.lock().push(from_ctx(&ctx));
                    Box::pin(futures::stream::empty())
                })),
                ..Default::default()
            })
            .unwrap()
        };
        let par = parallel_agent::new(ParallelAgentConfig {
            agent: AgentConfig {
                name: "par".into(),
                sub_agents: vec![probe("left"), probe("right")],
                ..Default::default()
            },
        })
        .unwrap();
        let rt = runtime();
        let ctx = attach(InvocationContext::builder(), rt.clone())
            .agent(par.clone())
            .build_invocation();
        let mut events = par.run(ctx);
        while let Some(item) = events.next().await {
            item.unwrap();
        }
        let seen = seen.lock();
        assert_eq!(seen.len(), 2, "both sub-agents should have run");
        for got in seen.iter() {
            assert!(Arc::ptr_eq(
                got.as_ref().expect("sub-agent saw no runtime"),
                &rt
            ));
        }
    }
}
