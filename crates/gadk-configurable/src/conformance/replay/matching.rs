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

//! Go `internal/configurable/conformance/replayplugin/replay_plugin.go`'s request and tool-call
//! matching: `verifyLLMRequestMatch` (go-cmp with ignored fields and `EquateEmpty`),
//! `canonicalizeRequestContents`, the JSON-substring regexes, `modifyString`,
//! `canonicalizeJSONSubstrings`, `verifyToolCallMatch` and `normalizeDescription`. Errors are
//! Go's texts (the plugin wraps them in `AgentError`); a mismatch's diff body is a gadk
//! field-path diff instead of go-cmp's (ledgered).

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::LazyLock;

use gadk_genai::{Content, GenerateContentConfig};
use gadk_goyaml::GoValue;
use gadk_model::LlmRequest;
use regex::{Captures, Regex};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::conformance::recording::{LlmRequestView, RecordedFunctionCall};
use crate::govalue::{
    GoMap, go_sprint, go_type_name, go_value_to_json, json_map_to_go, json_to_go_value,
};

/// The description Go forces on the expected `transfer_to_agent` declaration.
const TRANSFER_TO_AGENT_DESCRIPTION: &str = "Transfer the question to another agent.
This tool hands off control to another agent when it's more suitable to answer the user's question according to the agent's description.";

/// The fields of Go's `model.LLMRequest` that `cmp.Diff` compares (`Tools` is ignored). Go's
/// `Config.HTTPOptions` has no gadk counterpart (gadk keeps HTTP options on the request; sanitize
/// clears them on the recorded side), so neither side's HTTP options take part.
// Divergence (ledger 6c-2): Go compares the live `Config.HTTPOptions` against the recorded nil;
// gadk's `LlmRequest.http_options` is ignored.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct RequestFields {
    model: String,
    contents: Vec<Content>,
    config: Option<GenerateContentConfig>,
}

impl RequestFields {
    /// Go's `cmpopts.IgnoreFields` and `EquateEmpty` applied up front: function declarations'
    /// schemas and config labels cleared, an empty `ContinuationToken` (Go `[]byte`) equal to an
    /// absent one, and every declaration description normalized (`transfer_to_agent` forced on
    /// the expected side).
    fn normalized(
        model: &str,
        contents: &[Content],
        config: Option<&GenerateContentConfig>,
        expected: bool,
    ) -> RequestFields {
        let mut config = config.cloned();
        if let Some(cfg) = config.as_mut() {
            cfg.labels.clear();
            if cfg
                .continuation_token
                .as_ref()
                .is_some_and(|t| t.is_empty())
            {
                cfg.continuation_token = None;
            }
            for tool in &mut cfg.tools {
                for decl in &mut tool.function_declarations {
                    decl.parameters = None;
                    decl.parameters_json_schema = None;
                    decl.response = None;
                    decl.response_json_schema = None;
                    decl.description = normalize_description(&decl.description);
                    if expected && decl.name == "transfer_to_agent" {
                        decl.description = TRANSFER_TO_AGENT_DESCRIPTION.to_owned();
                    }
                }
            }
        }
        RequestFields {
            model: model.to_owned(),
            contents: contents.to_vec(),
            config,
        }
    }
}

/// Go `verifyLLMRequestMatch`: `LLM request mismatch for agent '%s' (index %d):\n%s` unless the
/// requests are equal (ignoring function declarations' schemas, request tools and config
/// labels, with `EquateEmpty` and normalized descriptions), directly or after
/// `canonicalizeRequestContents` on both sides.
///
/// Go normalizes and canonicalizes both requests in place; gadk compares copies.
// Divergence (ledger 6c-2): Go's in-place edits (descriptions, canonicalized texts) are visible
// on the live request afterwards; gadk leaves the live request untouched.
pub(crate) fn verify_llm_request_match(
    expected: &LlmRequestView,
    actual: &LlmRequest,
    agent_name: &str,
    agent_index: i64,
) -> Result<(), String> {
    // Go: replay_plugin.go verifyLLMRequestMatch
    let mut exp = RequestFields::normalized(
        &expected.model,
        &expected.contents,
        expected.config.as_ref(),
        true,
    );
    let mut act = RequestFields::normalized(
        &actual.model,
        &actual.contents,
        actual.config.as_ref(),
        false,
    );
    if exp != act {
        // If initial comparison fails due to string formatting (e.g., Python vs Go JSON
        // whitespace or quote styles), normalize embedded JSON structures across both expected
        // and actual requests before re-comparing.
        canonicalize_request_contents(&mut exp);
        canonicalize_request_contents(&mut act);
        if exp != act {
            return Err(format!(
                "LLM request mismatch for agent '{agent_name}' (index {agent_index}):\n{}",
                request_diff(&exp, &act)
            ));
        }
    }
    Ok(())
}

