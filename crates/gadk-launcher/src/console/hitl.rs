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

//! Go `cmd/launcher/console/hitl.go`: prompting for pending interrupts (tool confirmations,
//! workflow input requests and other long-running calls) and answering them with function
//! responses.

use std::collections::HashSet;
use std::io::Write;

use gadk_agent::{REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, original_call_from};
use gadk_engine::workflow::WORKFLOW_INPUT_FUNCTION_CALL_NAME;
use gadk_genai::{FunctionCall, FunctionResponse, Part};
use gadk_session::Event;
use serde_json::{Map, Number, Value};

/// Go `pendingInterrupt`: one HITL prompt the agent emitted on the previous turn that still needs
/// a user reply on the next. The id and name come from a `FunctionCall` part whose ID appears in
/// `Event::long_running_tool_ids`. Go's nil `args` is an empty map.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingInterrupt {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) args: Map<String, Value>,
}

/// Go `collectPendingInterrupts`: the `FunctionCall` parts referenced from
/// `long_running_tool_ids` on the same event, in order of appearance. Partial events are skipped
/// (only the final aggregated event represents a settled interrupt) and call IDs are deduplicated
/// (SSE streaming emits the same function-call event more than once).
pub(crate) fn collect_pending_interrupts(events: &[Event]) -> Vec<PendingInterrupt> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for ev in events {
        if ev.long_running_tool_ids.is_empty() || ev.response.partial {
            continue;
        }
        let Some(content) = &ev.response.content else {
            continue;
        };
        for p in &content.parts {
            let Some(fc) = &p.function_call else {
                continue;
            };
            if !ev.long_running_tool_ids.contains(&fc.id) {
                continue;
            }
            if !seen.insert(fc.id.as_str()) {
                continue;
            }
            out.push(PendingInterrupt {
                id: fc.id.clone(),
                name: fc.name.clone(),
                args: fc.args.clone(),
            });
        }
    }
    out
}

/// Go `renderInterruptPrompt`: prints the pending interrupt and a `User -> ` input prompt. The
/// caller reads the user's reply and passes it to [`build_interrupt_response`]. Write errors are
/// ignored, as Go's `fmt.Print*` results are.
pub(crate) fn render_interrupt_prompt(out: &mut dyn Write, p: &PendingInterrupt) {
    match p.name.as_str() {
        WORKFLOW_INPUT_FUNCTION_CALL_NAME => render_workflow_input_prompt(out, &p.args),
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME => render_tool_confirmation_prompt(out, &p.args),
        _ => render_generic_interrupt_prompt(out, &p.name, &p.args),
    }
    let _ = out.write_all(b"User -> ");
}

