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

//! Activation identity: identifies one activation of an invocation context (one end flag), so
//! callers can tell whether a callback runs under the same activation as an earlier one.

use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide source of activation ids; `0` is never handed out.
static NEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);

/// The identity of one end flag: assigned from a process-wide monotonic counter wherever a
/// context creates a new `ended` flag — the context builder (the first flag) and every fork
/// ([`ContextDelta`](crate::context::ContextDelta) changing an invocation-level field or asking for
/// a fresh end flag/invocation: agent runs, parallel branches, workflow node branches, fresh node
/// invocations). Contexts sharing an end flag share the id; deltas that do not fork (callback and
/// tool contexts, [`Context::with_agent_cancel`](crate::context::Context::with_agent_cancel)) keep
/// it. Ids are distinct across forks and never reused within the process; `0` is never assigned.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ActivationId(u64);

impl ActivationId {
    /// The next id from the process-wide counter.
    pub(crate) fn next() -> ActivationId {
        ActivationId(NEXT_ACTIVATION.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw counter value (never `0`).
    pub fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use futures::{FutureExt, StreamExt};
    use parking_lot::Mutex;

    use super::ActivationId;
    use crate::agent::{AgentConfig, new};
    use crate::callback::BeforeAgentCallback;
    use crate::context::builder::test_support::named_agent;
    use crate::context::{Context, ContextDelta, InvocationContext};

    #[test]
    fn builder_assigns_a_fresh_nonzero_id() {
        let a = Context::builder().build();
        let b = Context::builder().build();
        assert_ne!(a.activation_id(), b.activation_id());
        assert_ne!(a.activation_id().get(), 0);
        assert!(b.activation_id() > a.activation_id(), "monotonic");
    }

    #[test]
    fn non_forking_deltas_keep_the_id() {
        let ctx = Context::builder().build();
        let id = ctx.activation_id();
        let tool = ctx.with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            ..Default::default()
        });
        let callback = ctx.with_delta(ContextDelta {
            actions: Some(crate::context::delta::new_actions()),
            ..Default::default()
        });
        let node = ctx.with_delta(ContextDelta {
            path: Some("wf@1/n@1".into()),
            run_id: Some("1".into()),
            ..Default::default()
        });
        let (cancellable, _guard) = ctx.with_agent_cancel();
        assert_eq!(tool.activation_id(), id);
        assert_eq!(callback.activation_id(), id);
        assert_eq!(node.activation_id(), id);
        assert_eq!(cancellable.activation_id(), id);
        assert_eq!(ctx.as_invocation().activation_id(), id);
        assert_eq!(ctx.clone().activation_id(), id);
    }

    #[test]
    fn every_fork_gets_a_new_id_never_reused() {
        let ctx = Context::builder().build();
        let forks = [
            ctx.with_delta(ContextDelta {
                agent: Some(named_agent("a")),
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                branch: Some("b".into()),
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                fresh_end_flag: true,
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                fresh_invocation: true,
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                cancellation_token: Some(Default::default()),
                ..Default::default()
            }),
        ];
        let mut seen = vec![ctx.activation_id()];
        for f in &forks {
            assert!(!seen.contains(&f.activation_id()), "fork reused an id");
            seen.push(f.activation_id());
        }
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(sorted, seen, "ids are assigned in creation order");
    }

    #[test]
    fn ids_are_unique_across_threads() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..500).map(|_| ActivationId::next()).collect::<Vec<_>>())
            })
            .collect();
        let mut all: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n);
    }

    /// One agent run: before-agent callback, run body and after-agent callback see one id, which
    /// differs from the caller's and from a second run's.
    #[tokio::test]
    async fn one_agent_run_has_one_activation() {
        let seen: Arc<Mutex<Vec<(&'static str, ActivationId)>>> = Arc::default();
        let cb = |tag: &'static str, seen: Arc<Mutex<Vec<(&'static str, ActivationId)>>>| {
            let cb: BeforeAgentCallback = Arc::new(move |ctx: Context| {
                seen.lock().push((tag, ctx.activation_id()));
                async { Ok(None) }.boxed()
            });
            cb
        };
        let run_seen = seen.clone();
        let agent = new(AgentConfig {
            name: "a".into(),
            before_agent_callbacks: vec![cb("before", seen.clone())],
            after_agent_callbacks: vec![cb("after", seen.clone())],
            run: Some(Arc::new(move |ctx: InvocationContext| {
                run_seen.lock().push(("run", ctx.activation_id()));
                futures::stream::empty().boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        let outer = InvocationContext::builder()
            .agent(agent.clone())
            .build_invocation();
        let _: Vec<_> = agent.run(outer.clone()).collect().await;
        let first: Vec<_> = std::mem::take(&mut *seen.lock());
        let _: Vec<_> = agent.run(outer.clone()).collect().await;
        let second: Vec<_> = std::mem::take(&mut *seen.lock());

        let tags: Vec<_> = first.iter().map(|(t, _)| *t).collect();
        assert_eq!(tags, ["before", "run", "after"]);
        let id1 = first[0].1;
        assert!(first.iter().all(|(_, id)| *id == id1), "{first:?}");
        assert_ne!(id1, outer.activation_id(), "the agent run forks");
        let id2 = second[0].1;
        assert!(second.iter().all(|(_, id)| *id == id2), "{second:?}");
        assert_ne!(id1, id2, "a second run is a new activation");
    }
}
