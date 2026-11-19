//! Cloud Trace spans to [`ProductionRun`]s.
//!
//! gadk's telemetry gives each agent run an `invoke_agent` span (`gen_ai.agent.name`,
//! `gen_ai.conversation.id`, `gcp.vertex.agent.invocation_id`), each model call a
//! `generate_content` span (`gen_ai.input.messages`, `gen_ai.output.messages` when message
//! content is captured on spans) and each tool call an `execute_tool` span (`gen_ai.tool.name`,
//! with arguments in `gen_ai.tool.call.arguments`, or `gcp.vertex.agent.tool_call_args` in the
//! legacy schema). The span kind is the `gen_ai.operation.name` label. [`build_runs`] rebuilds
//! one run per invocation:
//!
//! - the invocation's top-level spans are its `invoke_agent` spans with no `invoke_agent`
//!   ancestor of the same invocation (several when a workflow agent runs sub-agents side by
//!   side under `invoke_workflow`); the earliest is the run's root and gives the agent, the
//!   session and the start time. The run's spans are every top-level span and its
//!   descendants, stopping at an `invoke_agent` span of another invocation (its own run);
//! - the run has one turn: `user` is the text of the last input message of the first model call
//!   when that message is the user's, and `reply` the text (not the reasoning) of every model
//!   call's output, in start order, joined with `"\n"`;
//! - the tool calls are the `execute_tool` spans in start order (ties by span id), except the
//!   `execute_tool (merged)` span that groups parallel calls;
//! - the run error is `error.type: <value>` from the root's `error.type` label, else from the
//!   earliest `invoke_agent` span of the run that has one. Span status (the error message) is not
//!   a label, so it is not available, and the legacy schema records no `error.type`.
//!
//! Content labels are JSON text (a JSON string holding JSON text is read too). Problems never
//! fail the mapping: they become notes and clear `content_complete` or `tools_complete`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use gadk_cloudtrace::{Span, Trace};
use gadk_eval::{RunTranscript, ToolCall, TurnRecord};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::source::ProductionRun;

const OPERATION: &str = "gen_ai.operation.name";
const INVOKE_AGENT: &str = "invoke_agent";
const GENERATE_CONTENT: &str = "generate_content";
const EXECUTE_TOOL: &str = "execute_tool";
/// The name of the span that groups parallel tool calls (its children are the calls).
const MERGED_TOOL_SPAN: &str = "execute_tool (merged)";
const INVOCATION_ID: &str = "gcp.vertex.agent.invocation_id";
const AGENT_NAME: &str = "gen_ai.agent.name";
const CONVERSATION_ID: &str = "gen_ai.conversation.id";
const INPUT_MESSAGES: &str = "gen_ai.input.messages";
const OUTPUT_MESSAGES: &str = "gen_ai.output.messages";
const TOOL_NAME: &str = "gen_ai.tool.name";
const TOOL_ARGUMENTS: &str = "gen_ai.tool.call.arguments";
const LEGACY_TOOL_ARGUMENTS: &str = "gcp.vertex.agent.tool_call_args";
const ERROR_TYPE: &str = "error.type";

/// Why a content attribute may be missing.
const CAPTURE_HINT: &str = "message content is recorded on spans only with \
    OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY (or SPAN_AND_EVENT), \
    and a value over 60 KiB is left off the span";

/// One message of a `gen_ai.*.messages` attribute.
#[derive(Deserialize)]
struct Message {
    role: String,
    #[serde(default)]
    parts: Vec<Part>,
}

/// One message part; only `text` parts' `content` is read.
#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Option<Value>,
}

/// The trace's spans by id and by parent id.
struct Index<'a> {
    by_id: HashMap<&'a str, &'a Span>,
    children: HashMap<&'a str, Vec<&'a Span>>,
    len: usize,
}

impl<'a> Index<'a> {
    fn new(trace: &'a Trace) -> Self {
        let mut by_id = HashMap::new();
        let mut children: HashMap<&'a str, Vec<&'a Span>> = HashMap::new();
        for span in &trace.spans {
            by_id.insert(span.span_id.as_str(), span);
            if let Some(parent) = span.parent_id.as_deref() {
                children.entry(parent).or_default().push(span);
            }
        }
        Self {
            by_id,
            children,
            len: trace.spans.len(),
        }
    }