/// Go `canonicalizeRequestContents`: every non-empty text part of the contents and of the
/// config's system instruction through [`canonicalize_json_substrings`].
fn canonicalize_request_contents(req: &mut RequestFields) {
    // Go: replay_plugin.go canonicalizeRequestContents
    for content in &mut req.contents {
        for part in &mut content.parts {
            if !part.text.is_empty() {
                part.text = canonicalize_json_substrings(&part.text);
            }
        }
    }
    if let Some(si) = req
        .config
        .as_mut()
        .and_then(|c| c.system_instruction.as_mut())
    {
        for part in &mut si.parts {
            if !part.text.is_empty() {
                part.text = canonicalize_json_substrings(&part.text);
            }
        }
    }
}

// Go's regexes on RE2 semantics: `\s` is `[\t\n\f\r ]` (no `\v`, unlike the regex crate's ASCII
// `\s`), `\b` is an ASCII word boundary, `(?i)` folds case with Unicode simple folding (both).
/// Go `dataBlockRegex`: `parameters:` or `result:` followed by a JSON-like object/array.
static DATA_BLOCK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(parameters|result):[\t\n\f\r ]*([\{\[].*[\}\]])")
        .unwrap_or_else(|e| unreachable!("{e}"))
});
/// Go `jsonCandidateRegex`: any standalone or embedded JSON-like object/array.
static JSON_CANDIDATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([\{\[].*[\}\]])").unwrap_or_else(|e| unreachable!("{e}")));
/// Go `quoteRegex`: `'key'` or `'value'`.
static QUOTE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'([^']*)'").unwrap_or_else(|e| unreachable!("{e}")));
/// Go `nullRegex`: Python's `None`.
static NULL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)None(?-u:\b)").unwrap_or_else(|e| unreachable!("{e}")));
/// Go `boolRegex`: Python's `True` / `False`.
static BOOL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)(True|False)(?-u:\b)").unwrap_or_else(|e| unreachable!("{e}"))
});

/// Go's Python-isms → JSON-isms step: single quotes → double quotes, `None` → `null`,
/// `True`/`False` → lowercase.
fn pythonic_to_json(raw: &str) -> String {
    let normalized = QUOTE_REGEX.replace_all(raw, "\"$1\"");
    let normalized = NULL_REGEX.replace_all(&normalized, "null");
    BOOL_REGEX
        .replace_all(&normalized, |c: &Captures<'_>| c[0].to_lowercase())
        .into_owned()
}

/// Go `json.Unmarshal(text, &parsed)` into `any` followed by `json.Marshal(parsed)`: numbers
/// become `float64`, keys sorted, Go's escaping; `None` where Go's `Unmarshal` fails.
fn go_json_round_trip(text: &str) -> Option<String> {
    // Divergence (ledger 6c-2): serde_json refuses nesting deeper than 128 levels, Go's decoder
    // allows 10000; such text stays as it is here.
    let parsed: Value = serde_json::from_str(&replace_lone_surrogates(text)).ok()?;
    let go = go_value_to_json(&json_to_go_value(&parsed)).ok()?;
    let bytes = gadk_model::to_go_json(&go).ok()?;
    String::from_utf8(bytes).ok()
}

/// Go's JSON decoder turns a `\uXXXX` escape of an unpaired UTF-16 surrogate into U+FFFD;
/// serde_json rejects it. Rewrites each such escape inside a string literal to `\ufffd`.
fn replace_lone_surrogates(text: &str) -> Cow<'_, str> {
    if !text.contains("\\u") {
        return Cow::Borrowed(text);
    }
    fn hex4(b: &[u8]) -> Option<u32> {
        let s = std::str::from_utf8(b.get(..4)?).ok()?;
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(s, 16).ok()
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut start, mut in_string) = (0, 0, false);
    while i < b.len() {
        match b[i] {
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'\\' if in_string => {
                if b.get(i + 1) != Some(&b'u') {
                    i += 2;
                    continue;
                }
                let Some(code) = hex4(&b[i + 2..]) else {
                    i += 2;
                    continue;
                };
                let paired = (0xD800..=0xDBFF).contains(&code)
                    && b.get(i + 6..i + 8) == Some(b"\\u")
                    && hex4(&b[i + 8..]).is_some_and(|lo| (0xDC00..=0xDFFF).contains(&lo));
                if paired {
                    i += 12;
                } else if (0xD800..=0xDFFF).contains(&code) {
                    out.push_str(&text[start..i]);
                    out.push_str("\\ufffd");
                    i += 6;
                    start = i;
                } else {
                    i += 6;
                }
            }
            _ => i += 1,
        }
    }
    if start == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[start..]);
    Cow::Owned(out)
}

