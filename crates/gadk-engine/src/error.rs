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

//! Workflow errors (port of ADK-Go `workflow/errors.go` and the sentinel errors in
//! `workflow/validation.go`, `workflow/scheduler.go` and `workflow/resume.go`).

use std::sync::Arc;

use gadk_agent::AgentError;

/// Port of the ADK-Go workflow sentinel errors (`workflow.Err*`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowErrorKind {
    /// Go `ErrNodeFailed`.
    NodeFailed,
    /// Go `ErrNodeInterrupted`.
    NodeInterrupted,
    /// Go `ErrNodeWaitingForOutput` (wraps `ErrNodeInterrupted`).
    NodeWaitingForOutput,
    /// Go `ErrInvalidRunNodeContext`.
    InvalidRunNodeContext,
    /// Go `ErrInvalidRunID`.
    InvalidRunId,
    /// Go `ErrParallelHITLUnsupported`.
    ParallelHitlUnsupported,
    /// Go `ErrInputValidation`.
    InputValidation,
    /// Output validation failed (no Go sentinel; Go wraps `ErrNodeFailed`, base_node.go).
    OutputValidation,
    /// Go `ErrOutputAlreadyDelegated`.
    OutputAlreadyDelegated,
    /// Go `ErrMultipleOutputs`.
    MultipleOutputs,
    /// Go `ErrMultipleRoutingEvents`.
    MultipleRoutingEvents,
    /// Go `ErrMultipleTerminalOutputs`.
    MultipleTerminalOutputs,
    /// Go `ErrDuplicateNodeName`.
    DuplicateNodeName,
    /// Go `ErrNoStartNode`.
    NoStartNode,
    /// Go `ErrNodePointsToStart`.
    NodePointsToStart,
    /// Go `ErrDuplicateEdge`.
    DuplicateEdge,
    /// Go `ErrMultipleDefaultRoutes`.
    MultipleDefaultRoutes,
    /// Go `ErrNodesNotReachable`.
    NodesNotReachable,
    /// Go `ErrUnconditionalCycle`.
    UnconditionalCycle,
    /// Go `ErrSubWorkflowNameCollision`.
    SubWorkflowNameCollision,
    /// Go `ErrUnsupportedFanIn`.
    UnsupportedFanIn,
    /// Go `ErrInvalidResumeResponse`.
    InvalidResumeResponse,
    /// Go `ErrNothingToResume`.
    NothingToResume,
    /// Retries were exhausted (Rust-only kind).
    RetryExhausted,
    /// A persistence operation failed (Rust-only kind).
    Persistence,
    /// Any other workflow error (Rust-only kind).
    Other,
}

/// A workflow error: a sentinel kind, Go's full `err.Error()` text, and an optional cause.
#[derive(Debug, Clone)]
pub struct WorkflowError {
    kind: WorkflowErrorKind,
    message: String,
    source: Option<Arc<AgentError>>,
    node_run: Option<NodeRunError>,
}

/// Port of Go `workflow.NodeRunError`: a dynamic child run failed. `cause: None` is Go's nil `Cause`.
#[derive(Debug, Clone)]
pub struct NodeRunError {
    /// The child node's name.
    pub child_name: String,
    /// The child node's path (`<parent>/<child>@<run_id>`); may be empty.
    pub child_path: String,
    /// The child run's id.
    pub run_id: String,
    /// The error the child run failed with.
    pub cause: Option<Arc<AgentError>>,
}