    /// Whether an ancestor of `span` is an `invoke_agent` span of `invocation` (a parent chain
    /// is followed at most as many steps as there are spans).
    fn has_invoke_agent_ancestor(&self, span: &Span, invocation: &str) -> bool {
        let mut parent = span.parent_id.as_deref();
        for _ in 0..self.len {
            let Some(p) = parent.and_then(|id| self.by_id.get(id)) else {
                return false;
            };
            if is_op(p, INVOKE_AGENT) && invocation_of(p) == Some(invocation) {
                return true;
            }
            parent = p.parent_id.as_deref();
        }
        false
    }

    /// The `tops` and their descendants, not descending into an `invoke_agent` span of another
    /// invocation; each span is visited once.
    fn run_spans(&self, tops: &[&'a Span], invocation: &str) -> Vec<&'a Span> {
        let mut out = tops.to_vec();
        let mut seen: HashSet<&str> = tops.iter().map(|s| s.span_id.as_str()).collect();
        let mut stack = tops.to_vec();
        while let Some(span) = stack.pop() {
            for &child in self
                .children
                .get(span.span_id.as_str())
                .into_iter()
                .flatten()
            {
                if !seen.insert(child.span_id.as_str()) {
                    continue;
                }
                if is_op(child, INVOKE_AGENT)
                    && invocation_of(child).is_some_and(|i| i != invocation)
                {
                    continue;
                }
                out.push(child);
                stack.push(child);
            }
        }
        out
    }
}

/// The runs of `trace`, one per invocation, ordered by start time (ties by invocation id).
pub fn build_runs(trace: &Trace) -> Vec<ProductionRun> {
    let index = Index::new(trace);
    // The top-level `invoke_agent` spans of each invocation.
    let mut tops: BTreeMap<&str, Vec<&Span>> = BTreeMap::new();
    for span in &trace.spans {
        if !is_op(span, INVOKE_AGENT) {
            continue;
        }
        let Some(invocation) = invocation_of(span) else {
            continue;
        };
        if index.has_invoke_agent_ancestor(span, invocation) {
            continue;
        }
        tops.entry(invocation).or_default().push(span);
    }
    let mut runs: Vec<ProductionRun> = tops
        .into_iter()
        .map(|(invocation, mut tops)| {
            // The earliest top-level span is the root.
            tops.sort_by(by_start);
            build_run(trace, &index, invocation, &tops)
        })
        .collect();
    runs.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.invocation_id.cmp(&b.invocation_id))
    });
    runs
}

/// The run of `invocation`; `tops` are its top-level spans, earliest (the root) first.
fn build_run<'a>(
    trace: &Trace,
    index: &Index<'a>,
    invocation: &str,
    tops: &[&'a Span],
) -> ProductionRun {
    let root = tops[0];
    let spans = index.run_spans(tops, invocation);
    let mut models: Vec<&Span> = spans
        .iter()
        .copied()
        .filter(|s| is_op(s, GENERATE_CONTENT))
        .collect();
    models.sort_by(by_start);
    let mut tools: Vec<&Span> = spans
        .iter()
        .copied()
        .filter(|s| is_op(s, EXECUTE_TOOL) && s.name != MERGED_TOOL_SPAN)
        .collect();
    tools.sort_by(by_start);
    let capture = spans
        .iter()
        .any(|s| s.labels.contains_key(INPUT_MESSAGES) || s.labels.contains_key(OUTPUT_MESSAGES));
    let error = run_error(root, &spans);
    let mut content_notes = Vec::new();
    let user = user_text(models.first().copied(), &mut content_notes);
    let reply = reply_text(&models, error.is_some(), &mut content_notes);
    let mut tool_notes = Vec::new();
    let calls = tools
        .iter()
        .filter_map(|s| tool_call(s, capture, &mut tool_notes))
        .collect();
    let content_complete = content_notes.is_empty();
    let tools_complete = tool_notes.is_empty();
    ProductionRun {
        invocation_id: invocation.to_owned(),
        session_id: label(root, CONVERSATION_ID).unwrap_or_default().to_owned(),
        trace_id: Some(trace.trace_id.clone()),
        agent: label(root, AGENT_NAME).unwrap_or_default().to_owned(),
        started_at: root.start,
        transcript: RunTranscript {
            turns: vec![TurnRecord { user, reply, calls }],
            error,
        },
        content_complete,
        tools_complete,
        notes: content_notes.into_iter().chain(tool_notes).collect(),
    }
}