/// Go `modifyString`: canonicalizes the JSON after each `parameters:` / `result:` label.
pub(crate) fn modify_string(input: &str) -> String {
    // Go: replay_plugin.go modifyString
    DATA_BLOCK_REGEX
        .replace_all(input, |c: &Captures<'_>| {
            let full_match = &c[0];
            // Split label (e.g., "parameters:") from the data (e.g., "{'a': 1}")
            let Some(parts) = DATA_BLOCK_REGEX.captures(full_match) else {
                return full_match.to_owned();
            };
            let (Some(label), Some(raw_data)) = (parts.get(1), parts.get(2)) else {
                return full_match.to_owned();
            };
            match go_json_round_trip(&pythonic_to_json(raw_data.as_str())) {
                Some(fixed) => format!("{}: {fixed}", label.as_str()),
                // If it's still not valid JSON, return the original match to avoid corruption
                None => full_match.to_owned(),
            }
        })
        .into_owned()
}

/// Go `canonicalizeJSONSubstrings`: [`modify_string`], then every JSON-like candidate
/// re-marshalled with Go's `json.Marshal` (sorted keys, Go escaping).
pub(crate) fn canonicalize_json_substrings(input: &str) -> String {
    // Go: replay_plugin.go canonicalizeJSONSubstrings
    let s = modify_string(input);
    JSON_CANDIDATE_REGEX
        .replace_all(&s, |c: &Captures<'_>| {
            let raw_data = &c[0];
            go_json_round_trip(&pythonic_to_json(raw_data)).unwrap_or_else(|| raw_data.to_owned())
        })
        .into_owned()
}

/// Go `verifyToolCallMatch`: `tool name mismatch for agent '%s' (index %d): expected '%s', got
/// '%s'`, `tool args mismatch for agent '%s' (index %d):\n%s` (Go `cmp.Diff` over
/// yaml-typed recorded args vs the live args as Go's JSON-decoded `map[string]any`).
pub(crate) fn verify_tool_call_match(
    expected: &RecordedFunctionCall,
    tool_name: &str,
    tool_args: &Map<String, Value>,
    agent_name: &str,
    agent_index: i64,
) -> Result<(), String> {
    // Go: replay_plugin.go verifyToolCallMatch
    if expected.name != tool_name {
        return Err(format!(
            "tool name mismatch for agent '{agent_name}' (index {agent_index}): expected '{}', got '{tool_name}'",
            expected.name
        ));
    }
    // Divergence (ledger 6c-2): gadk's live args are never Go's nil map, so recorded `nil`
    // args always differ from them (Go: equal only when the live map is nil too).
    let actual = json_map_to_go(tool_args);
    let equal = match &expected.args {
        Some(exp) => go_map_eq(exp, &actual),
        None => false,
    };
    if !equal {
        return Err(format!(
            "tool args mismatch for agent '{agent_name}' (index {agent_index}):\n{}",
            args_diff(expected.args.as_ref(), &actual)
        ));
    }
    Ok(())
}

/// Go `normalizeDescription`: each line trimmed, leading and trailing empty lines dropped.
pub(crate) fn normalize_description(desc: &str) -> String {
    // Go: replay_plugin.go normalizeDescription
    // Go strings.TrimSpace and str::trim strip the same set (Unicode White_Space).
    let mut cleaned: Vec<&str> = desc.split('\n').map(str::trim).collect();
    // Remove empty lines at the start and end
    while cleaned.first() == Some(&"") {
        cleaned.remove(0);
    }
    while cleaned.last() == Some(&"") {
        cleaned.pop();
    }
    cleaned.join("\n")
}

/// Go `cmp.Equal` of two `map[string]any` without options: same keys, equal values.
fn go_map_eq(a: &GoMap, b: &GoMap) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| go_value_eq(v, w)))
}