impl WorkflowError {
    /// A workflow error of `kind` whose text is `message` (Go's full `err.Error()` text, built by the
    /// caller, e.g. `"<sentinel>: <detail>"` for Go `fmt.Errorf("%w: %s", ErrX, detail)`).
    pub fn new(kind: WorkflowErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            node_run: None,
        }
    }

    /// Sets the wrapped cause (Go `%w`); the message is unchanged, since the caller already formatted it.
    pub fn with_source(mut self, source: AgentError) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    /// The error's kind.
    pub fn kind(&self) -> WorkflowErrorKind {
        self.kind
    }

    /// Go `errors.Is(err, ErrX)` over the `%w` chains of errors.go and base_node.go.
    pub fn is(&self, kind: WorkflowErrorKind) -> bool {
        self.kind == kind
            || (self.kind == WorkflowErrorKind::NodeWaitingForOutput
                && kind == WorkflowErrorKind::NodeInterrupted)
            || (self.kind == WorkflowErrorKind::OutputValidation
                && kind == WorkflowErrorKind::NodeFailed)
            || self
                .source
                .as_deref()
                .and_then(workflow_error)
                .is_some_and(|w| w.is(kind))
    }

    /// The verbatim Go `.Error()` text of the sentinel for `kind`.
    pub fn sentinel_text(kind: WorkflowErrorKind) -> &'static str {
        use WorkflowErrorKind as K;
        match kind {
            K::NodeFailed => "workflow: dynamic child failed",
            K::NodeInterrupted => "workflow: dynamic child interrupted",
            K::NodeWaitingForOutput => "workflow: dynamic child interrupted: waiting for output",
            K::InvalidRunNodeContext => "workflow: RunNode called outside a dynamic node",
            K::InvalidRunId => "workflow: invalid run id",
            K::ParallelHitlUnsupported => "workflow: parallel HITL is not supported",
            K::InputValidation => "workflow: input validation failed",
            K::OutputValidation => "output validation failed",
            K::OutputAlreadyDelegated => "workflow: parent already has a use_as_output child",
            K::MultipleOutputs => {
                "workflow: node produced multiple events with output values; only one event per execution can carry output"
            }
            K::MultipleRoutingEvents => {
                "workflow: node produced multiple events with route tags; only one event per execution can specify routes"
            }
            K::MultipleTerminalOutputs => {
                "workflow: multiple terminal nodes produced output; a workflow must have at most one terminal output"
            }
            K::DuplicateNodeName => "duplicate node name",
            K::NoStartNode => "no start node found",
            K::NodePointsToStart => "node points to start node",
            K::DuplicateEdge => "duplicate edge",
            K::MultipleDefaultRoutes => "node has more than one default route",
            K::NodesNotReachable => "nodes not reachable from start node",
            K::UnconditionalCycle => "unconditional cycle detected",
            K::SubWorkflowNameCollision => "sub-workflow name collision",
            K::UnsupportedFanIn => "non-JoinNode fan-in is not yet supported",
            K::InvalidResumeResponse => "workflow: resume response does not match request schema",
            K::NothingToResume => "workflow: no waiting node matched the supplied responses",
            K::RetryExhausted => "workflow: retries exhausted",
            K::Persistence => "workflow: persistence error",
            K::Other => "workflow: error",
        }
    }

    /// The bare sentinel error of `kind` (Go `workflow.ErrX`).
    pub fn sentinel(kind: WorkflowErrorKind) -> Self {
        Self::new(kind, Self::sentinel_text(kind))
    }

    /// The wrapped cause (Go `Unwrap`).
    pub fn cause(&self) -> Option<&AgentError> {
        self.source.as_deref()
    }

    /// Port of Go `&NodeRunError{..}` (`Error`/`Unwrap`): the kind is the cause's workflow kind, else
    /// `NodeFailed`.
    pub fn node_run(info: NodeRunError) -> Self {
        // Spans name this error's Go type (`error.type`), which gadk-agent cannot see.
        gadk_agent::agent::telemetry::set_go_error_type_resolver(go_error_type);
        let path = if info.child_path.is_empty() {
            &info.child_name
        } else {
            &info.child_path
        };
        let message = match &info.cause {
            Some(cause) => format!("workflow: dynamic child {path}: {cause}"),
            None => format!("workflow: dynamic child {path}: <nil cause>"),
        };
        let kind = info
            .cause
            .as_deref()
            .and_then(workflow_error)
            .map_or(WorkflowErrorKind::NodeFailed, |w| w.kind());
        Self {
            kind,
            message,
            source: info.cause.clone(),
            node_run: Some(info),
        }
    }

    /// The dynamic child run failure this error reports (Go `errors.As(err, *NodeRunError)`).
    pub fn node_run_error(&self) -> Option<&NodeRunError> {
        self.node_run.as_ref()
    }
}