/// The text of the last input message of the run's first model call, when it is the user's.
fn user_text(first: Option<&Span>, notes: &mut Vec<String>) -> String {
    let Some(span) = first else {
        notes.push("the run has no generate_content span".to_owned());
        return String::new();
    };
    let Some(text) = label(span, INPUT_MESSAGES) else {
        notes.push(missing(GENERATE_CONTENT, span, INPUT_MESSAGES));
        return String::new();
    };
    let messages = match parse_messages(text) {
        Ok(messages) => messages,
        Err(e) => {
            notes.push(format!(
                "generate_content span {}: {INPUT_MESSAGES} {e}",
                span.span_id
            ));
            return String::new();
        }
    };
    let Some(last) = messages.last() else {
        notes.push(format!(
            "generate_content span {}: {INPUT_MESSAGES} has no messages",
            span.span_id
        ));
        return String::new();
    };
    if last.role != "user" {
        notes.push(format!(
            "resumed invocation: the last input message has role `{}`, not `user`",
            last.role
        ));
        return String::new();
    }
    let user = text_parts(&last.parts).collect::<Vec<_>>().join("\n");
    if user.is_empty() {
        notes.push("the user message has no text".to_owned());
    }
    user
}

/// The text of every model call's output, in start order, joined with `"\n"`.
fn reply_text(models: &[&Span], has_error: bool, notes: &mut Vec<String>) -> String {
    let mut texts: Vec<String> = Vec::new();
    let mut noted = false;
    for span in models {
        match label(span, OUTPUT_MESSAGES) {
            // A failed call (default schema) records `error.type` and no output.
            None if span.labels.contains_key(ERROR_TYPE) => {}
            None => {
                notes.push(missing(GENERATE_CONTENT, span, OUTPUT_MESSAGES));
                noted = true;
            }
            Some(text) => match parse_messages(text) {
                Ok(messages) => texts.extend(messages.iter().flat_map(|m| text_parts(&m.parts))),
                Err(e) => {
                    notes.push(format!(
                        "generate_content span {}: {OUTPUT_MESSAGES} {e}",
                        span.span_id
                    ));
                    noted = true;
                }
            },
        }
    }
    let reply = texts.join("\n");
    if reply.is_empty() && !has_error && !models.is_empty() && !noted {
        notes.push("the run has no reply text and no error".to_owned());
    }
    reply
}

/// The call of an `execute_tool` span; `None` (with a note) when it has no tool name.
fn tool_call(span: &Span, capture: bool, notes: &mut Vec<String>) -> Option<ToolCall> {
    let Some(name) = label(span, TOOL_NAME).filter(|n| !n.is_empty()) else {
        notes.push(format!(
            "execute_tool span {} has no {TOOL_NAME}",
            span.span_id
        ));
        return None;
    };
    let id = &span.span_id;
    let args = match label(span, TOOL_ARGUMENTS).or_else(|| label(span, LEGACY_TOOL_ARGUMENTS)) {
        Some(text) => match parse_json(text) {
            Ok(Value::Null) => Map::new(),
            Ok(Value::Object(args)) => args,
            Ok(_) => {
                notes.push(format!(
                    "execute_tool span {id} ({name}): the arguments are not a JSON object"
                ));
                Map::new()
            }
            Err(e) => {
                notes.push(format!(
                    "execute_tool span {id} ({name}): the arguments attribute {e}"
                ));
                Map::new()
            }
        },
        // With content capture on, the default schema leaves empty arguments off the span.
        None if capture => Map::new(),
        None => {
            notes.push(format!(
                "execute_tool span {id} ({name}) has no {TOOL_ARGUMENTS} or \
                 {LEGACY_TOOL_ARGUMENTS} ({CAPTURE_HINT})"
            ));
            Map::new()
        }
    };
    Some(ToolCall {
        name: name.to_owned(),
        args,
    })
}

/// `error.type: <value>` from the root, else from the earliest `invoke_agent` span that has one.
fn run_error(root: &Span, spans: &[&Span]) -> Option<String> {
    if let Some(e) = label(root, ERROR_TYPE) {
        return Some(format!("error.type: {e}"));
    }
    let mut agents: Vec<&Span> = spans
        .iter()
        .copied()
        .filter(|s| is_op(s, INVOKE_AGENT) && s.labels.contains_key(ERROR_TYPE))
        .collect();
    agents.sort_by(by_start);
    agents
        .first()
        .and_then(|s| label(s, ERROR_TYPE))
        .map(|e| format!("error.type: {e}"))
}

/// The note for a content attribute missing from a span.
fn missing(kind: &str, span: &Span, key: &str) -> String {
    format!("{kind} span {} has no {key} ({CAPTURE_HINT})", span.span_id)
}