/// Go `cmp.Equal` of two yaml/JSON-decoded `any` values: dynamic types must match (`int` and
/// `float64` differ), floats compare with `==`, `time.Time` with `Equal` (same instant), maps
/// by key whatever their order.
fn go_value_eq(a: &GoValue, b: &GoValue) -> bool {
    match (a, b) {
        (GoValue::Null, GoValue::Null) => true,
        (GoValue::Bool(x), GoValue::Bool(y)) => x == y,
        (GoValue::Int(x), GoValue::Int(y)) => x == y,
        (GoValue::Uint(x), GoValue::Uint(y)) => x == y,
        (GoValue::Float(x), GoValue::Float(y)) => x == y,
        (GoValue::String(x), GoValue::String(y)) => x == y,
        (GoValue::Bytes(x), GoValue::Bytes(y)) => x == y,
        (GoValue::String(x), GoValue::Bytes(y)) | (GoValue::Bytes(y), GoValue::String(x)) => {
            x.as_bytes() == y.as_slice()
        }
        (GoValue::Timestamp(x), GoValue::Timestamp(y)) => x == y,
        (GoValue::Seq(x), GoValue::Seq(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| go_value_eq(v, w))
        }
        (GoValue::Map(x), GoValue::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(l, w)| k == l && go_value_eq(v, w)))
        }
        (GoValue::MapAny(x), GoValue::MapAny(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .any(|(l, w)| go_value_eq(k, l) && go_value_eq(v, w))
                })
        }
        _ => false,
    }
}

// The field-path diff that replaces go-cmp's report in mismatch errors.
// Divergence (ledger 6c-2): Go's body is `cmp.Diff`'s text; gadk writes one `-<path>: <expected>`
// / `+<path>: <actual>` line pair per differing leaf (a side's line is left out when the value
// is absent there).

/// The diff body of two unequal requests over their gadk JSON form (serde field names).
fn request_diff(exp: &RequestFields, act: &RequestFields) -> String {
    let mut out = String::new();
    let (e, a) = (
        serde_json::to_value(exp).unwrap_or(Value::Null),
        serde_json::to_value(act).unwrap_or(Value::Null),
    );
    json_diff("", Some(&e), Some(&a), &mut out);
    if out.is_empty() {
        // Only values the JSON form cannot tell apart differ (NaN floats).
        out.push_str("(the requests differ in values that render identically)\n");
    }
    out
}

/// `{path}.{key}`, or `key` at the root.
fn field_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn json_diff(path: &str, exp: Option<&Value>, act: Option<&Value>, out: &mut String) {
    match (exp, act) {
        (Some(Value::Object(e)), Some(Value::Object(a))) => {
            let mut keys: Vec<&String> = e.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                json_diff(&field_path(path, k), e.get(k), a.get(k), out);
            }
        }
        (Some(Value::Array(e)), Some(Value::Array(a))) => {
            for i in 0..e.len().max(a.len()) {
                json_diff(&format!("{path}[{i}]"), e.get(i), a.get(i), out);
            }
        }
        _ if exp == act => {}
        _ => {
            if let Some(e) = exp {
                let _ = writeln!(out, "-{path}: {e}");
            }
            if let Some(a) = act {
                let _ = writeln!(out, "+{path}: {a}");
            }
        }
    }
}

/// The diff body of unequal tool args (`args["key"][0]` paths, leaves as Go `type(value)`).
fn args_diff(exp: Option<&GoMap>, act: &GoMap) -> String {
    let mut out = String::new();
    match exp {
        None => {
            out.push_str("-args: map[string]interface {}(nil)\n");
            let _ = writeln!(out, "+args: {}", go_repr_map(act));
        }
        Some(e) => {
            let mut keys: Vec<&String> = e.keys().chain(act.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                go_diff(
                    &format!("args[{}]", gadk_platform::go_quote(k)),
                    e.get(k),
                    act.get(k),
                    &mut out,
                );
            }
        }
    }
    out
}

