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

//! Agent and tool errors, and the agent event stream type (ports of Go `agent.ErrLLMCallsLimitExceeded`,
//! `tool.ErrConfirmationRequired`/`ErrConfirmationRejected`, and the `fmt.Errorf("…: %w")` wrapping in
//! `agent/agent.go`).

use futures::stream::BoxStream;
use gadk_model::ModelError;
use gadk_session::compaction::CompactionError;
use gadk_session::{Event, SessionError};

/// Every agent's event stream; dropping it cancels the work it drives.
pub type EventStream = BoxStream<'static, Result<Event, AgentError>>;

/// Errors produced while running agents, callbacks, plugins and tools.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// A model call failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// A session service or state operation failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// An artifact service operation failed.
    #[error(transparent)]
    Artifact(#[from] gadk_artifact::ArtifactError),
    /// A memory service operation failed.
    #[error(transparent)]
    Memory(#[from] gadk_memory::MemoryError),
    /// A tool failed (Go functiontool: `fmt.Errorf("error tool %q %w", name, err)`).
    #[error("error tool {} {source}", crate::go_fmt::go_quote(.tool))]
    Tool {
        /// The tool's name.
        tool: String,
        /// The tool's error.
        #[source]
        source: ToolError,
    },
    /// A workflow error (carries `gadk_engine::WorkflowError`).
    #[error(transparent)]
    Workflow(Box<dyn std::error::Error + Send + Sync>),
    /// Go `fmt.Errorf("%w: limit is %d", agent.ErrLLMCallsLimitExceeded, max)`
    /// (`internal/agent/runconfig/limits.go`).
    #[error("max number of llm calls exceeded: limit is {limit}")]
    LlmCallsLimitExceeded {
        /// The configured limit.
        limit: i32,
    },
    /// Go `context.Canceled`.
    #[error("context canceled")]
    Cancelled,
    /// Go `context.DeadlineExceeded`.
    #[error("context deadline exceeded")]
    DeadlineExceeded,
    /// Invalid configuration; the text is Go's error text.
    #[error("{0}")]
    Config(String),
    /// Context compaction failed (Go `fmt.Errorf("%w: …", compaction.ErrCompaction, …)`); the
    /// turn's own events are already persisted.
    #[error(transparent)]
    Compaction(CompactionError),
    /// Any other error.
    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl AgentError {
    /// An [`AgentError::Other`] carrying `msg` (Go `errors.New(msg)`).
    pub fn other(msg: impl Into<String>) -> Self {
        AgentError::Other(msg.into().into())
    }

    /// Go `errors.Is(err, compaction.ErrCompaction)`: whether this error, or any error it wraps,
    /// is a [`CompactionError`].
    pub fn is_compaction(&self) -> bool {
        chain_any(self, &|e| {
            e.is::<CompactionError>()
                || matches!(
                    e.downcast_ref::<AgentError>(),
                    Some(AgentError::Compaction(_))
                )
        })
    }
}

/// Go `errors.Is`'s walk: whether `pred` holds for `err` or any error it wraps. Besides
/// `source()`, it steps into the errors [`AgentError::Other`], [`AgentError::Workflow`] and
/// [`ToolError::Execution`] carry as a plain field (their `source()` skips them).
pub(crate) fn chain_any(
    err: &AgentError,
    pred: &dyn Fn(&(dyn std::error::Error + 'static)) -> bool,
) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if pred(e) {
            return true;
        }
        match e.downcast_ref::<AgentError>() {
            Some(AgentError::Other(inner) | AgentError::Workflow(inner)) => {
                cur = Some(&**inner);
                continue;
            }
            Some(AgentError::Tool {
                source: ToolError::Execution(inner),
                ..
            }) => {
                cur = Some(&**inner);
                continue;
            }
            _ => {}
        }
        if let Some(ToolError::Execution(inner)) = e.downcast_ref::<ToolError>() {
            cur = Some(&**inner);
            continue;
        }
        cur = e.source();
    }
    false
}

/// Errors produced by tools.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Go `tool.ErrConfirmationRequired`.
    #[error("requires confirmation, please approve or reject")]
    ConfirmationRequired,
    /// Go `tool.ErrConfirmationRejected`.
    #[error("call is rejected")]
    ConfirmationRejected,
    /// Invalid arguments or tool usage; the text is Go's error text.
    #[error("{0}")]
    InvalidArgument(String),
    /// A tool panicked; the text describes the panic.
    #[error("{0}")]
    Panic(String),
    /// The tool's own execution failed.
    #[error(transparent)]
    Execution(Box<dyn std::error::Error + Send + Sync>),
}

/// A shared [`ToolError`] carried as an error value, for callbacks that hand the original tool
/// error back unchanged (e.g. as `AgentError::Other(Box::new(SharedToolError(err)))`), where Go
/// returns the same `error` value. Display-transparent: it writes the tool error's text, and
/// `source()` is the tool error's own source. The `Arc` is kept, so callers can check identity
/// with `Arc::ptr_eq` after a downcast.
#[derive(Debug, Clone)]
pub struct SharedToolError(pub std::sync::Arc<ToolError>);

impl std::fmt::Display for SharedToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.0)
    }
}

