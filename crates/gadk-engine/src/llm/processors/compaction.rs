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

//! Request-time tail-retention compaction: summarises older session events before the model call.

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{AgentError, InvocationContext, Mode};
use gadk_model::LlmRequest;
use gadk_session::Event;
use gadk_session::compaction::CompactionError;

use crate::compaction::tail_retention::{
    TokenCounter, TurnScope, estimate_tokens_from_contents, tail_retention,
};
use crate::compaction::{BoxError, ProgressGate, apply, runtime, summary_event::sanitize_summary};
use crate::llm::{
    flow::Flow, mode::mode_for, processors::contents::build_contents_default_with_call_source,
    state::reveal,
};

/// Compacts the session in place before the model call
/// once its prompt has grown past the configured token threshold.
///
/// Every failure degrades to an uncompacted prompt (logged, never yielded). It yields no events.
/// Tail-retention summaries do not pass through plugins. A summarizer panic is not caught here.
pub(crate) fn compaction_request_processor<'a>(
    ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        run(ctx).await;
        Ok(Vec::new())
    })
}

/// The processor body: resolves the runtime and session, chooses a window, summarises and appends.
async fn run(ctx: &InvocationContext) {
    let Some(rt) = runtime::from_ctx(ctx) else {
        return;
    };
    if !rt.enabled() {
        return;
    }
    // enabled() implies a session service.
    let Some(svc) = rt.session_service().cloned() else {
        return;
    };

    // Writers use the session underneath any wrapper: every session service
    // downcasts to its own type, and appending to this handle is what makes the summary visible
    // to the contents processor in this same model call.
    let sess = apply::unwrap_session(&ctx.session());

    // Captured before the window is chosen, so the race check knows what the summary saw.
    let before = apply::known_event_ids(&*sess);

    // A canceled turn spends no model call on compaction.
    if ctx.cancellation_token().is_cancelled() {
        return;
    }
    let agent = ctx.agent();
    let branch = ctx.branch();
    let isolation_scope = ctx.isolation_scope();
    let scope = TurnScope {
        invocation_id: ctx.invocation_id().to_string(),
        branch: branch.clone(),
        isolation_scope: isolation_scope.clone(),
    };
    let estimate = prompt_token_estimator(ctx);
    let gate: Arc<dyn ProgressGate> = rt.gate_for(agent.name(), &branch, &isolation_scope);
    let (mut summary, finish) = match tail_retention(
        ctx.platform(),
        rt.config(),
        Some(&*sess),
        &scope,
        Some(&*estimate),
        Some(gate),
    )
    .await
    {
        Err(err) => {
            degrade("token-threshold", err);
            return;
        }
        Ok((None, _)) => return,
        Ok((Some(summary), finish)) => (summary, finish),
    };

    if ctx.cancellation_token().is_cancelled() {
        finish.finish(None, "the turn ended before the summary could be stored");
        return;
    }
    let latest = match apply::reload_session(Some(svc.as_ref()), &*sess).await {
        Ok(latest) => latest,
        Err(err) => {
            finish.finish(Some(&*err as &(dyn std::error::Error + 'static)), "");
            degrade("token-threshold", err);
            return;
        }
    };
    if apply::range_raced_since(&*latest, &before, &summary) {
        finish.finish(
            None,
            "another compaction covering the same events landed while summarizing",
        );
        tracing::warn!(
            target: "adk",
            "adk: discarding a tail-retention summary because the session changed inside its range while summarizing"
        );
        return;
    }

    if !sanitize_summary(&mut summary) {
        finish.finish(None, "the summary held nothing usable");
        tracing::warn!(
            target: "adk",
            "adk: discarding a tail-retention summary because it held no usable content"
        );
        return;
    }

    if let Err(e) = svc.append_event(&*sess, &mut summary).await {
        let err: BoxError = Box::new(e);
        finish.finish(Some(&*err as &(dyn std::error::Error + 'static)), "");
        degrade("failed to append the summary event", err);
        return;
    }
    finish.finish(None, "");

    // The repair is detached from the turn (spawned, so dropping the turn does
    // not stop it) and bounded by apply::REPAIR_TIMEOUT.
    let repaired = apply::repair(async move {
        match apply::reload_session(Some(svc.as_ref()), &*sess).await {
            Err(err) => tracing::warn!(
                target: "adk",
                "adk: could not re-read the session to check a stored compaction for stragglers: {err}"
            ),
            Ok(latest) => {
                if let Some(mut repair) = apply::repair_after_append(&summary, &before, &*latest) {
                    let added = excluded_len(&repair).saturating_sub(excluded_len(&summary));
                    match svc.append_event(&*sess, &mut repair).await {
                        Err(err) => tracing::warn!(
                            target: "adk",
                            "adk: could not store a corrected compaction record: {err}"
                        ),
                        Ok(()) => tracing::warn!(
                            target: "adk",
                            "adk: corrected a tail-retention record that would have covered {added} event(s) it did not summarize"
                        ),
                    }
                }
            }
        }
    })
    .await;
    if repaired.is_none() {
        tracing::warn!(
            target: "adk",
            "adk: could not re-read the session to check a stored compaction for stragglers: context deadline exceeded"
        );
    }

    rt.mark_compacted();
}

fn excluded_len(ev: &Event) -> usize {
    ev.actions
        .compaction
        .as_ref()
        .map_or(0, |c| c.excluded_events.len())
}

/// Logs the failure and lets the model call proceed.
fn degrade(stage: &str, err: BoxError) {
    tracing::warn!(
        target: "adk",
        "adk: {}; continuing with an uncompacted prompt",
        CompactionError::wrap(stage, err)
    );
}

/// Estimates the prompt `events` would produce, with the agent's
/// resolved mode, the whole session as the call source, and the invocation's user content.
pub(crate) fn prompt_token_estimator(ctx: &InvocationContext) -> Box<TokenCounter> {
    let ctx = ctx.clone();
    Box::new(move |events: &[Event]| -> i64 {
        let agent = ctx.agent();
        if reveal(&*agent).is_none() {
            return 0;
        }
        let all_events = ctx.session().events();
        let is_single_turn = mode_for(&ctx, &*agent) == Mode::SingleTurn;
        match build_contents_default_with_call_source(
            agent.name(),
            &ctx.branch(),
            &ctx.isolation_scope(),
            events,
            &all_events,
            is_single_turn,
            ctx.user_content(),
        ) {
            Ok(contents) => estimate_tokens_from_contents(&contents),
            Err(_) => 0,
        }
    })
}