fn go_diff(path: &str, exp: Option<&GoValue>, act: Option<&GoValue>, out: &mut String) {
    match (exp, act) {
        (Some(GoValue::Map(e)), Some(GoValue::Map(a))) => {
            let mut keys: Vec<&String> = e.iter().chain(a.iter()).map(|(k, _)| k).collect();
            keys.sort();
            keys.dedup();
            let get = |m: &'_ [(String, GoValue)], k: &str| -> Option<GoValue> {
                m.iter().find(|(l, _)| l == k).map(|(_, v)| v.clone())
            };
            for k in keys {
                go_diff(
                    &format!("{path}[{}]", gadk_platform::go_quote(k)),
                    get(e, k).as_ref(),
                    get(a, k).as_ref(),
                    out,
                );
            }
        }
        (Some(GoValue::Seq(e)), Some(GoValue::Seq(a))) => {
            for i in 0..e.len().max(a.len()) {
                go_diff(&format!("{path}[{i}]"), e.get(i), a.get(i), out);
            }
        }
        (Some(e), Some(a)) if go_value_eq(e, a) => {}
        _ => {
            if let Some(e) = exp {
                let _ = writeln!(out, "-{path}: {}", go_repr(e));
            }
            if let Some(a) = act {
                let _ = writeln!(out, "+{path}: {}", go_repr(a));
            }
        }
    }
}

/// A leaf as Go `%T(%v)`, strings quoted.
fn go_repr(v: &GoValue) -> String {
    match v {
        GoValue::String(s) => gadk_platform::go_quote(s),
        GoValue::Bytes(b) => gadk_platform::go_quote_bytes(b),
        GoValue::Null => "<nil>".to_owned(),
        _ => format!("{}({})", go_type_name(v), go_sprint(v)),
    }
}