/// Go `fmt.Errorf("<msg>: %w", err)`: Display `"<msg>: <source>"`, `source()` = the wrapped error.
#[derive(Debug)]
pub(crate) struct Wrapped {
    msg: String,
    source: AgentError,
}

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Wraps `source` as Go's `fmt.Errorf("<msg>: %w", source)` does.
pub(crate) fn wrap(msg: impl Into<String>, source: AgentError) -> AgentError {
    AgentError::Other(Box::new(Wrapped {
        msg: msg.into(),
        source,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_session::compaction::CompactionError;

    use super::*;

    #[test]
    fn compaction_error_displays_go_text_and_is_compaction() {
        let e = AgentError::Compaction(CompactionError::new("post-invocation: boom"));
        assert_eq!(
            e.to_string(),
            "context compaction failed: post-invocation: boom"
        );
        assert!(e.to_string().starts_with("context compaction failed: "));
        assert!(e.is_compaction());
    }

    #[test]
    fn shared_tool_error_is_display_transparent_and_keeps_the_arc() {
        use std::error::Error as _;
        use std::sync::Arc;

        let plain = Arc::new(ToolError::InvalidArgument("bad args".into()));
        let shared = SharedToolError(plain.clone());
        assert_eq!(shared.to_string(), "bad args");
        assert!(shared.source().is_none(), "InvalidArgument has no source");

        // Execution is transparent, so the source is the boxed error's own source.
        let inner = Wrapped {
            msg: "outer".into(),
            source: AgentError::other("root cause"),
        };
        let exec = Arc::new(ToolError::Execution(Box::new(inner)));
        let shared_exec = SharedToolError(exec.clone());
        assert_eq!(shared_exec.to_string(), "outer: root cause");
        assert_eq!(
            shared_exec.source().map(|s| s.to_string()).as_deref(),
            Some("root cause")
        );

        // Boxed as an AgentError, the original Arc is recoverable for an identity check.
        let agent_err = AgentError::Other(Box::new(SharedToolError(plain.clone())));
        assert_eq!(agent_err.to_string(), "bad args");
        let AgentError::Other(boxed) = &agent_err else {
            panic!("not Other: {agent_err:?}")
        };
        let got = boxed.downcast_ref::<SharedToolError>().unwrap();
        assert!(Arc::ptr_eq(&got.0, &plain));
        assert!(!Arc::ptr_eq(&got.0, &exec));
        let cloned = shared.clone();
        assert!(Arc::ptr_eq(&cloned.0, &shared.0));
    }

    #[test]
    fn is_compaction_walks_the_chain() {
        // Wrapped by Go-style fmt.Errorf("…: %w") (Wrapped's source is the AgentError).
        let wrapped = wrap("outer", AgentError::Compaction(CompactionError::new("x")));
        assert!(wrapped.is_compaction());
        // Boxed inside Other / Workflow.
        assert!(AgentError::Other(Box::new(CompactionError::new("x"))).is_compaction());
        assert!(AgentError::Workflow(Box::new(CompactionError::new("x"))).is_compaction());
        // Not a compaction failure.
        assert!(!AgentError::other("context compaction failed: lookalike").is_compaction());
        assert!(!AgentError::Cancelled.is_compaction());
        assert!(!wrap("outer", AgentError::Cancelled).is_compaction());
    }
}
