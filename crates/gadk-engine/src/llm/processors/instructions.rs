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

//! Instruction request processor and session-state templating (port of Go
//! internal/llminternal/instruction_processor.go).

use std::sync::{Arc, LazyLock};

use futures::future::BoxFuture;
use gadk_agent::{Agent, AgentError, InvocationContext};
use gadk_artifact::ArtifactError;
use gadk_model::LlmRequest;
use gadk_session::{Event, SessionError};
use serde_json::Value;

use crate::{
    llm::{
        flow::Flow,
        processors::{go_sprint, wrap_err},
        state::{LlmAgentState, reveal},
    },
    tool::utils::append_instructions,
};

const APP_PREFIX: &str = "app:";
const USER_PREFIX: &str = "user:";
const TEMP_PREFIX: &str = "temp:";

/// The regex to find placeholders like {variable} or {artifact.file_name}.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static PLACEHOLDER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\{+[^{}]*\}+").expect("valid placeholder regex"));

/// A placeholder directly after one of these bytes is literal text ("${x}", "\{x}").
const LITERAL_PLACEHOLDER_PREFIXES: &[u8] = b"$\\";

/// Port of instruction_processor.go:43 instructionsRequestProcessor.
pub(crate) fn instructions_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Ok(Vec::new());
        };
        // Go: rootAgent := asLLMAgent(parents.RootAgent(ctx.Agent())); fall back to the agent
        // itself.
        let root = root_agent(ctx, &agent);
        let root_state = reveal(&*root).unwrap_or(state);
        append_global_instructions(ctx, req, root_state)
            .await
            .map_err(|e| wrap_err("failed to append global instructions", e))?;
        append_agent_instructions(ctx, req, state)
            .await
            .map_err(|e| wrap_err("failed to append instructions", e))?;
        Ok(Vec::new())
    })
}

/// Go parentmap.Map.RootAgent: walk parent links by name until none; no parent map → the agent
/// itself.
fn root_agent(ctx: &InvocationContext, agent: &Arc<dyn Agent>) -> Arc<dyn Agent> {
    match ctx.parent_map() {
        Some(pm) => pm.root_agent(agent),
        None => agent.clone(),
    }
}

/// Port of instruction_processor.go:81 appendInstructions. The provider result is not templated.
async fn append_agent_instructions(
    ctx: &InvocationContext,
    req: &mut LlmRequest,
    state: &LlmAgentState,
) -> Result<(), AgentError> {
    if let Some(provider) = &state.cfg.instruction_provider {
        let instruction = provider(ctx.as_readonly().clone())
            .await
            .map_err(|e| wrap_err("failed to evaluate instruction provider", e))?;
        append_instructions(req, &[instruction.as_str()]);
        return Ok(());
    }
    if state.cfg.instruction.is_empty() {
        return Ok(());
    }
    let inst = inject_session_state(ctx, &state.cfg.instruction)
        .await
        .map_err(|e| wrap_err("failed to inject session state into instruction", e))?;
    append_instructions(req, &[inst.as_str()]);
    Ok(())
}

/// Port of instruction_processor.go:105 appendGlobalInstructions. The provider result is not
/// templated.
async fn append_global_instructions(
    ctx: &InvocationContext,
    req: &mut LlmRequest,
    state: &LlmAgentState,
) -> Result<(), AgentError> {
    if let Some(provider) = &state.cfg.global_instruction_provider {
        let instruction = provider(ctx.as_readonly().clone())
            .await
            .map_err(|e| wrap_err("failed to evaluate global instruction provider", e))?;
        append_instructions(req, &[instruction.as_str()]);
        return Ok(());
    }
    if state.cfg.global_instruction.is_empty() {
        return Ok(());
    }
    let inst = inject_session_state(ctx, &state.cfg.global_instruction)
        .await
        .map_err(|e| wrap_err("failed to inject session state into global instruction", e))?;
    append_instructions(req, &[inst.as_str()]);
    Ok(())
}

/// Port of instruction_processor.go:211 InjectSessionState: populates values in an instruction
/// template from the context.
pub(crate) async fn inject_session_state(
    ctx: &InvocationContext,
    template: &str,
) -> Result<String, AgentError> {
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for m in PLACEHOLDER.find_iter(template) {
        let (start, end) = (m.start(), m.end());
        // Leave literal placeholders in place; they are copied with the surrounding text.
        if start > 0 && LITERAL_PLACEHOLDER_PREFIXES.contains(&template.as_bytes()[start - 1]) {
            continue;
        }
        out.push_str(&template[last..start]);
        out.push_str(&replace_match(ctx, m.as_str()).await?);
        last = end;
    }
    out.push_str(&template[last..]);
    Ok(out)
}