/// The Go `%T` of a [`WorkflowError`] reporting a dynamic child run failure
/// (`*workflow.NodeRunError`); other errors are not named.
fn go_error_type(err: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    err.downcast_ref::<WorkflowError>()
        .and_then(WorkflowError::node_run_error)
        .map(|_| "*workflow.NodeRunError")
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<WorkflowError> for AgentError {
    fn from(e: WorkflowError) -> Self {
        AgentError::Workflow(Box::new(e))
    }
}

/// The [`WorkflowError`] carried by `err`, if any (`AgentError::Workflow` or `AgentError::Other`).
pub fn workflow_error(err: &AgentError) -> Option<&WorkflowError> {
    match err {
        AgentError::Workflow(b) | AgentError::Other(b) => b.downcast_ref::<WorkflowError>(),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_output_is_also_interrupted() {
        let e = WorkflowError::sentinel(WorkflowErrorKind::NodeWaitingForOutput);
        assert!(e.is(WorkflowErrorKind::NodeWaitingForOutput));
        assert!(e.is(WorkflowErrorKind::NodeInterrupted));
        assert!(!e.is(WorkflowErrorKind::NodeFailed));
        assert_eq!(
            e.to_string(),
            "workflow: dynamic child interrupted: waiting for output"
        );
    }

    #[test]
    fn output_validation_is_also_node_failed() {
        let e = WorkflowError::new(WorkflowErrorKind::OutputValidation, "x");
        assert!(e.is(WorkflowErrorKind::NodeFailed));
    }

    #[test]
    fn is_walks_the_source_chain() {
        let inner = WorkflowError::sentinel(WorkflowErrorKind::InputValidation);
        let outer =
            WorkflowError::new(WorkflowErrorKind::NodeFailed, "outer").with_source(inner.into());
        assert!(outer.is(WorkflowErrorKind::InputValidation));
        assert!(std::error::Error::source(&outer).is_some());
    }

    fn cause(kind: WorkflowErrorKind) -> Option<Arc<AgentError>> {
        Some(Arc::new(WorkflowError::sentinel(kind).into()))
    }

    #[test]
    fn node_run_formats_like_go() {
        let e = WorkflowError::node_run(NodeRunError {
            child_name: "fixer".into(),
            child_path: "code_workflow/fixer@2".into(),
            run_id: "2".into(),
            cause: cause(WorkflowErrorKind::NodeFailed),
        });
        assert_eq!(
            e.to_string(),
            "workflow: dynamic child code_workflow/fixer@2: workflow: dynamic child failed"
        );
        assert!(e.is(WorkflowErrorKind::NodeFailed));
        assert_eq!(e.node_run_error().unwrap().run_id, "2");
        assert_eq!(
            e.cause().unwrap().to_string(),
            "workflow: dynamic child failed"
        );
        let by_name = WorkflowError::node_run(NodeRunError {
            child_name: "fixer".into(),
            child_path: String::new(),
            run_id: String::new(),
            cause: cause(WorkflowErrorKind::NodeInterrupted),
        });
        assert_eq!(
            by_name.to_string(),
            "workflow: dynamic child fixer: workflow: dynamic child interrupted"
        );
        assert_eq!(by_name.kind(), WorkflowErrorKind::NodeInterrupted);
        // Go "<nil cause>": no source, kind falls back to NodeFailed.
        let nil_cause = WorkflowError::node_run(NodeRunError {
            child_name: String::new(),
            child_path: "x/y@1".into(),
            run_id: String::new(),
            cause: None,
        });
        assert_eq!(
            nil_cause.to_string(),
            "workflow: dynamic child x/y@1: <nil cause>"
        );
        assert_eq!(nil_cause.kind(), WorkflowErrorKind::NodeFailed);
        assert!(nil_cause.cause().is_none());
        // A non-workflow cause also falls back to NodeFailed.
        let plain = WorkflowError::node_run(NodeRunError {
            child_name: "c".into(),
            child_path: String::new(),
            run_id: String::new(),
            cause: Some(Arc::new(AgentError::other("boom"))),
        });
        assert_eq!(plain.kind(), WorkflowErrorKind::NodeFailed);
        assert_eq!(plain.to_string(), "workflow: dynamic child c: boom");
    }

    #[test]
    fn rust_only_sentinel_texts() {
        assert_eq!(
            WorkflowError::sentinel_text(WorkflowErrorKind::OutputValidation),
            "output validation failed"
        );
        assert_eq!(
            WorkflowError::sentinel_text(WorkflowErrorKind::RetryExhausted),
            "workflow: retries exhausted"
        );
        assert_eq!(
            WorkflowError::sentinel_text(WorkflowErrorKind::Persistence),
            "workflow: persistence error"
        );
        assert_eq!(
            WorkflowError::sentinel_text(WorkflowErrorKind::Other),
            "workflow: error"
        );
    }

    #[test]
    fn workflow_error_downcasts_from_agent_error() {
        let a: AgentError = WorkflowError::sentinel(WorkflowErrorKind::NoStartNode).into();
        assert!(matches!(a, AgentError::Workflow(_)));
        assert_eq!(
            workflow_error(&a).unwrap().kind(),
            WorkflowErrorKind::NoStartNode
        );
        assert_eq!(a.to_string(), "no start node found");
        assert!(workflow_error(&AgentError::Cancelled).is_none());
    }
}