fn go_repr_map(m: &GoMap) -> String {
    let v = GoValue::Map(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    go_repr(&v)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use gadk_genai::{FunctionCall, FunctionDeclaration, Part, Schema, Tool};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn text(role: &str, t: &str) -> Content {
        Content {
            role: role.to_owned(),
            parts: vec![Part {
                text: t.to_owned(),
                ..Part::default()
            }],
        }
    }

    fn decl(name: &str, description: &str) -> FunctionDeclaration {
        FunctionDeclaration {
            name: name.to_owned(),
            description: description.to_owned(),
            ..FunctionDeclaration::default()
        }
    }

    fn config(decls: Vec<FunctionDeclaration>) -> GenerateContentConfig {
        GenerateContentConfig {
            tools: vec![Tool {
                function_declarations: decls,
                ..Tool::default()
            }],
            ..GenerateContentConfig::default()
        }
    }

    fn view(contents: Vec<Content>, config: Option<GenerateContentConfig>) -> LlmRequestView {
        LlmRequestView {
            model: "gemini".to_owned(),
            contents,
            config,
            tools: GoMap::new(),
        }
    }

    fn request(contents: Vec<Content>, config: Option<GenerateContentConfig>) -> LlmRequest {
        LlmRequest {
            model: "gemini".to_owned(),
            contents,
            config,
            ..LlmRequest::default()
        }
    }

    fn check(exp: &LlmRequestView, act: &LlmRequest) -> Result<(), String> {
        verify_llm_request_match(exp, act, "agent", 2)
    }

    /// `canonicalize.json` (Task 2's probe): Go `modifyString` and `canonicalizeJSONSubstrings`
    /// of 40 inputs.
    #[test]
    fn canonicalize_fixture_matches_go() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/canonicalize.json"
        );
        let rows: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(rows.len(), 40);
        for row in rows {
            let input = row["input"].as_str().unwrap();
            assert_eq!(
                modify_string(input),
                row["modify"].as_str().unwrap(),
                "modify {input:?}"
            );
            assert_eq!(
                canonicalize_json_substrings(input),
                row["output"].as_str().unwrap(),
                "canonicalize {input:?}"
            );
        }
    }

    /// Go-checked edge cases outside the fixture: RE2's `\s` has no `\v`; `(?i)` folds `ſ` to
    /// `s`; `-0` stays `-0`; unpaired surrogate escapes decode to U+FFFD (an escaped backslash
    /// is not an escape); two top-level values are not one JSON text.
    #[test]
    fn canonicalize_edge_cases_match_go() {
        let cases = [
            (
                "parameters:\u{b}{'a': 1}",
                "parameters:\u{b}{'a': 1}",
                "parameters:\u{b}{\"a\":1}",
            ),
            (
                "parameterſ: {'a': 1}",
                "parameterſ: {\"a\":1}",
                "parameterſ: {\"a\":1}",
            ),
            ("[-0]", "[-0]", "[-0]"),
            (
                r#"{"a": "\udc00x\ud800A😀"}"#,
                r#"{"a": "\udc00x\ud800A😀"}"#,
                "{\"a\":\"\u{fffd}x\u{fffd}A😀\"}",
            ),
            (r#"["\\ud800"]"#, r#"["\\ud800"]"#, r#"["\\ud800"]"#),
            ("[1] [2]", "[1] [2]", "[1] [2]"),
            ("{'a': 1.0e400}", "{'a': 1.0e400}", "{'a': 1.0e400}"),
        ];
        for (input, modify, output) in cases {
            assert_eq!(modify_string(input), modify, "modify {input:?}");
            assert_eq!(
                canonicalize_json_substrings(input),
                output,
                "canonicalize {input:?}"
            );
        }
    }

    #[test]
    fn equal_requests_match() {
        let contents = vec![text("user", "hi")];
        let cfg = config(vec![decl("f", "does f")]);
        assert_eq!(
            check(
                &view(contents.clone(), Some(cfg.clone())),
                &request(contents, Some(cfg))
            ),
            Ok(())
        );
    }

    /// `IgnoreFields(FunctionDeclaration, ParametersJsonSchema, ResponseJsonSchema,
    /// Parameters, Response)`, `IgnoreFields(LLMRequest, Tools)`,
    /// `IgnoreFields(GenerateContentConfig, Labels)`, and gadk's request HTTP options.
    #[test]
    fn ignored_fields_do_not_count() {
        let mut exp_cfg = config(vec![decl("f", "d")]);
        let mut act_cfg = exp_cfg.clone();
        let d = &mut act_cfg.tools[0].function_declarations[0];
        d.parameters = Some(Schema::default());
        d.parameters_json_schema = Some(json!({"type": "object"}));
        d.response = Some(Schema::default());
        d.response_json_schema = Some(json!({"type": "string"}));
        act_cfg.labels.insert("k".to_owned(), "v".to_owned());
        exp_cfg.labels.insert("other".to_owned(), "x".to_owned());
        let mut exp = view(vec![], Some(exp_cfg));
        exp.tools
            .insert("f".to_owned(), GoValue::String("tool".to_owned()));
        let mut act = request(vec![], Some(act_cfg));
        act.tools.insert("g".to_owned(), Arc::new(1_u8));
        act.http_options = Some(gadk_model::HttpOptions::default());
        assert_eq!(check(&exp, &act), Ok(()));
    }

    /// Each ignored declaration field alone, and a non-ignored one (`behavior`) still counts.
    #[test]
    fn each_ignored_declaration_field() {
        let base = config(vec![decl("f", "d")]);
        let edits: [fn(&mut FunctionDeclaration); 4] = [
            |d| d.parameters = Some(Schema::default()),
            |d| d.parameters_json_schema = Some(json!(true)),
            |d| d.response = Some(Schema::default()),
            |d| d.response_json_schema = Some(json!(false)),
        ];
        for edit in edits {
            let mut act = base.clone();
            edit(&mut act.tools[0].function_declarations[0]);
            assert_eq!(
                check(
                    &view(vec![], Some(base.clone())),
                    &request(vec![], Some(act))
                ),
                Ok(())
            );
        }
        let mut act = base.clone();
        act.tools[0].function_declarations[0].name = "g".to_owned();
        assert!(check(&view(vec![], Some(base)), &request(vec![], Some(act))).is_err());
    }

    /// `EquateEmpty`: an empty `[]byte` equals a nil one (gadk's `Vec`/maps have no nil), but a
    /// nil config pointer still differs from an empty config.
    #[test]
    fn equate_empty() {
        let exp = GenerateContentConfig {
            continuation_token: Some(Default::default()),
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            check(
                &view(vec![], Some(exp)),
                &request(vec![], Some(GenerateContentConfig::default()))
            ),
            Ok(())
        );
        assert_eq!(
            check(
                &view(vec![], None),
                &request(vec![], Some(GenerateContentConfig::default()))
            ),
            Err(
                "LLM request mismatch for agent 'agent' (index 2):\n-config: null\n+config: {}\n"
                    .to_owned()
            )
        );
    }

    #[test]
    fn descriptions_are_normalized_on_both_sides() {
        let exp = config(vec![decl("f", "\n  Line 1  \n  Line 2\n")]);
        let act = config(vec![decl("f", "Line 1\nLine 2  \n\n")]);
        assert_eq!(
            check(&view(vec![], Some(exp)), &request(vec![], Some(act))),
            Ok(())
        );
        let exp = config(vec![decl("f", "one")]);
        let act = config(vec![decl("f", "two")]);
        assert_eq!(
            check(&view(vec![], Some(exp)), &request(vec![], Some(act))),
            Err("LLM request mismatch for agent 'agent' (index 2):\n\
                 -config.tools[0].functionDeclarations[0].description: \"one\"\n\
                 +config.tools[0].functionDeclarations[0].description: \"two\"\n"
                .to_owned())
        );
    }

    /// The expected `transfer_to_agent` description is replaced by Go's fixed text; the
    /// actual one is only normalized.
    #[test]
    fn transfer_to_agent_description_is_fixed_on_the_expected_side() {
        let exp = config(vec![decl("transfer_to_agent", "whatever Python wrote")]);
        let act = config(vec![decl(
            "transfer_to_agent",
            &format!("  {TRANSFER_TO_AGENT_DESCRIPTION}\n"),
        )]);
        assert_eq!(
            check(
                &view(vec![], Some(exp.clone())),
                &request(vec![], Some(act))
            ),
            Ok(())
        );
        let act = config(vec![decl("transfer_to_agent", "whatever Python wrote")]);
        assert!(check(&view(vec![], Some(exp)), &request(vec![], Some(act))).is_err());
    }

    /// The canonicalization retry: Python-style JSON in a text part (contents and system
    /// instruction) matches Go's JSON; text that stays different still fails.
    #[test]
    fn canonicalization_retry() {
        let exp_cfg = GenerateContentConfig {
            system_instruction: Some(text("user", "ctx {'b': 1, 'a': None}")),
            ..GenerateContentConfig::default()
        };
        let act_cfg = GenerateContentConfig {
            system_instruction: Some(text("user", "ctx {\"a\": null, \"b\": 1}")),
            ..GenerateContentConfig::default()
        };
        let exp = view(
            vec![text(
                "user",
                "For context: tool returned result: {'ok': True, 'n': 2}",
            )],
            Some(exp_cfg),
        );
        let act = request(
            vec![text(
                "user",
                "For context: tool returned result: {\"n\":2,\"ok\":true}",
            )],
            Some(act_cfg),
        );
        assert_eq!(check(&exp, &act), Ok(()));

        let exp = view(vec![text("user", "result: {'a': 1}")], None);
        let act = request(vec![text("user", "result: {'a': 2}")], None);
        assert_eq!(
            check(&exp, &act),
            Err("LLM request mismatch for agent 'agent' (index 2):\n\
                 -contents[0].parts[0].text: \"result: {\\\"a\\\":1}\"\n\
                 +contents[0].parts[0].text: \"result: {\\\"a\\\":2}\"\n"
                .to_owned())
        );
    }

    #[test]
    fn model_and_contents_mismatch_diff() {
        let exp = view(vec![text("user", "a"), text("model", "b")], None);
        let mut act = request(vec![text("user", "a")], None);
        act.model = "other".to_owned();
        assert_eq!(
            check(&exp, &act),
            Err("LLM request mismatch for agent 'agent' (index 2):\n\
                 -contents[1]: {\"parts\":[{\"text\":\"b\"}],\"role\":\"model\"}\n\
                 -model: \"gemini\"\n\
                 +model: \"other\"\n"
                .to_owned())
        );
    }

    /// Function-call args in contents are JSON values: an integer and a float differ, as Go's
    /// `int` and `float64` do.
    #[test]
    fn content_args_number_kinds_differ() {
        let call = |v: Value| Content {
            role: "model".to_owned(),
            parts: vec![Part {
                function_call: Some(FunctionCall {
                    name: "f".to_owned(),
                    args: json!({"n": v}).as_object().unwrap().clone(),
                    ..FunctionCall::default()
                }),
                ..Part::default()
            }],
        };
        assert_eq!(
            check(
                &view(vec![call(json!(2))], None),
                &request(vec![call(json!(2))], None)
            ),
            Ok(())
        );
        assert!(
            check(
                &view(vec![call(json!(2))], None),
                &request(vec![call(json!(2.0))], None)
            )
            .is_err()
        );
    }

    fn recorded(name: &str, args: Option<GoMap>) -> RecordedFunctionCall {
        RecordedFunctionCall {
            name: name.to_owned(),
            args,
            ..RecordedFunctionCall::default()
        }
    }

    fn live(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn tool_name_mismatch() {
        assert_eq!(
            verify_tool_call_match(&recorded("a", None), "b", &Map::new(), "agent", 1),
            Err("tool name mismatch for agent 'agent' (index 1): expected 'a', got 'b'".to_owned())
        );
    }

    /// Recorded args keep yaml.v3's types; live JSON numbers are Go `float64`: `int(3)` differs
    /// from `float64(3)`, a float equals it.
    #[test]
    fn tool_args_int_and_float_are_distinct() {
        let exp: GoMap = [("n".to_owned(), GoValue::Int(3))].into();
        assert_eq!(
            verify_tool_call_match(
                &recorded("f", Some(exp)),
                "f",
                &live(json!({"n": 3})),
                "agent",
                1
            ),
            Err("tool args mismatch for agent 'agent' (index 1):\n\
                 -args[\"n\"]: int(3)\n\
                 +args[\"n\"]: float64(3)\n"
                .to_owned())
        );
        let exp: GoMap = [("n".to_owned(), GoValue::Float(3.0))].into();
        assert_eq!(
            verify_tool_call_match(
                &recorded("f", Some(exp)),
                "f",
                &live(json!({"n": 3})),
                "agent",
                1
            ),
            Ok(())
        );
    }

    /// No `EquateEmpty` for tool args: nil recorded args differ from the live (empty) map.
    #[test]
    fn tool_args_nil_differs_from_empty() {
        assert_eq!(
            verify_tool_call_match(&recorded("f", None), "f", &Map::new(), "agent", 0),
            Err("tool args mismatch for agent 'agent' (index 0):\n\
                 -args: map[string]interface {}(nil)\n\
                 +args: map[string]interface {}(map[])\n"
                .to_owned())
        );
        assert_eq!(
            verify_tool_call_match(
                &recorded("f", Some(GoMap::new())),
                "f",
                &Map::new(),
                "agent",
                0
            ),
            Ok(())
        );
    }

    /// Nested maps compare by key (yaml document order is irrelevant); nested paths and
    /// missing/extra keys appear in the diff.
    #[test]
    fn tool_args_nested() {
        let nested = GoValue::Map(vec![
            ("b".to_owned(), GoValue::String("x".to_owned())),
            (
                "a".to_owned(),
                GoValue::Seq(vec![GoValue::Bool(true), GoValue::Null]),
            ),
        ]);
        let exp: GoMap = [("m".to_owned(), nested)].into();
        let args = live(json!({"m": {"a": [true, null], "b": "x"}}));
        assert_eq!(
            verify_tool_call_match(&recorded("f", Some(exp.clone())), "f", &args, "g", 0),
            Ok(())
        );
        let args = live(json!({"m": {"a": [false], "c": 1}}));
        assert_eq!(
            verify_tool_call_match(&recorded("f", Some(exp)), "f", &args, "g", 0),
            Err("tool args mismatch for agent 'g' (index 0):\n\
                 -args[\"m\"][\"a\"][0]: bool(true)\n\
                 +args[\"m\"][\"a\"][0]: bool(false)\n\
                 -args[\"m\"][\"a\"][1]: <nil>\n\
                 -args[\"m\"][\"b\"]: \"x\"\n\
                 +args[\"m\"][\"c\"]: float64(1)\n"
                .to_owned())
        );
    }

    /// `time.Time` compares with `Equal` (same instant, any offset); NaN never equals itself.
    #[test]
    fn go_value_eq_semantics() {
        let t1 = chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z").unwrap();
        let t2 = chrono::DateTime::parse_from_rfc3339("2024-01-02T05:04:05+02:00").unwrap();
        assert!(go_value_eq(
            &GoValue::Timestamp(t1),
            &GoValue::Timestamp(t2)
        ));
        assert!(!go_value_eq(
            &GoValue::Float(f64::NAN),
            &GoValue::Float(f64::NAN)
        ));
        assert!(go_value_eq(&GoValue::Float(0.0), &GoValue::Float(-0.0)));
        assert!(!go_value_eq(&GoValue::Int(1), &GoValue::Uint(1)));
        assert!(go_value_eq(
            &GoValue::MapAny(vec![
                (GoValue::Int(1), GoValue::Null),
                (GoValue::Bool(true), GoValue::Int(2))
            ]),
            &GoValue::MapAny(vec![
                (GoValue::Bool(true), GoValue::Int(2)),
                (GoValue::Int(1), GoValue::Null)
            ]),
        ));
        assert!(!go_value_eq(
            &GoValue::MapAny(vec![(GoValue::Int(1), GoValue::Null)]),
            &GoValue::MapAny(vec![(GoValue::Float(1.0), GoValue::Null)]),
        ));
    }
}
