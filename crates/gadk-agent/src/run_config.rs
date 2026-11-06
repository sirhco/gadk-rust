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

//! Run configuration (port of Go `agent/run_config.go` and `internal/agent/runconfig`).

use crate::live::LiveRunConfig;

/// Go `agent.StreamingMode` (`"none"` | `"sse"` | `"bidi"`); `Bidi` is set by the runner for live
/// runs (Go `runconfig.StreamingModeBidi`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StreamingMode {
    /// No streaming.
    #[default]
    None,
    /// Server-sent events streaming.
    Sse,
    /// Bidirectional (live) streaming.
    Bidi,
}

/// Configuration of one agent run (Go `agent.RunConfig`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunConfig {
    /// The streaming mode.
    pub streaming_mode: StreamingMode,
    /// Whether input blobs are saved as artifacts.
    pub save_input_blobs_as_artifacts: bool,
    /// The maximum number of LLM calls per invocation; zero takes the default (or
    /// `ADK_MAX_LLM_CALLS`), a negative value is unlimited.
    pub max_llm_calls: i32,
    /// The live run configuration (Go `runconfig.RunConfig.Live`); set only for live runs.
    pub live: Option<LiveRunConfig>,
}

/// Port of Go `internal/agent/runconfig` (`limits.go`, `run_config.go`).
pub mod run_limits {
    use std::sync::atomic::{AtomicI64, Ordering};

    use crate::error::AgentError;

    /// Go `DefaultMaxLLMCalls`.
    pub const DEFAULT_MAX_LLM_CALLS: i32 = 500;
    /// Go `MaxLLMCallsEnvVar`.
    pub const MAX_LLM_CALLS_ENV_VAR: &str = "ADK_MAX_LLM_CALLS";

    /// Go `ResolveMaxLLMCalls`: as [`resolve_max_llm_calls_with`], reading `ADK_MAX_LLM_CALLS`.
    pub fn resolve_max_llm_calls(configured: i32) -> i32 {
        resolve_max_llm_calls_with(
            configured,
            std::env::var(MAX_LLM_CALLS_ENV_VAR).ok().as_deref(),
        )
    }

    /// Pure form of Go `ResolveMaxLLMCalls`: a non-zero value passes through; zero takes `env`
    /// parsed as an integer (Go `strconv.Atoi`), else [`DEFAULT_MAX_LLM_CALLS`].
    pub fn resolve_max_llm_calls_with(configured: i32, env: Option<&str>) -> i32 {
        if configured != 0 {
            return configured;
        }
        env.and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or(DEFAULT_MAX_LLM_CALLS)
    }

    /// Go `runconfig.RunConfig`'s resolved `MaxLLMCalls` plus its atomic `llmCalls` counter: one per
    /// invocation, shared by every agent in it.
    #[derive(Debug)]
    pub struct LlmCallLimiter {
        max_llm_calls: i32,
        calls: AtomicI64,
    }

    impl LlmCallLimiter {
        /// A limiter allowing `max_llm_calls` calls; non-positive means unlimited.
        pub fn new(max_llm_calls: i32) -> Self {
            Self {
                max_llm_calls,
                calls: AtomicI64::new(0),
            }
        }

        /// Go `RunConfig.MaxLLMCalls`.
        pub fn max_llm_calls(&self) -> i32 {
            self.max_llm_calls
        }

        /// Go `IncrementAndEnforceLLMCallsLimit`: count first, then check; non-positive is unlimited.
        pub fn increment_and_enforce(&self) -> Result<(), AgentError> {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            if self.max_llm_calls <= 0 || calls <= i64::from(self.max_llm_calls) {
                return Ok(());
            }
            Err(AgentError::LlmCallsLimitExceeded {
                limit: self.max_llm_calls,
            })
        }
    }
}