/// The non-empty `content` strings of the `text` parts.
fn text_parts(parts: &[Part]) -> impl Iterator<Item = String> + '_ {
    parts
        .iter()
        .filter(|p| p.kind == "text")
        .filter_map(|p| p.content.as_ref()?.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// A label's JSON value; a JSON string is parsed once more (JSON text inside a JSON string).
fn parse_json(text: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("is not valid JSON ({e})"))?;
    match value {
        Value::String(inner) => {
            serde_json::from_str(&inner).map_err(|e| format!("is not valid JSON ({e})"))
        }
        other => Ok(other),
    }
}

fn parse_messages(text: &str) -> Result<Vec<Message>, String> {
    serde_json::from_value(parse_json(text)?).map_err(|e| format!("is not a message list ({e})"))
}

fn label<'s>(span: &'s Span, key: &str) -> Option<&'s str> {
    span.labels.get(key).map(String::as_str)
}

fn is_op(span: &Span, op: &str) -> bool {
    label(span, OPERATION) == Some(op)
}

fn invocation_of(span: &Span) -> Option<&str> {
    label(span, INVOCATION_ID).filter(|i| !i.is_empty())
}

/// Start time, then span id.
fn by_start(a: &&Span, b: &&Span) -> Ordering {
    a.start
        .cmp(&b.start)
        .then_with(|| a.span_id.cmp(&b.span_id))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testutil::at;

    const TRACE_ID: &str = "0123456789abcdef0123456789abcdef";
    /// 2025-11-17T12:00:00Z.
    const T0: i64 = 1_763_380_800;

    /// A span starting `offset` seconds after `T0`, one second long.
    fn span(
        id: &str,
        parent: Option<&str>,
        name: &str,
        offset: i64,
        labels: &[(&str, &str)],
    ) -> Span {
        Span {
            span_id: id.into(),
            parent_id: parent.map(str::to_owned),
            name: name.into(),
            start: at(T0 + offset),
            end: at(T0 + offset + 1),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn with(mut span: Span, key: &str, value: &str) -> Span {
        span.labels.insert(key.to_owned(), value.to_owned());
        span
    }

    fn agent(id: &str, parent: Option<&str>, name: &str, invocation: &str, offset: i64) -> Span {
        span(
            id,
            parent,
            &format!("invoke_agent {name}"),
            offset,
            &[
                (OPERATION, INVOKE_AGENT),
                (AGENT_NAME, name),
                (CONVERSATION_ID, "s-1"),
                (INVOCATION_ID, invocation),
            ],
        )
    }

    /// A `generate_content` span with the given message lists as JSON text.
    fn model(
        id: &str,
        parent: &str,
        offset: i64,
        input: Option<Value>,
        output: Option<Value>,
    ) -> Span {
        let mut s = span(
            id,
            Some(parent),
            "generate_content gemini-2.5-flash",
            offset,
            &[(OPERATION, GENERATE_CONTENT)],
        );
        if let Some(input) = input {
            s = with(s, INPUT_MESSAGES, &input.to_string());
        }
        if let Some(output) = output {
            s = with(s, OUTPUT_MESSAGES, &output.to_string());
        }
        s
    }

    fn tool(id: &str, parent: &str, offset: i64, name: &str) -> Span {
        span(
            id,
            Some(parent),
            &format!("execute_tool {name}"),
            offset,
            &[(OPERATION, EXECUTE_TOOL), (TOOL_NAME, name)],
        )
    }

    fn msg(role: &str, texts: &[&str]) -> Value {
        let parts: Vec<Value> = texts
            .iter()
            .map(|t| json!({"type": "text", "content": t}))
            .collect();
        json!({"role": role, "parts": parts})
    }

    fn asked(texts: &[&str]) -> Value {
        json!([msg("user", texts)])
    }

    fn answered(texts: &[&str]) -> Value {
        let mut m = msg("assistant", texts);
        m["finish_reason"] = json!("stop");
        json!([m])
    }

    fn trace(spans: Vec<Span>) -> Trace {
        Trace {
            trace_id: TRACE_ID.into(),
            spans,
        }
    }

    fn only(trace: &Trace) -> ProductionRun {
        let mut runs = build_runs(trace);
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs.remove(0)
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn call(name: &str, a: Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args: args(a),
        }
    }

    #[test]
    fn one_invocation_is_one_run_with_one_turn() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["Where is my order?"])),
                Some(json!([{"role": "assistant", "parts": [
                    {"type": "tool_call", "id": "c1", "name": "lookup_order", "arguments": {"id": 7}}
                ], "finish_reason": "tool_call"}])),
            ),
            with(
                tool("t1", "a1", 2, "lookup_order"),
                TOOL_ARGUMENTS,
                r#"{"id":7}"#,
            ),
            model(
                "m2",
                "a1",
                3,
                Some(json!([
                    msg("user", &["Where is my order?"]),
                    {"role": "assistant", "parts": [
                        {"type": "tool_call", "name": "lookup_order", "arguments": {"id": 7}}
                    ]},
                    {"role": "tool", "parts": [
                        {"type": "tool_call_response", "response": {"status": "shipped"}}
                    ]}
                ])),
                Some(answered(&["It ships today."])),
            ),
        ]);
        assert_eq!(
            build_runs(&t),
            vec![ProductionRun {
                invocation_id: "inv-1".into(),
                session_id: "s-1".into(),
                trace_id: Some(TRACE_ID.into()),
                agent: "support".into(),
                started_at: at(T0),
                transcript: RunTranscript {
                    turns: vec![TurnRecord {
                        user: "Where is my order?".into(),
                        reply: "It ships today.".into(),
                        calls: vec![call("lookup_order", json!({"id": 7}))],
                    }],
                    error: None,
                },
                content_complete: true,
                tools_complete: true,
                notes: Vec::new(),
            }]
        );
    }

    #[test]
    fn two_invocations_make_two_runs_in_start_order() {
        let t = trace(vec![
            agent("b1", None, "support", "inv-b", 10),
            model(
                "bm",
                "b1",
                11,
                Some(asked(&["second"])),
                Some(answered(&["two"])),
            ),
            agent("a1", None, "support", "inv-a", 0),
            model(
                "am",
                "a1",
                1,
                Some(asked(&["first"])),
                Some(answered(&["one"])),
            ),
        ]);
        let runs = build_runs(&t);
        let got: Vec<(&str, &str, &str)> = runs
            .iter()
            .map(|r| {
                let turn = &r.transcript.turns[0];
                (
                    r.invocation_id.as_str(),
                    turn.user.as_str(),
                    turn.reply.as_str(),
                )
            })
            .collect();
        assert_eq!(got, [("inv-a", "first", "one"), ("inv-b", "second", "two")]);
        assert!(runs.iter().all(|r| r.content_complete && r.tools_complete));
    }

    #[test]
    fn a_sub_agent_of_the_same_invocation_belongs_to_the_run() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            agent("a2", Some("a1"), "billing", "inv-1", 1),
            model(
                "m1",
                "a2",
                2,
                Some(asked(&["refund?"])),
                Some(answered(&["Refunded."])),
            ),
            with(
                tool("t1", "a2", 3, "refund"),
                TOOL_ARGUMENTS,
                r#"{"order":7}"#,
            ),
        ]);
        let run = only(&t);
        assert_eq!((run.agent.as_str(), run.started_at), ("support", at(T0)));
        let turn = &run.transcript.turns[0];
        assert_eq!(
            (turn.user.as_str(), turn.reply.as_str()),
            ("refund?", "Refunded.")
        );
        assert_eq!(turn.calls, vec![call("refund", json!({"order": 7}))]);
        assert!(
            run.content_complete && run.tools_complete,
            "{:?}",
            run.notes
        );
    }

    #[test]
    fn a_workflow_parent_is_not_the_root() {
        let t = trace(vec![
            span(
                "w1",
                None,
                "invoke_workflow pipeline",
                0,
                &[(OPERATION, "invoke_workflow"), (CONVERSATION_ID, "s-1")],
            ),
            agent("a1", Some("w1"), "step_one", "inv-1", 1),
            model(
                "m1",
                "a1",
                2,
                Some(asked(&["start"])),
                Some(answered(&["done"])),
            ),
        ]);
        let run = only(&t);
        assert_eq!(
            (run.agent.as_str(), run.started_at),
            ("step_one", at(T0 + 1))
        );
        assert!(run.content_complete, "{:?}", run.notes);
    }

    #[test]
    fn sibling_sub_agents_under_a_workflow_are_one_run() {
        let t = trace(vec![
            span(
                "w1",
                None,
                "invoke_workflow pipeline",
                0,
                &[(OPERATION, "invoke_workflow"), (CONVERSATION_ID, "s-1")],
            ),
            agent("a2", Some("w1"), "step_two", "inv-1", 3),
            model(
                "m2",
                "a2",
                4,
                Some(asked(&["later"])),
                Some(answered(&["second"])),
            ),
            with(
                tool("t2", "a2", 5, "publish"),
                TOOL_ARGUMENTS,
                r#"{"id":2}"#,
            ),
            agent("a1", Some("w1"), "step_one", "inv-1", 1),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["start"])),
                Some(answered(&["first"])),
            ),
            with(tool("t1", "a1", 2, "draft"), TOOL_ARGUMENTS, r#"{"id":1}"#),
        ]);
        let run = only(&t);
        assert_eq!(
            (run.agent.as_str(), run.started_at),
            ("step_one", at(T0 + 1))
        );
        let turn = &run.transcript.turns[0];
        assert_eq!(
            (turn.user.as_str(), turn.reply.as_str()),
            ("start", "first\nsecond")
        );
        assert_eq!(
            turn.calls,
            vec![
                call("draft", json!({"id": 1})),
                call("publish", json!({"id": 2}))
            ]
        );
        assert!(
            run.content_complete && run.tools_complete,
            "{:?}",
            run.notes
        );
    }

    #[test]
    fn the_user_turn_is_the_last_input_message() {
        let input = json!([
            msg("user", &["old question"]),
            msg("assistant", &["old answer"]),
            msg("user", &["new", "question"]),
        ]);
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, Some(input), Some(answered(&["ok"]))),
        ]);
        assert_eq!(only(&t).transcript.turns[0].user, "new\nquestion");
    }

    #[test]
    fn the_reply_joins_every_model_call_and_skips_reasoning() {
        let first = json!([{"role": "assistant", "parts": [
            {"type": "reasoning", "content": "thinking"},
            {"type": "text", "content": "Step one."},
            {"type": "tool_call", "name": "f", "arguments": null}
        ]}]);
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, Some(asked(&["hi"])), Some(first)),
            model(
                "m2",
                "a1",
                3,
                Some(asked(&["hi"])),
                Some(answered(&["Step two."])),
            ),
        ]);
        let run = only(&t);
        assert_eq!(run.transcript.turns[0].reply, "Step one.\nStep two.");
        assert!(run.content_complete, "{:?}", run.notes);
    }

    #[test]
    fn parallel_calls_are_ordered_and_the_merged_span_is_skipped() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["time and weather?"])),
                Some(answered(&["Noon, sunny."])),
            ),
            span(
                "t0",
                Some("a1"),
                MERGED_TOOL_SPAN,
                2,
                &[
                    (OPERATION, EXECUTE_TOOL),
                    (TOOL_NAME, "(merged tools)"),
                    (LEGACY_TOOL_ARGUMENTS, "N/A"),
                ],
            ),
            with(
                tool("t2", "t0", 2, "weather"),
                TOOL_ARGUMENTS,
                r#"{"city":"Oslo"}"#,
            ),
            with(
                tool("t1", "t0", 2, "time"),
                TOOL_ARGUMENTS,
                r#"{"tz":"CET"}"#,
            ),
        ]);
        let run = only(&t);
        assert_eq!(
            run.transcript.turns[0].calls,
            vec![
                call("time", json!({"tz": "CET"})),
                call("weather", json!({"city": "Oslo"})),
            ]
        );
        assert!(run.tools_complete, "{:?}", run.notes);
    }

    #[test]
    fn both_argument_labels_null_and_wrapped_json_are_read() {
        let wrapped = serde_json::to_string(r#"{"b":true}"#).unwrap();
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["hi"])),
                Some(answered(&["hello"])),
            ),
            with(
                tool("t1", "a1", 2, "legacy"),
                LEGACY_TOOL_ARGUMENTS,
                r#"{"city":"Oslo"}"#,
            ),
            with(
                with(tool("t2", "a1", 3, "both"), TOOL_ARGUMENTS, r#"{"a":1}"#),
                LEGACY_TOOL_ARGUMENTS,
                r#"{"a":2}"#,
            ),
            with(tool("t3", "a1", 4, "empty"), LEGACY_TOOL_ARGUMENTS, "null"),
            with(tool("t4", "a1", 5, "wrapped"), TOOL_ARGUMENTS, &wrapped),
        ]);
        let run = only(&t);
        assert_eq!(
            run.transcript.turns[0].calls,
            vec![
                call("legacy", json!({"city": "Oslo"})),
                call("both", json!({"a": 1})),
                call("empty", json!({})),
                call("wrapped", json!({"b": true})),
            ]
        );
        assert!(run.tools_complete, "{:?}", run.notes);
    }

    #[test]
    fn without_content_capture_the_run_is_incomplete() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, None, None),
            tool("t1", "a1", 2, "lookup_order"),
        ]);
        let run = only(&t);
        assert!(!run.content_complete);
        assert!(!run.tools_complete);
        assert_eq!(run.notes.len(), 3, "{:?}", run.notes);
        assert!(
            run.notes[0].starts_with("generate_content span m1 has no gen_ai.input.messages"),
            "{}",
            run.notes[0]
        );
        assert!(
            run.notes[1].starts_with("generate_content span m1 has no gen_ai.output.messages"),
            "{}",
            run.notes[1]
        );
        assert!(
            run.notes[2].starts_with(
                "execute_tool span t1 (lookup_order) has no gen_ai.tool.call.arguments"
            ),
            "{}",
            run.notes[2]
        );
        for note in &run.notes {
            assert!(
                note.contains("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY"),
                "{note}"
            );
            assert!(note.contains("SPAN_AND_EVENT"), "{note}");
            assert!(note.contains("60 KiB"), "{note}");
        }
        let turn = &run.transcript.turns[0];
        assert_eq!((turn.user.as_str(), turn.reply.as_str()), ("", ""));
        assert_eq!(turn.calls, vec![call("lookup_order", json!({}))]);
    }

    #[test]
    fn with_content_capture_a_call_without_arguments_has_none() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["list my orders"])),
                Some(answered(&["Two."])),
            ),
            tool("t1", "a1", 2, "list_orders"),
        ]);
        let run = only(&t);
        assert_eq!(
            run.transcript.turns[0].calls,
            vec![call("list_orders", json!({}))]
        );
        assert!(
            run.content_complete && run.tools_complete,
            "{:?}",
            run.notes
        );
    }

    #[test]
    fn bad_json_and_bad_shapes_are_notes() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            with(
                with(model("m1", "a1", 1, None, None), INPUT_MESSAGES, "not json"),
                OUTPUT_MESSAGES,
                r#"{"role":"assistant"}"#,
            ),
            with(tool("t1", "a1", 2, "lookup_order"), TOOL_ARGUMENTS, "[1,2]"),
        ]);
        let run = only(&t);
        assert!(!run.content_complete && !run.tools_complete);
        assert_eq!(run.notes.len(), 3, "{:?}", run.notes);
        assert!(
            run.notes[0]
                .starts_with("generate_content span m1: gen_ai.input.messages is not valid JSON"),
            "{}",
            run.notes[0]
        );
        assert!(
            run.notes[1].starts_with(
                "generate_content span m1: gen_ai.output.messages is not a message list"
            ),
            "{}",
            run.notes[1]
        );
        assert_eq!(
            run.notes[2],
            "execute_tool span t1 (lookup_order): the arguments are not a JSON object"
        );
        assert_eq!(
            run.transcript.turns[0].calls,
            vec![call("lookup_order", json!({}))]
        );
    }

    #[test]
    fn a_message_label_holding_a_json_string_is_read() {
        let doubly = serde_json::to_string(&asked(&["hi"]).to_string()).unwrap();
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            with(
                model("m1", "a1", 1, None, Some(answered(&["hello"]))),
                INPUT_MESSAGES,
                &doubly,
            ),
        ]);
        let run = only(&t);
        assert_eq!(run.transcript.turns[0].user, "hi");
        assert!(run.content_complete, "{:?}", run.notes);
    }

    #[test]
    fn a_resumed_invocation_has_no_user_text() {
        let input = json!([
            msg("user", &["refund order 7"]),
            {"role": "assistant", "parts": [
                {"type": "tool_call", "id": "c1", "name": "refund", "arguments": {"order": 7}}
            ]},
            {"role": "tool", "parts": [
                {"type": "tool_call_response", "id": "c1", "response": {"approved": true}}
            ]}
        ]);
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, Some(input), Some(answered(&["Refunded."]))),
        ]);
        let run = only(&t);
        let turn = &run.transcript.turns[0];
        assert_eq!((turn.user.as_str(), turn.reply.as_str()), ("", "Refunded."));
        assert!(!run.content_complete);
        assert_eq!(
            run.notes,
            ["resumed invocation: the last input message has role `tool`, not `user`"]
        );
    }

    #[test]
    fn a_failed_model_call_with_an_error_type_is_complete() {
        let t = trace(vec![
            with(
                agent("a1", None, "support", "inv-1", 0),
                ERROR_TYPE,
                "ModelError",
            ),
            with(
                model("m1", "a1", 1, Some(asked(&["hi"])), None),
                ERROR_TYPE,
                "429",
            ),
        ]);
        let run = only(&t);
        assert_eq!(
            run.transcript.error.as_deref(),
            Some("error.type: ModelError")
        );
        assert_eq!(run.transcript.turns[0].reply, "");
        assert!(run.content_complete, "{:?}", run.notes);
        assert!(run.notes.is_empty());
    }

    #[test]
    fn a_failed_model_call_without_an_error_type_is_incomplete() {
        // The legacy schema records no `error.type`.
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, Some(asked(&["hi"])), None),
        ]);
        let run = only(&t);
        assert_eq!(run.transcript.error, None);
        assert!(!run.content_complete);
        assert_eq!(run.notes.len(), 1, "{:?}", run.notes);
        assert!(
            run.notes[0].starts_with("generate_content span m1 has no gen_ai.output.messages"),
            "{}",
            run.notes[0]
        );
    }

    #[test]
    fn the_error_comes_from_the_root_else_the_earliest_sub_agent() {
        let mut spans = vec![
            agent("a1", None, "support", "inv-1", 0),
            with(
                agent("a2", Some("a1"), "billing", "inv-1", 2),
                ERROR_TYPE,
                "ToolError",
            ),
            with(
                agent("a3", Some("a1"), "search", "inv-1", 1),
                ERROR_TYPE,
                "Timeout",
            ),
            model(
                "m1",
                "a3",
                1,
                Some(asked(&["hi"])),
                Some(answered(&["hello"])),
            ),
        ];
        assert_eq!(
            only(&trace(spans.clone())).transcript.error.as_deref(),
            Some("error.type: Timeout")
        );
        spans[0] = with(spans[0].clone(), ERROR_TYPE, "AgentError");
        assert_eq!(
            only(&trace(spans)).transcript.error.as_deref(),
            Some("error.type: AgentError")
        );
    }

    #[test]
    fn a_nested_invoke_agent_of_another_invocation_is_its_own_run() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["ask the specialist"])),
                Some(answered(&["Asked."])),
            ),
            agent("b1", Some("a1"), "specialist", "inv-2", 2),
            model(
                "m2",
                "b1",
                3,
                Some(asked(&["specialist question"])),
                Some(answered(&["Specialist answer."])),
            ),
            with(
                tool("t2", "b1", 4, "search"),
                TOOL_ARGUMENTS,
                r#"{"q":"x"}"#,
            ),
        ]);
        let runs = build_runs(&t);
        let got: Vec<(&str, &str, &str, usize)> = runs
            .iter()
            .map(|r| {
                let turn = &r.transcript.turns[0];
                (
                    r.invocation_id.as_str(),
                    r.agent.as_str(),
                    turn.reply.as_str(),
                    turn.calls.len(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("inv-1", "support", "Asked.", 0),
                ("inv-2", "specialist", "Specialist answer.", 1),
            ]
        );
        assert_eq!(runs[1].started_at, at(T0 + 2));
    }

    #[test]
    fn a_run_without_a_model_call_is_incomplete() {
        let run = only(&trace(vec![agent("a1", None, "support", "inv-1", 0)]));
        assert!(!run.content_complete);
        assert!(run.tools_complete);
        assert_eq!(run.notes, ["the run has no generate_content span"]);
        let turn = &run.transcript.turns[0];
        assert_eq!((turn.user.as_str(), turn.reply.as_str()), ("", ""));
    }

    #[test]
    fn a_call_without_a_tool_name_is_skipped_with_a_note() {
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model(
                "m1",
                "a1",
                1,
                Some(asked(&["hi"])),
                Some(answered(&["hello"])),
            ),
            span(
                "t1",
                Some("a1"),
                "execute_tool",
                2,
                &[(OPERATION, EXECUTE_TOOL)],
            ),
        ]);
        let run = only(&t);
        assert!(run.transcript.turns[0].calls.is_empty());
        assert!(run.content_complete);
        assert!(!run.tools_complete);
        assert_eq!(run.notes, ["execute_tool span t1 has no gen_ai.tool.name"]);
    }

    #[test]
    fn empty_user_text_and_empty_reply_are_notes() {
        let input = json!([{"role": "user", "parts": [
            {"type": "blob", "modality": "image", "content": "aGk="}
        ]}]);
        let output = json!([{"role": "assistant", "parts": [
            {"type": "reasoning", "content": "hmm"}
        ]}]);
        let t = trace(vec![
            agent("a1", None, "support", "inv-1", 0),
            model("m1", "a1", 1, Some(input), Some(output)),
        ]);
        let run = only(&t);
        assert!(!run.content_complete);
        assert_eq!(
            run.notes,
            [
                "the user message has no text",
                "the run has no reply text and no error",
            ]
        );
    }

    #[test]
    fn cyclic_parent_links_do_not_loop() {
        let t = trace(vec![
            agent("a1", Some("a2"), "x", "inv-1", 0),
            agent("a2", Some("a1"), "y", "inv-1", 1),
        ]);
        assert!(build_runs(&t).is_empty());
    }
}
