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
//! Rendering a prompt as an agent instruction.

use std::sync::Arc;

use gadk_agent::{AgentError, PromptUse, ReadonlyContext};
use gadk_engine::llm::InstructionProvider;
use tracing::Instrument;

use crate::model::Selector;
use crate::registry::{PromptRegistry, resolve_for};
use crate::store::StoreError;

/// A prompt bound to a selector, rendered on every call against the registry's current
/// snapshot (so a label moved mid-session applies from the next model call) with session-state
/// templating (`{var}`, `{var?}`, `{artifact.x}`). A label's rollout is applied per session
/// (see [`PromptRegistry::get_for`]). Each render records a `render_prompt <name>` span with
/// `prompt.name`, `prompt.version`, `prompt.label`, `prompt.hash`, `prompt.fallback` and
/// `prompt.rollout` (never the text), and records the version it used in the invocation's
/// context (see [`render`](Self::render)).
#[derive(Clone)]
pub struct PromptInstruction {
    registry: PromptRegistry,
    name: String,
    sel: Selector,
    fallback: Option<String>,
}

impl std::fmt::Debug for PromptInstruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptInstruction")
            .field("name", &self.name)
            .field("sel", &self.sel)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl PromptInstruction {
    pub(crate) fn new(registry: PromptRegistry, name: String, sel: Selector) -> Self {
        Self {
            registry,
            name,
            sel,
            fallback: None,
        }
    }

    /// Text to render (with the same templating) when the prompt, label or version is not
    /// found. A template that fails to render is still an error.
    pub fn with_fallback(mut self, text: impl Into<String>) -> Self {
        self.fallback = Some(text.into());
        self
    }

    /// This prompt as an [`InstructionProvider`] for `LlmAgentConfig::instruction_provider` or
    /// `global_instruction_provider`.
    pub fn into_provider(self) -> InstructionProvider {
        let this = Arc::new(self);
        Arc::new(move |ctx: ReadonlyContext| {
            let this = this.clone();
            Box::pin(async move { this.render(&ctx).await })
        })
    }

    fn err(&self, msg: impl std::fmt::Display) -> AgentError {
        AgentError::other(format!("prompts: {}@{}: {msg}", self.name, self.sel))
    }

    /// Resolves and renders the prompt for `ctx`.
    ///
    /// The version is resolved with [`PromptRegistry::get_for`] keyed by `ctx`'s session id, so
    /// a session stays on one side of a label's rollout; the span's `prompt.rollout` is true
    /// when the rollout version was served.
    ///
    /// The version served is recorded for `ctx`'s agent with
    /// [`ReadonlyContext::record_prompt`] as `PromptUse { name, version, label }`, where `label`
    /// is the selector's label, `latest` for [`Selector::Latest`], or none for
    /// [`Selector::Version`] (the span's `prompt.label`); a fallback render clears the agent's
    /// record. Analytics plugins read the record to attribute model calls to prompt versions.
    /// When an agent renders more than one `PromptInstruction` (instruction and global
    /// instruction), the last render before the model call wins.
    ///
    /// # Errors
    ///
    /// Not found without a fallback, or a template that fails to render; messages start with
    /// `prompts: <name>@<selector>: `.
    pub async fn render(&self, ctx: &ReadonlyContext) -> Result<String, AgentError> {
        let label = match &self.sel {
            Selector::Label(l) => l.as_str().to_owned(),
            Selector::Latest => "latest".to_owned(),
            Selector::Version(_) => String::new(),
        };
        let resolved = {
            let loaded = self.registry.inner.loaded.load();
            resolve_for(
                &loaded.0,
                &loaded.1,
                &self.name,
                &self.sel,
                ctx.session_id(),
            )
        };
        let (text, version, hash, fallback, rollout) = match resolved {
            Ok((v, rollout)) => {
                let mut used = PromptUse::new(self.name.clone(), v.version);
                if !label.is_empty() {
                    used = used.with_label(label.clone());
                }
                ctx.record_prompt(used);
                (
                    v.template.clone(),
                    i64::from(v.version),
                    v.meta.hash.clone(),
                    false,
                    rollout,
                )
            }
            Err(e @ StoreError::NotFound { .. }) => match &self.fallback {
                Some(fb) => {
                    ctx.clear_recorded_prompt();
                    (fb.clone(), 0, String::new(), true, false)
                }
                None => return Err(self.err(e)),
            },
            Err(e) => return Err(self.err(e)),
        };
        let span = tracing::info_span!(
            "render_prompt",
            otel.name = %format!("render_prompt {}", self.name),
            prompt.name = %self.name,
            prompt.version = version,
            prompt.label = %label,
            prompt.hash = %hash,
            prompt.fallback = fallback,
            prompt.rollout = rollout,
        );
        gadk_engine::llm::inject_session_state(ctx, &text)
            .instrument(span)
            .await
            .map_err(|e| self.err(e))
    }
}

impl From<PromptInstruction> for InstructionProvider {
    fn from(p: PromptInstruction) -> Self {
        p.into_provider()
    }
}