/// Port of instruction_processor.go:130 replaceMatch.
pub(crate) async fn replace_match(ctx: &InvocationContext, m: &str) -> Result<String, AgentError> {
    // Go strings.TrimSpace(strings.Trim(match, "{}")).
    let var = m.trim_matches(|c| c == '{' || c == '}').trim();
    let (var, optional) = match var.strip_suffix('?') {
        Some(v) => (v, true),
        None => (var, false),
    };
    if let Some(file) = var.strip_prefix("artifact.") {
        let Some(arts) = ctx.artifacts() else {
            return Err(AgentError::other("artifact service is not initialized"));
        };
        return match arts.load(file).await {
            Ok(resp) => Ok(resp.part.map(|p| p.text).unwrap_or_default()),
            Err(e) if optional && is_artifact_not_found(&e) => Ok(String::new()),
            Err(e) => Err(wrap_err(format!("failed to load artifact {file}"), e)),
        };
    }
    if !is_valid_state_name(var) {
        // Return the original string if not a valid name.
        return Ok(m.to_owned());
    }
    let session = ctx.session();
    match session.state().get(var) {
        // Go: value == nil → "".
        Ok(Value::Null) => Ok(String::new()),
        // Go fmt.Sprintf("%v", value).
        Ok(v) => Ok(go_sprint(&v)),
        Err(SessionError::StateKeyNotExist) if optional => Ok(String::new()),
        // Unwrapped, as Go returns err as is.
        Err(e) => Err(AgentError::Session(e)),
    }
}

/// Go errors.Is(err, fs.ErrNotExist).
fn is_artifact_not_found(e: &AgentError) -> bool {
    // `AgentError::Artifact` is `#[error(transparent)]`, which forwards `source()` to the inner
    // ArtifactError's own source, so a chain walk never yields the ArtifactError itself; check
    // the direct variant first.
    if matches!(e, AgentError::Artifact(ArtifactError::NotFound)) {
        return true;
    }
    // Wrapped forms. `AgentError::Other` has no `source()`, so start from its payload.
    let mut cur: Option<&(dyn std::error::Error + 'static)> = match e {
        AgentError::Other(inner) | AgentError::Workflow(inner) => Some(&**inner),
        other => Some(other),
    };
    while let Some(err) = cur {
        if matches!(
            err.downcast_ref::<ArtifactError>(),
            Some(ArtifactError::NotFound)
        ) {
            return true;
        }
        if matches!(
            err.downcast_ref::<AgentError>(),
            Some(AgentError::Artifact(ArtifactError::NotFound))
        ) {
            return true;
        }
        cur = err.source();
    }
    false
}

/// Port of instruction_processor.go:175 isIdentifier (Python `str.isidentifier()`).
///
/// Go's `unicode.IsLetter`/`IsDigit` are categories L and Nd; Rust's `char::is_alphabetic` and
/// `char::is_numeric` are slightly broader for rare scripts (Other_Alphabetic, Nl/No).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_alphabetic() || c.is_numeric() || c == '_')
}

/// Port of instruction_processor.go:194 isValidStateName.
fn is_valid_state_name(var: &str) -> bool {
    let parts: Vec<&str> = var.split(':').collect();
    match parts.as_slice() {
        [_] => is_identifier(var),
        [prefix, name] => {
            let prefix = format!("{prefix}:");
            [APP_PREFIX, USER_PREFIX, TEMP_PREFIX].contains(&prefix.as_str()) && is_identifier(name)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use gadk_agent::InvocationContext;
    use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn literal_dollar_brace_is_kept() {
        let svc = InMemorySessionService::new();
        let sess = svc
            .create(CreateRequest {
                app_name: "a".into(),
                user_id: "u".into(),
                session_id: Some("s".into()),
                state: json!({"x": "X", "y": 1.5}).as_object().cloned(),
            })
            .await
            .unwrap();
        let ctx = InvocationContext::builder()
            .session(sess)
            .build_invocation();
        assert_eq!(
            inject_session_state(&ctx, "${x} \\{x} {x} {y} { x }")
                .await
                .unwrap(),
            "${x} \\{x} X 1.5 X"
        );
    }

    #[test]
    fn state_names() {
        assert!(is_valid_state_name("user:name"));
        assert!(is_valid_state_name("_a1"));
        assert!(!is_valid_state_name("1a"));
        assert!(!is_valid_state_name("bad:name"));
        assert!(!is_valid_state_name("a:b:c"));
        assert!(!is_valid_state_name(""));
    }
}
