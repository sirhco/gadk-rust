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

//! `BaseNode` (port of Go workflow/base_node.go).

use std::sync::Arc;

use gadk_agent::AgentError;
use gadk_genai::Content;
use gadk_session::Event;
use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::CompiledSchema;
use crate::workflow::route::go_quote;
use crate::workflow::validation::default_validate_input;
use crate::workflow::{Node, NodeConfig};

/// Port of Go workflow.BaseNode: the identity, configuration and schemas shared by every node
/// kind. Custom node types hold a `BaseNode` and delegate the [`Node`] metadata methods to it.
#[derive(Clone)]
pub struct BaseNode {
    name: String,
    description: String,
    config: NodeConfig,
    input: Option<Arc<CompiledSchema>>,
    output: Option<Arc<CompiledSchema>>,
}

/// Compiles an optional JSON schema (Go `jsonschema.Schema.Resolve`).
pub(crate) fn compile_schema(schema: Option<Value>) -> Result<Option<Arc<CompiledSchema>>, String> {
    match schema {
        None => Ok(None),
        Some(v) => CompiledSchema::new(v).map(|c| Some(Arc::new(c))),
    }
}

impl BaseNode {
    /// Port of Go NewBaseNodeWithSchemas (base_node.go:42) for already-compiled schemas.
    pub(crate) fn from_compiled(
        name: &str,
        description: &str,
        cfg: NodeConfig,
        input: Option<Arc<CompiledSchema>>,
        output: Option<Arc<CompiledSchema>>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            config: cfg,
            input,
            output,
        }
    }

    /// Port of Go NewBaseNodeWithSchemas (base_node.go:42): a `BaseNode` with the given identity,
    /// configuration, and JSON schemas. Fails when a schema does not compile.
    pub fn with_schemas(
        name: &str,
        description: &str,
        input: Option<Value>,
        output: Option<Value>,
        cfg: NodeConfig,
    ) -> Result<Self, WorkflowError> {
        let input = compile_schema(input).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!("resolving input schema for node {}: {e}", go_quote(name)),
            )
        })?;
        let output = compile_schema(output).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!("resolving output schema for node {}: {e}", go_quote(name)),
            )
        })?;
        Ok(Self::from_compiled(name, description, cfg, input, output))
    }

    /// Port of Go NewBaseNode (base_node.go:68): a `BaseNode` with no schemas.
    pub fn new(name: &str, description: &str, cfg: NodeConfig) -> Self {
        Self::from_compiled(name, description, cfg, None, None)
    }

    /// The node's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The node's human-readable description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The node's configuration.
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    /// The node's input validation schema.
    pub fn input_schema(&self) -> Option<&Value> {
        self.input.as_deref().map(CompiledSchema::schema)
    }

    /// The node's output validation schema.
    pub fn output_schema(&self) -> Option<&Value> {
        self.output.as_deref().map(CompiledSchema::schema)
    }

    /// The compiled input schema.
    pub(crate) fn input_compiled(&self) -> Option<&CompiledSchema> {
        self.input.as_deref()
    }

    /// The compiled output schema.
    pub(crate) fn output_compiled(&self) -> Option<&CompiledSchema> {
        self.output.as_deref()
    }

    /// Port of Go BaseNode.ValidateInput (base_node.go:88): validates and coerces the input using
    /// the node's input schema. The error message is the raw validator text.
    pub fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        default_validate_input(v, self.input_compiled())
            .map_err(|m| WorkflowError::new(WorkflowErrorKind::InputValidation, m))
    }

    /// Port of Go BaseNode.ValidateOutput (base_node.go:93): validates the output against the
    /// node's output schema. The error message is the raw validator text.
    pub fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        default_validate_output(v, self.output_compiled())
            .map_err(|m| WorkflowError::new(WorkflowErrorKind::OutputValidation, m))
    }
}

/// Port of Go defaultValidateOutput (base_node.go:104). When direct validation fails on model text
/// (a string), `project_text_onto_schema` retries via the schema; on total failure the original
/// error wins. Go's `*session.Event`/`*session.RequestInput` passthrough has no counterpart: Rust
/// outputs are `Value`.
pub(crate) fn default_validate_output(
    out: Value,
    schema: Option<&CompiledSchema>,
) -> Result<Value, String> {
    let Some(schema) = schema else { return Ok(out) };
    if out.is_null() {
        return Ok(out);
    }
    let err = match schema.validate(&out) {
        Ok(()) => return Ok(out),
        Err(e) => e,
    };
    if let Value::String(text) = &out
        && let Some(v) = project_text_onto_schema(text, schema)
    {
        return Ok(v);
    }
    Err(err)
}

/// Port of the `*genai.Content` arm of Go defaultValidateOutput / modelText (base_node.go:128):
/// validates the content's JSON form, then falls back to projecting the concatenated non-thought
/// text parts onto the schema.
pub(crate) fn default_validate_content_output(
    content: &Content,
    schema: Option<&CompiledSchema>,
) -> Result<Value, String> {
    let v = serde_json::to_value(content).map_err(|e| e.to_string())?;
    let Some(schema) = schema else { return Ok(v) };
    let err = match schema.validate(&v) {
        Ok(()) => return Ok(v),
        Err(e) => e,
    };
    let text: String = content
        .parts
        .iter()
        .filter(|p| !p.text.is_empty() && !p.thought)
        .map(|p| p.text.as_str())
        .collect();
    project_text_onto_schema(&text, schema).ok_or(err)
}

/// Port of Go projectTextOntoSchema (base_node.go:151): returns the text directly for a string
/// schema (after re-validating so per-string constraints still apply), otherwise JSON-parses and
/// re-validates. `None` when no valid value can be produced.
fn project_text_onto_schema(s: &str, schema: &CompiledSchema) -> Option<Value> {
    if root_schema_is_string(schema) {
        let v = Value::String(s.to_owned());
        return schema.validate(&v).ok().map(|()| v);
    }
    if s.trim().is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(s).ok()?;
    schema.validate(&parsed).ok().map(|()| parsed)
}

/// Port of Go rootSchemaIsString (base_node.go:171): the root permits the "string" type, via a
/// single `"type"` or a `"type"` array.
fn root_schema_is_string(schema: &CompiledSchema) -> bool {
    match schema.schema().get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "string"),
        _ => false,
    }
}

/// Port of Go validateAndStampOutput (base_node.go:187): runs `n.validate_output` on `out` and, on
/// success, stamps the validated value back onto `ev.output` when it rode there. The error is
/// both NodeFailed and OutputValidation (Go's double `%w`).
pub(crate) fn validate_and_stamp_output(
    n: &dyn Node,
    out: Value,
    ev: &mut Event,
) -> Result<Value, AgentError> {
    let validated = n.validate_output(out).map_err(|e| {
        let msg = format!(
            "{}: output validation failed for node {}: {}",
            WorkflowError::sentinel_text(WorkflowErrorKind::NodeFailed),
            go_quote(n.name()),
            e
        );
        AgentError::from(
            WorkflowError::new(WorkflowErrorKind::OutputValidation, msg).with_source(e.into()),
        )
    })?;
    if ev.output.is_some() {
        ev.output = Some(validated.clone());
    }
    Ok(validated)
}