/// Go `buildInterruptResponse`: the operator's one-line input as a `FunctionResponse` part keyed
/// by the interrupt's id and name, with the same per-name dispatch as
/// [`render_interrupt_prompt`].
pub(crate) fn build_interrupt_response(p: &PendingInterrupt, user_input: &str) -> Part {
    let line = user_input.trim_end_matches(['\r', '\n']);
    let response = match p.name.as_str() {
        WORKFLOW_INPUT_FUNCTION_CALL_NAME => workflow_input_response_from_user_input(line),
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME => tool_confirmation_response_from_user_input(line),
        _ => generic_response_from_user_input(line),
    };
    Part {
        function_response: Some(FunctionResponse {
            id: p.id.clone(),
            name: p.name.clone(),
            response,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `renderWorkflowInputPrompt`: the workflow request prompt. The args carry the fields
/// `workflow.NewRequestInputEvent` sets: `interruptId`, `message`, `payload`, `responseSchema`.
pub(crate) fn render_workflow_input_prompt(out: &mut dyn Write, args: &Map<String, Value>) {
    let msg = match args.get("message").and_then(Value::as_str) {
        Some(m) if !m.is_empty() => m,
        _ => "Input requested",
    };
    let _ = writeln!(out, "Agent -> {msg}");
    if let Some(payload) = args.get("payload").filter(|v| !v.is_null()) {
        // Strings print raw; other values as pretty JSON aligned under the "  Payload: " label
        // (Go json.MarshalIndent(payload, "  ", "  ")).
        match payload.as_str() {
            Some(s) => {
                let _ = writeln!(out, "  Payload: {s}");
            }
            None => {
                let _ = writeln!(out, "  Payload: {}", go_json(payload, Some(("  ", "  "))));
            }
        }
    }
    if let Some(schema) = args.get("responseSchema").filter(|v| !v.is_null()) {
        let _ = writeln!(out, "  Expected response schema: {}", go_json(schema, None));
    }
}

/// Go `workflowInputResponseFromUserInput`: a JSON object reply is returned verbatim, other JSON
/// values are wrapped under `payload`, and unparseable input is passed through under `payload`.
fn workflow_input_response_from_user_input(line: &str) -> Map<String, Value> {
    wrap_reply(line, "payload")
}

/// Go `renderToolConfirmationPrompt`: `Agent -> Confirm <original tool>?` (`unknown` when the
/// original call cannot be read) and the answer hint.
///
/// Go prints `args["toolConfirmation"]`'s hint when that value is a `map[string]any`. On the events
/// the runner yields it never is: Go's flow stores the `toolconfirmation.ToolConfirmation` struct
/// itself there (`internal/llminternal/functions.go`), so the assertion fails and Go always prints
/// the fallback (the probe's `hitl_*` fixtures record `Confirm transfer_funds?` for a tool whose
/// hint is `Please approve or reject the tool call transfer_funds() …`). gadk's args are JSON, so
/// the hint is skipped here to print what Go prints.
fn render_tool_confirmation_prompt(out: &mut dyn Write, args: &Map<String, Value>) {
    let call = FunctionCall {
        args: args.clone(),
        ..Default::default()
    };
    let original_name = match original_call_from(&call) {
        Ok(oc) if !oc.name.is_empty() => oc.name,
        _ => "unknown".to_owned(),
    };
    let _ = writeln!(out, "Agent -> Confirm {original_name}?");
    let _ = writeln!(out, "  Type 'yes' to confirm, anything else to reject.");
}

/// Go `toolConfirmationResponseFromUserInput`: `y`/`yes`/`true`/`confirm` (case-insensitive,
/// surrounding space ignored) → `{"confirmed": true}`; anything else, a blank line included,
/// → `{"confirmed": false}`.
fn tool_confirmation_response_from_user_input(line: &str) -> Map<String, Value> {
    let lower = line.to_lowercase();
    let confirmed = matches!(lower.trim(), "y" | "yes" | "true" | "confirm");
    Map::from_iter([("confirmed".to_owned(), Value::Bool(confirmed))])
}

/// Go `renderGenericInterruptPrompt`: the fallback for kinds the launcher does not recognize; the
/// kind name and the raw args.
fn render_generic_interrupt_prompt(out: &mut dyn Write, name: &str, args: &Map<String, Value>) {
    let _ = writeln!(out, "Agent -> waiting for response (kind: {name})");
    if !args.is_empty() {
        let _ = writeln!(
            out,
            "  Args: {}",
            go_json(&Value::Object(args.clone()), None)
        );
    }
}

/// Go `genericResponseFromUserInput`: a JSON object reply is returned verbatim, other JSON values
/// and raw text are wrapped under `result`.
fn generic_response_from_user_input(line: &str) -> Map<String, Value> {
    wrap_reply(line, "result")
}

/// The shared body of Go's two JSON-or-text reply parsers. Numbers keep serde's integer/float
/// distinction where Go decodes every JSON number as `float64`.
fn wrap_reply(line: &str, key: &str) -> Map<String, Value> {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(m)) => return m,
        Ok(parsed) => parsed,
        Err(_) => Value::String(line.to_owned()),
    };
    Map::from_iter([(key.to_owned(), value)])
}

/// Go console.go `renderOutput`: a node's `Output` value for the console; strings as they are,
/// anything else as compact JSON (Go `json.Marshal`).
pub(crate) fn render_output(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => go_json(other, None),
    }
}

/// Go `json.Marshal` (`indent == None`) or `json.MarshalIndent(v, prefix, indent)` of a decoded
/// JSON value: map keys sorted (as `serde_json::Map` is without `preserve_order`), `<`, `>`, `&`,
/// U+2028 and U+2029 escaped, floats in Go's format.
fn go_json(v: &Value, indent: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    write_go_json(&mut out, v, indent, 0);
    out
}

fn write_go_json(out: &mut String, v: &Value, indent: Option<(&str, &str)>, depth: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&go_number(n)),
        Value::String(s) => out.push_str(&go_string(s)),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_go_json(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                out.push_str(&go_string(k));
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_go_json(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
    }
}

/// Go `json.Indent`'s line break: newline, prefix, one indent per nesting level.
fn newline(out: &mut String, indent: Option<(&str, &str)>, depth: usize) {
    if let Some((prefix, unit)) = indent {
        out.push('\n');
        out.push_str(prefix);
        for _ in 0..depth {
            out.push_str(unit);
        }
    }
}

/// A Go `encoding/json` string literal: serde's escaping plus Go's HTML and line-separator
/// escapes (serde never produces these characters inside an escape sequence).
fn go_string(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap_or_default();
    let mut out = String::with_capacity(quoted.len());
    for c in quoted.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// Go `encoding/json` `floatEncoder` for floats (`'f'` shortest, `'e'` below 1e-6 or from 1e21
/// on, a positive exponent with at least two digits); integers as they are.
fn go_number(n: &Number) -> String {
    let Some(f) = n.as_f64().filter(|_| n.is_f64()) else {
        return n.to_string();
    };
    let abs = f.abs();
    if abs == 0.0 || (1e-6..1e21).contains(&abs) {
        return format!("{f}");
    }
    let e = format!("{f:e}");
    match e.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp:0>2}"),
        _ => e,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn go_json_matches_go_marshal() {
        let v = json!({"b": [1, 2.5, "x<y>&z"], "a": {}, "c": [], "d": null, "e": 1e21, "f": 1e-7});
        assert_eq!(
            go_json(&v, None),
            r#"{"a":{},"b":[1,2.5,"x\u003cy\u003e\u0026z"],"c":[],"d":null,"e":1e+21,"f":1e-7}"#
        );
    }

    #[test]
    fn go_json_matches_go_marshal_indent() {
        let v = json!({"user": "alice", "days": 5, "list": [1, {"k": true}], "empty": {}});
        assert_eq!(
            go_json(&v, Some(("  ", "  "))),
            "{\n    \"days\": 5,\n    \"empty\": {},\n    \"list\": [\n      1,\n      {\n        \"k\": true\n      }\n    ],\n    \"user\": \"alice\"\n  }"
        );
    }

    #[test]
    fn tool_confirmation_prompt_names_the_original_call_as_go_prints_it() {
        let mut out = Vec::new();
        render_tool_confirmation_prompt(
            &mut out,
            json!({
                "originalFunctionCall": {"name": "transfer_funds", "args": {"amount": 100}},
                "toolConfirmation": {"hint": "Please approve or reject the tool call transfer_funds() by responding with a FunctionResponse with an expected ToolConfirmation payload.", "confirmed": false},
            })
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Agent -> Confirm transfer_funds?\n  Type 'yes' to confirm, anything else to reject.\n"
        );
        let mut out = Vec::new();
        render_tool_confirmation_prompt(&mut out, &Map::new());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Agent -> Confirm unknown?\n  Type 'yes' to confirm, anything else to reject.\n"
        );
    }

    #[test]
    fn generic_prompt_prints_kind_and_args() {
        let mut out = Vec::new();
        render_interrupt_prompt(
            &mut out,
            &PendingInterrupt {
                id: "g".into(),
                name: "ask".into(),
                args: json!({"q": "a&b"}).as_object().unwrap().clone(),
            },
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Agent -> waiting for response (kind: ask)\n  Args: {\"q\":\"a\\u0026b\"}\nUser -> "
        );
    }

    #[test]
    fn workflow_prompt_defaults_message_and_prints_schema() {
        let mut out = Vec::new();
        render_workflow_input_prompt(
            &mut out,
            json!({"responseSchema": {"type": "string"}, "payload": null})
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Agent -> Input requested\n  Expected response schema: {\"type\":\"string\"}\n"
        );
    }
}
