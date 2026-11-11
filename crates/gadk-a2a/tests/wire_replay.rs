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

//! Wire replay (spec R14): replays the ADK-Go A2A wire fixtures of `scripts/gen-go-a2a`
//! (`testdata/wire/<scenario>.json`) against gadk-a2a, both directions, exactly as
//! `scripts/gen-go-a2a/SCENARIO.md` ("Normalization", "Replay") describes, and compares after
//! applying the scenario's declared deltas (`testdata/wire/<scenario>.deltas.json`).
//!
//! - Server direction: each server unit's requests are sent, raw, to a gadk-a2a
//!   [`Executor`] behind a2a-rs's JSON-RPC and REST routers (the probe's layout), recorded by
//!   the same wrapper format the probe uses.
//! - Client direction: a fake server serves each client unit's recorded responses to a gadk
//!   remote agent ([`new_a2a`]) run by a gadk runner; the recorded requests and the yielded
//!   session events are compared.
//!
//! Deltas file format: `{"integral_floats": "<divergence>", "deltas": [{"unit", "path",
//! "fixture", "actual", "divergence"}]}`. `integral_floats`, when present, lets an integral float
//! on gadk-a2a's side (`5.0`) equal Go's integer (`5`).
//! `unit` is `server.jsonrpc`, `server.rest` or `client.<name>`; `path` a JSON pointer into the
//! normalized unit whose segments may be `*` (every array element or object member); `fixture`
//! is Go's value there (absent = Go has no such key) and `actual` gadk-a2a's (absent = gadk-a2a
//! has no such key); `divergence` cites the `docs/parity/5b.md` line. Each delta must match at
//! least one location, holding exactly `fixture`, or the replay fails.

#![cfg(all(feature = "server", feature = "client"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use futures::StreamExt as _;
use gadk_a2a::a2a;
use gadk_a2a::a2a_server::{
    DefaultRequestHandler, InMemoryTaskStore, StaticAgentCard, agent_card::agent_card_router,
    jsonrpc::jsonrpc_router, rest::rest_router,
};
use gadk_a2a::remoteagent::{A2AConfig, CardResolveOptions, new_a2a, new_agent_card_provider};
use gadk_a2a::server::{Executor, ExecutorConfig, OutputMode, build_agent_skills};
use gadk_agent::{
    Agent, AgentConfig, Context, EventStream, InvocationContext, RunConfig, StreamingMode,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm_agent;
use gadk_engine::loop_agent::{self, LoopAgentConfig};
use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, FunctionCall, GenerateContentResponseUsageMetadata, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

// ---------------------------------------------------------------------------------------------
// Normalization (SCENARIO.md "Normalization")
// ---------------------------------------------------------------------------------------------

const UUID_LEN: usize = 36;

/// Whether `s[i..]` starts with a UUID (`8-4-4-4-12` hex digits).
fn uuid_at(s: &[u8], i: usize) -> bool {
    if s.len() < i + UUID_LEN {
        return false;
    }
    s[i..i + UUID_LEN].iter().enumerate().all(|(k, &c)| {
        if matches!(k, 8 | 13 | 18 | 23) {
            c == b'-'
        } else {
            c.is_ascii_hexdigit()
        }
    })
}

fn is_uuid(s: &str) -> bool {
    s.len() == UUID_LEN && uuid_at(s.as_bytes(), 0)
}

/// Assigns placeholders to UUIDs in canonical order, one unit at a time.
#[derive(Default)]
struct Normalizer {
    assigned: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

impl Normalizer {
    fn placeholder(&mut self, uuid: &str, kind: &'static str) -> String {
        if let Some(p) = self.assigned.get(uuid) {
            return p.clone();
        }
        let n = self.counts.entry(kind).or_default();
        *n += 1;
        let p = format!("<{kind}-{n}>");
        self.assigned.insert(uuid.to_owned(), p.clone());
        p
    }

    fn string(&mut self, s: &str, key: &str, holder: &Map<String, Value>) -> String {
        if is_uuid(s) {
            let kind = match key {
                "taskId" | "a2a:task_id" | "referenceTaskIds" => "task",
                "id" if holder.contains_key("status") && holder.contains_key("contextId") => "task",
                "contextId" | "a2a:context_id" => "ctx",
                "messageId" => "msg",
                "artifactId" => "artifact",
                _ => "id",
            };
            return self.placeholder(s, kind);
        }
        let bytes = s.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        let mut last = 0;
        while i < bytes.len() {
            if uuid_at(bytes, i) {
                out.push_str(&s[last..i]);
                out.push_str(&self.placeholder(&s[i..i + UUID_LEN], "id"));
                i += UUID_LEN;
                last = i;
            } else {
                i += 1;
            }
        }
        out.push_str(&s[last..]);
        out
    }

    /// Visits `v` stored under `key` of `holder` (canonical order: sorted keys, depth first).
    fn visit(&mut self, v: &mut Value, key: &str, holder: &Map<String, Value>) {
        match v {
            Value::String(s) => *s = self.string(s, key, holder),
            Value::Array(items) => {
                for item in items {
                    self.visit(item, key, holder);
                }
            }
            Value::Object(map) => {
                let snapshot = map.clone();
                let mut keys: Vec<String> = map.keys().cloned().collect();
                keys.sort();
                for k in keys {
                    if let Some(child) = map.get_mut(&k) {
                        self.visit(child, &k, &snapshot);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Replaces live base URLs (longest first) in every string.
fn replace_bases(v: &mut Value, bases: &[(String, &str)]) {
    match v {
        Value::String(s) => {
            let mut sorted: Vec<&(String, &str)> = bases.iter().collect();
            sorted.sort_by_key(|(url, _)| std::cmp::Reverse(url.len()));
            for (url, placeholder) in sorted {
                if s.contains(url.as_str()) {
                    *s = s.replace(url.as_str(), placeholder);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| replace_bases(i, bases)),
        Value::Object(map) => map.values_mut().for_each(|i| replace_bases(i, bases)),
        _ => {}
    }
}

/// Every `timestamp` value becomes `"<t>"`.
fn replace_timestamps(v: &mut Value) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(replace_timestamps),
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if k == "timestamp" {
                    *child = Value::String("<t>".into());
                } else {
                    replace_timestamps(child);
                }
            }
        }
        _ => {}
    }
}

/// SCENARIO.md "Normalization", applied to one unit document.
fn normalize(unit: &Value, bases: &[(String, &str)]) -> Value {
    let mut v = unit.clone();
    replace_bases(&mut v, bases);
    replace_timestamps(&mut v);
    Normalizer::default().visit(&mut v, "", &Map::new());
    v
}

// ---------------------------------------------------------------------------------------------
// Replay: align / bind / denormalize (SCENARIO.md "Replay")
// ---------------------------------------------------------------------------------------------

const FIXED_TIME: &str = "2026-01-02T03:04:05Z";

/// One token of a fixture string: literal text or a `<kind-N>` placeholder.
enum Token {
    Lit(String),
    Ph(String),
}

/// Splits `s` into literal text and `<kind-N>` placeholders.
fn tokens(s: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        lit.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail.find('>').map(|end| &tail[..=end]) {
            Some(ph) if is_id_placeholder(ph) => {
                if !lit.is_empty() {
                    out.push(Token::Lit(std::mem::take(&mut lit)));
                }
                out.push(Token::Ph(ph.to_owned()));
                rest = &tail[ph.len()..];
            }
            _ => {
                lit.push('<');
                rest = &tail[1..];
            }
        }
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        out.push(Token::Lit(lit));
    }
    out
}

fn is_id_placeholder(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    let Some((kind, n)) = inner.rsplit_once('-') else {
        return false;
    };
    matches!(kind, "task" | "ctx" | "msg" | "artifact" | "id")
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
}

/// The placeholder bindings of one unit, and the base URLs replacing `<base>`/`<child-base>`.
#[derive(Default, Clone)]
struct Bindings {
    ids: HashMap<String, String>,
    bases: Vec<(String, &'static str)>,
}

impl Bindings {
    fn with_bases(bases: Vec<(String, &'static str)>) -> Self {
        Self {
            ids: HashMap::new(),
            bases,
        }
    }

    fn base_literal(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for (url, placeholder) in &self.bases {
            out = out.replace(placeholder, url);
        }
        out
    }

    /// Binds the placeholders of `pattern` to the UUIDs of `actual` when the anchored pattern
    /// matches it.
    fn bind_string(&mut self, pattern: &str, actual: &str) {
        let toks = tokens(&self.base_literal(pattern));
        if !toks.iter().any(|t| matches!(t, Token::Ph(_))) {
            return;
        }
        let bytes = actual.as_bytes();
        let mut pos = 0;
        let mut found = Vec::new();
        for t in &toks {
            match t {
                Token::Lit(l) => {
                    if !actual[pos..].starts_with(l.as_str()) {
                        return;
                    }
                    pos += l.len();
                }
                Token::Ph(p) => {
                    if !uuid_at(bytes, pos) {
                        return;
                    }
                    found.push((p.clone(), actual[pos..pos + UUID_LEN].to_owned()));
                    pos += UUID_LEN;
                }
            }
        }
        if pos != actual.len() {
            return;
        }
        for (p, v) in found {
            self.ids.entry(p).or_insert(v);
        }
    }

    /// Walks a normalized fixture value and an actual value in parallel, binding placeholders.
    fn align(&mut self, fixture: &Value, actual: &Value) {
        match (fixture, actual) {
            (Value::String(f), Value::String(a)) => self.bind_string(f, a),
            (Value::Array(f), Value::Array(a)) => {
                for (f, a) in f.iter().zip(a) {
                    self.align(f, a);
                }
            }
            (Value::Object(f), Value::Object(a)) => {
                for (k, fv) in f {
                    if let Some(av) = a.get(k) {
                        self.align(fv, av);
                    }
                }
            }
            _ => {}
        }
    }

    /// Replaces `"<t>"`, the base placeholders and every `<kind-N>` (unbound → a fresh UUID,
    /// remembered).
    fn denormalize(&mut self, v: &Value) -> Value {
        match v {
            Value::String(s) if s == "<t>" => Value::String(FIXED_TIME.into()),
            Value::String(s) => {
                let s = self.base_literal(s);
                let mut out = String::new();
                for t in tokens(&s) {
                    match t {
                        Token::Lit(l) => out.push_str(&l),
                        Token::Ph(p) => {
                            let v = self
                                .ids
                                .entry(p)
                                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
                            out.push_str(v);
                        }
                    }
                }
                Value::String(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|i| self.denormalize(i)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.denormalize(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Deltas and comparison
// ---------------------------------------------------------------------------------------------

/// The differing locations between two JSON values (pointer, expected, actual).
fn diff(path: &str, want: Option<&Value>, got: Option<&Value>, out: &mut Vec<String>) {
    match (want, got) {
        (Some(Value::Object(w)), Some(Value::Object(g))) => {
            let mut keys: Vec<&String> = w.keys().chain(g.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                diff(
                    &format!("{path}/{}", k.replace('~', "~0").replace('/', "~1")),
                    w.get(k),
                    g.get(k),
                    out,
                );
            }
        }
        (Some(Value::Array(w)), Some(Value::Array(g))) if w.len() == g.len() => {
            for (i, (w, g)) in w.iter().zip(g).enumerate() {
                diff(&format!("{path}/{i}"), Some(w), Some(g), out);
            }
        }
        (w, g) if w == g => {}
        (w, g) => out.push(format!(
            "{path}: fixture {} != actual {}",
            w.map_or("<absent>".into(), |v| v.to_string()),
            g.map_or("<absent>".into(), |v| v.to_string())
        )),
    }
}

/// One declared difference (see the module docs).
struct Delta {
    unit: String,
    path: Vec<String>,
    fixture: Option<Value>,
    actual: Option<Value>,
}

/// A scenario's declared deltas.
#[derive(Default)]
struct Deltas {
    /// Set (to the divergence it cites) when integral floats (`5.0`) on gadk-a2a's side compare
    /// equal to Go's integers (`5`).
    integral_floats: Option<String>,
    deltas: Vec<Delta>,
}

fn load_deltas(scenario: &str) -> Deltas {
    let path = testdata().join(format!("{scenario}.deltas.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Deltas::default();
    };
    let doc: Value = serde_json::from_str(&text).unwrap();
    let integral_floats = doc
        .get("integral_floats")
        .map(|d| d.as_str().unwrap().to_owned());
    let deltas = doc["deltas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            assert!(
                d["divergence"].as_str().is_some_and(|s| !s.is_empty()),
                "{scenario}: every delta cites a ledger divergence: {d}"
            );
            Delta {
                unit: d["unit"].as_str().unwrap().to_owned(),
                path: d["path"]
                    .as_str()
                    .unwrap()
                    .split('/')
                    .skip(1)
                    .map(|s| s.replace("~1", "/").replace("~0", "~"))
                    .collect(),
                fixture: d.get("fixture").cloned(),
                actual: d.get("actual").cloned(),
            }
        })
        .collect();
    Deltas {
        integral_floats,
        deltas,
    }
}

/// Rewrites every integral float (`5.0`) as an integer (`5`).
fn integral_floats_as_integers(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or_default();
            if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                *v = json!(f as i64);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(integral_floats_as_integers),
        Value::Object(map) => map.values_mut().for_each(integral_floats_as_integers),
        _ => {}
    }
}

/// Applies `delta` to `v` at every location its path names; returns the number of locations
/// that held the delta's fixture value.
fn apply_delta(v: &mut Value, path: &[String], delta: &Delta) -> usize {
    let Some((head, rest)) = path.split_first() else {
        return 0;
    };
    if rest.is_empty() {
        let Value::Object(map) = v else {
            if let (Value::Array(items), Ok(i), Some(actual)) =
                (&mut *v, head.parse::<usize>(), &delta.actual)
                && items.get(i) == delta.fixture.as_ref()
            {
                items[i] = actual.clone();
                return 1;
            }
            return 0;
        };
        let keys: Vec<String> = if head == "*" {
            map.keys().cloned().collect()
        } else {
            vec![head.clone()]
        };
        let mut n = 0;
        for k in keys {
            if map.get(&k) != delta.fixture.as_ref() {
                continue;
            }
            match &delta.actual {
                Some(a) => {
                    map.insert(k, a.clone());
                }
                None => {
                    map.remove(&k);
                }
            }
            n += 1;
        }
        return n;
    }
    match v {
        Value::Object(map) if head == "*" => {
            map.values_mut().map(|c| apply_delta(c, rest, delta)).sum()
        }
        Value::Object(map) => map.get_mut(head).map_or(0, |c| apply_delta(c, rest, delta)),
        Value::Array(items) if head == "*" => {
            items.iter_mut().map(|c| apply_delta(c, rest, delta)).sum()
        }
        Value::Array(items) => head
            .parse::<usize>()
            .ok()
            .and_then(|i| items.get_mut(i))
            .map_or(0, |c| apply_delta(c, rest, delta)),
        _ => 0,
    }
}

/// Compares a normalized actual unit with the normalized fixture unit after the unit's deltas;
/// `Err` lists every difference (and every delta that matched nothing).
fn compare_unit(
    unit: &str,
    fixture: &Value,
    actual: &Value,
    deltas: &Deltas,
) -> Result<(), String> {
    let mut want = fixture.clone();
    let mut actual = actual.clone();
    // Debug aid: `WIRE_REPLAY_DUMP=<dir>` writes every normalized actual unit there.
    if let Ok(dir) = std::env::var("WIRE_REPLAY_DUMP") {
        let file = PathBuf::from(dir).join(format!("{unit}.json"));
        std::fs::write(file, serde_json::to_vec_pretty(&actual).unwrap()).unwrap();
    }
    if deltas.integral_floats.is_some() {
        integral_floats_as_integers(&mut actual);
    }
    let mut problems = Vec::new();
    for d in deltas.deltas.iter().filter(|d| d.unit == unit) {
        if apply_delta(&mut want, &d.path, d) == 0 {
            problems.push(format!(
                "delta /{} matched nothing (stale delta)",
                d.path.join("/")
            ));
        }
    }
    diff("", Some(&want), Some(&actual), &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{unit}:\n  {}", problems.join("\n  ")))
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP recording (SCENARIO.md "Exchange")
// ---------------------------------------------------------------------------------------------

/// Go's `http.CanonicalHeaderKey`.
fn canonical_header(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for c in name.chars() {
        out.push(if upper {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        });
        upper = c == '-';
    }
    out
}

fn record_headers(headers: &http::HeaderMap, skip: &[&str]) -> Value {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        if skip.contains(&name.as_str()) {
            continue;
        }
        out.entry(canonical_header(name.as_str()))
            .or_default()
            .push(String::from_utf8_lossy(value.as_bytes()).into_owned());
    }
    Value::Object(
        out.into_iter()
            .map(|(k, v)| (k, Value::String(v.join(", "))))
            .collect(),
    )
}

const REQUEST_SKIP: &[&str] = &[
    "host",
    "content-length",
    "accept-encoding",
    "user-agent",
    "connection",
];
const RESPONSE_SKIP: &[&str] = &["date", "content-length", "transfer-encoding"];

/// A body as the fixture records it: parsed JSON, `null` when empty, else the raw string.
fn record_body(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// The `data:` payloads of an SSE body.
fn record_sse(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    Value::Array(
        text.split("\n\n")
            .filter_map(|frame| {
                let data: Vec<&str> = frame
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(|l| l.strip_prefix(' ').unwrap_or(l))
                    .collect();
                (!data.is_empty()).then(|| record_body(data.join("\n").as_bytes()))
            })
            .collect(),
    )
}

fn record_request(parts: &http::request::Parts, body: &[u8]) -> Value {
    json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path_and_query().map_or("/", |p| p.as_str()),
        "headers": record_headers(&parts.headers, REQUEST_SKIP),
        "body": record_body(body),
    })
}

fn record_response(status: u16, headers: &http::HeaderMap, body: &[u8]) -> Value {
    let is_sse = headers
        .get(http::header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"));
    let mut out = json!({
        "status": status,
        "headers": record_headers(headers, RESPONSE_SKIP),
        "body": if is_sse { Value::Null } else { record_body(body) },
    });
    if is_sse {
        out["sse"] = record_sse(body);
    }
    out
}

type Exchanges = Arc<Mutex<Vec<Value>>>;

/// Wraps `router` in a recorder appending one exchange per completed request.
fn recorded(router: axum::Router, exchanges: Exchanges) -> axum::Router {
    router.layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let exchanges = Arc::clone(&exchanges);
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let request = record_request(&parts, &body);
                let resp = next
                    .run(axum::extract::Request::from_parts(parts, Body::from(body)))
                    .await;
                let (parts, body) = resp.into_parts();
                let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let response = record_response(parts.status.as_u16(), &parts.headers, &body);
                exchanges
                    .lock()
                    .push(json!({"request": request, "response": response}));
                axum::response::Response::from_parts(parts, Body::from(body))
            }
        },
    ))
}

/// Sends one raw HTTP/1.1 request with exactly the fixture's headers (plus `Host`,
/// `Content-Length` and `Connection: close`, which the recorder ignores) and reads the whole
/// response.
async fn send_raw(base: &str, request: &Value) -> (u16, Vec<u8>) {
    let addr = base.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = match &request["body"] {
        Value::Null => Vec::new(),
        Value::String(s) => s.clone().into_bytes(),
        v => serde_json::to_vec(v).unwrap(),
    };
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n",
        request["method"].as_str().unwrap(),
        request["path"].as_str().unwrap()
    );
    for (name, value) in request["headers"].as_object().unwrap() {
        head.push_str(&format!("{name}: {}\r\n", value.as_str().unwrap()));
    }
    if !body.is_empty() || request["method"] == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    let mut body = raw[split + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = dechunk(&body);
    }
    (status, body)
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = raw.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size_text = String::from_utf8_lossy(&raw[..eol]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap().trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&raw[eol + 2..eol + 2 + size]);
        raw = &raw[eol + 2 + size + 2..];
    }
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Agents and models (SCENARIO.md "Agents and models")
// ---------------------------------------------------------------------------------------------

type Reply = dyn Fn(&LlmRequest, bool) -> Vec<Result<LlmResponse, ModelError>> + Send + Sync;

/// Go `scriptedModel`.
struct ScriptedModel {
    name: String,
    reply: Box<Reply>,
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> futures::stream::BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::iter((self.reply)(&req, stream)).boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn model(
    name: &str,
    reply: impl Fn(&LlmRequest, bool) -> Vec<Result<LlmResponse, ModelError>> + Send + Sync + 'static,
) -> Arc<dyn Llm> {
    Arc::new(ScriptedModel {
        name: name.into(),
        reply: Box::new(reply),
    })
}

/// Go `final(parts…)`.
fn final_(parts: Vec<Part>) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(Content::from_parts(parts, "model")),
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            ..Default::default()
        }),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })
}

/// Go `chunk(text)`.
fn chunk(text: &str) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(Content::from_parts(vec![Part::from_text(text)], "model")),
        partial: true,
        ..Default::default()
    })
}

fn call(name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The function responses of `req`'s last content.
fn last_responses(req: &LlmRequest) -> Vec<Map<String, Value>> {
    req.contents
        .last()
        .map(|c| {
            c.parts
                .iter()
                .filter_map(|p| p.function_response.as_ref().map(|r| r.response.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn tool(
    name: &str,
    description: &str,
    long_running: bool,
    result: Value,
) -> Arc<dyn gadk_agent::Tool> {
    let result = result.as_object().cloned().unwrap_or_default();
    FunctionTool::new(
        FunctionToolConfig {
            is_long_running: long_running,
            ..FunctionToolConfig::new(name, description)
        },
        move |_ctx: Context, _args: Map<String, Value>| {
            let result = result.clone();
            async move { Ok::<_, gadk_agent::ToolError>(result) }
        },
    )
    .unwrap()
}

fn llm(
    name: &str,
    description: &str,
    instruction: &str,
    model: Arc<dyn Llm>,
    tools: Vec<Arc<dyn gadk_agent::Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        instruction: instruction.into(),
        model: Some(model),
        tools,
        sub_agents,
        ..Default::default()
    })
    .unwrap()
}

fn assistant() -> Arc<dyn Agent> {
    llm(
        "assistant",
        "Answers greetings.",
        "Answer briefly.",
        model("scripted-assistant", |_, _| {
            vec![final_(vec![Part::from_text("Hello! How can I help?")])]
        }),
        vec![],
        vec![],
    )
}

fn weather() -> Arc<dyn Agent> {
    llm(
        "weather",
        "Reports the weather.",
        "Use get_weather to answer.",
        model("scripted-weather", |req, stream| {
            if !last_responses(req).is_empty() {
                let done = final_(vec![Part::from_text("It is sunny.")]);
                return if stream {
                    vec![chunk("It is "), chunk("sunny."), done]
                } else {
                    vec![done]
                };
            }
            let done = final_(vec![
                Part::from_text("Let me check."),
                call("get_weather", json!({"city": "Paris"})),
            ]);
            if stream {
                vec![chunk("Let me "), chunk("check."), done]
            } else {
                vec![done]
            }
        }),
        vec![tool(
            "get_weather",
            "Returns the forecast for a city.",
            false,
            json!({"forecast": "sunny"}),
        )],
        vec![],
    )
}

fn approver() -> Arc<dyn Agent> {
    llm(
        "approver",
        "Requests approval before acting.",
        "Ask for approval first.",
        model("scripted-approver", |req, _| {
            let responses = last_responses(req);
            if responses.is_empty() {
                vec![final_(vec![
                    Part::from_text("need to request approval first!"),
                    call("request_approval", json!({})),
                ])]
            } else if responses.len() == 1 && responses[0].get("status") == Some(&json!("pending"))
            {
                vec![final_(vec![Part::from_text(
                    "waiting for user's approval...",
                )])]
            } else {
                vec![final_(vec![Part::from_text("Task complete!")])]
            }
        }),
        vec![tool(
            "request_approval",
            "Request approval before proceeding.",
            true,
            json!({"status": "pending", "ticket_id": "ticket-1"}),
        )],
        vec![],
    )
}

fn broken() -> Arc<dyn Agent> {
    llm(
        "broken",
        "Always fails.",
        "Fail.",
        model("scripted-broken", |_, _| {
            vec![Err(ModelError::Other("scripted model failure".into()))]
        }),
        vec![],
        vec![],
    )
}

fn root(child_card: a2a::AgentCard) -> Arc<dyn Agent> {
    let child = new_a2a(A2AConfig {
        name: "child".into(),
        description: "Remote approver.".into(),
        agent_card: Some(child_card),
        ..Default::default()
    })
    .unwrap();
    llm(
        "root",
        "Delegates to the child agent.",
        "Transfer to child.",
        model("scripted-root", |_, _| {
            vec![final_(vec![
                Part::from_text("transferring to child"),
                call("transfer_to_agent", json!({"agent_name": "child"})),
            ])]
        }),
        vec![],
        vec![child],
    )
}

fn pipeline() -> Arc<dyn Agent> {
    let text_model = |name: &str, text: &'static str| {
        model(name, move |_, _| vec![final_(vec![Part::from_text(text)])])
    };
    let drafter = llm(
        "drafter",
        "Writes a draft.",
        "You write a draft of your reply.",
        text_model("scripted-drafter", "Draft ready."),
        vec![tool("lookup", "", false, json!({"found": true}))],
        vec![],
    );
    let fact_checker = llm(
        "fact_checker",
        "Checks facts.",
        "Check the facts.",
        text_model("scripted-fact-checker", "Facts checked."),
        vec![],
        vec![],
    );
    let checks = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "checks".into(),
            sub_agents: vec![fact_checker],
            ..Default::default()
        },
    })
    .unwrap();
    let editor = llm(
        "editor",
        "",
        "You're the editor; polish your draft.",
        text_model("scripted-editor", "Polished."),
        vec![],
        vec![],
    );
    let polisher = loop_agent::new(LoopAgentConfig {
        agent: AgentConfig {
            name: "polisher".into(),
            description: "Polishes the draft.".into(),
            sub_agents: vec![editor],
            ..Default::default()
        },
        max_iterations: 1,
    })
    .unwrap();
    let stamper = gadk_agent::new_agent(AgentConfig {
        name: "stamper".into(),
        run: Some(Arc::new(|ctx: InvocationContext| -> EventStream {
            let ro = ctx.as_readonly();
            let mut event = Event::new(ctx.platform(), ro.invocation_id());
            event.author = "stamper".into();
            event.branch = ro.branch();
            event.response.content = Some(Content::from_parts(
                vec![Part::from_text("Stamped.")],
                "model",
            ));
            futures::stream::iter(vec![Ok(event)]).boxed()
        })),
        ..Default::default()
    })
    .unwrap();
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "pipeline".into(),
            description: "Drafts, checks, polishes and stamps a reply.".into(),
            sub_agents: vec![drafter, checks, polisher, stamper],
            ..Default::default()
        },
    })
    .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Servers (SCENARIO.md "Server layout")
// ---------------------------------------------------------------------------------------------

/// Go `newCard`, interfaces at `<base>` (`only` = one binding).
fn new_card(agent: &dyn Agent, base: &str, only: Option<&str>) -> a2a::AgentCard {
    let mut interfaces = vec![
        json!({"url": format!("{base}/jsonrpc"), "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}),
        json!({"url": format!("{base}/rest"), "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"}),
    ];
    match only {
        Some("jsonrpc") => interfaces.truncate(1),
        Some(_) => {
            interfaces.remove(0);
        }
        None => {}
    }
    let mut card: a2a::AgentCard = serde_json::from_value(json!({
        "name": agent.name(),
        "description": agent.description(),
        "version": "1.0.0",
        "supportedInterfaces": interfaces,
        // a2a-go marshals `pushNotifications` even when false; a2a-rs writes it when set.
        "capabilities": {"streaming": true, "pushNotifications": false},
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [],
    }))
    .unwrap();
    card.skills = build_agent_skills(agent);
    card
}

/// SCENARIO.md "Fixed providers": server-side UUIDs `00000000-0000-4000-8000-%012d`
/// (n = 1, 2, …), one sequence per server. Normalization removes them, except that the probe's
/// root and child servers mint the same first value (their first invocation IDs), which the
/// fixture records as one placeholder; the same sequence reproduces that.
fn fixed_uuids() -> Platform {
    let n = Arc::new(std::sync::atomic::AtomicU64::new(0));
    Platform::default().with_uuid(move || {
        let n = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        format!("00000000-0000-4000-8000-{n:012}")
    })
}

/// A recorded gadk-a2a server: the executor behind a2a-rs's routers in the probe's layout.
struct RecordedServer {
    base: String,
    exchanges: Exchanges,
    sessions: Arc<InMemorySessionService>,
}

async fn start_server(
    agent: Arc<dyn Agent>,
    output_mode: OutputMode,
    streaming_mode: StreamingMode,
    only: Option<&str>,
) -> RecordedServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sessions = Arc::new(InMemorySessionService::new());
    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(RunnerConfig {
            app_name: agent.name().to_owned(),
            agent: Arc::clone(&agent),
            session_service: sessions.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: fixed_uuids(),
            compaction: None,
        }),
        run_config: RunConfig {
            streaming_mode,
            ..Default::default()
        },
        output_mode,
        ..Default::default()
    });
    let card = new_card(agent.as_ref(), &base, only);
    let handler = Arc::new(
        DefaultRequestHandler::new(executor, InMemoryTaskStore::new())
            .with_capabilities(card.capabilities.clone()),
    );
    let app = axum::Router::new()
        .nest("/jsonrpc", jsonrpc_router(Arc::clone(&handler)))
        .nest("/rest", rest_router(handler))
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))));
    let exchanges = Exchanges::default();
    let app = recorded(app, Arc::clone(&exchanges));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    RecordedServer {
        base,
        exchanges,
        sessions,
    }
}

// ---------------------------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------------------------

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/wire")
}

fn load_fixture(scenario: &str) -> Value {
    let text = std::fs::read_to_string(testdata().join(format!("{scenario}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A scenario's executor (SCENARIO.md "Scenarios", executor column).
struct ServerSetup {
    agent: fn() -> Arc<dyn Agent>,
    output_mode: OutputMode,
    streaming_mode: StreamingMode,
}

fn server_setup(scenario: &str) -> Option<ServerSetup> {
    let (agent, output_mode, streaming_mode): (fn() -> Arc<dyn Agent>, _, _) = match scenario {
        "text_reply" => (assistant, OutputMode::ArtifactPerRun, StreamingMode::None),
        "streaming_partials_per_run" => (weather, OutputMode::ArtifactPerRun, StreamingMode::Sse),
        "streaming_partials_per_event" => {
            (weather, OutputMode::ArtifactPerEvent, StreamingMode::Sse)
        }
        "input_required_resume" => (approver, OutputMode::ArtifactPerRun, StreamingMode::None),
        "failed_task" => (broken, OutputMode::ArtifactPerRun, StreamingMode::None),
        "agent_card_workflow" => (pipeline, OutputMode::ArtifactPerRun, StreamingMode::None),
        _ => return None,
    };
    Some(ServerSetup {
        agent,
        output_mode,
        streaming_mode,
    })
}

/// Replays one server unit; returns the actual (not normalized) unit and its base URLs.
async fn run_server_unit(scenario: &str, fixture: &Value) -> (Value, Vec<(String, &'static str)>) {
    let (server, child) = if scenario == "cancel_input_required_with_child" {
        let child = start_server(
            approver(),
            OutputMode::ArtifactPerEvent,
            StreamingMode::Sse,
            Some("jsonrpc"),
        )
        .await;
        let child_card = new_card(approver().as_ref(), &child.base, Some("jsonrpc"));
        let server = start_server(
            root(child_card),
            OutputMode::ArtifactPerRun,
            StreamingMode::Sse,
            None,
        )
        .await;
        (server, Some(child))
    } else {
        let setup = server_setup(scenario).unwrap();
        let server = start_server(
            (setup.agent)(),
            setup.output_mode,
            setup.streaming_mode,
            None,
        )
        .await;
        (server, None)
    };
    let mut bases = vec![(server.base.clone(), "<base>")];
    if let Some(child) = &child {
        bases.push((child.base.clone(), "<child-base>"));
    }

    let fixture_exchanges = fixture["exchanges"].as_array().unwrap();
    let mut actual: Vec<Value> = Vec::new();
    for (i, fx) in fixture_exchanges.iter().enumerate() {
        let mut bindings = Bindings::with_bases(bases.clone());
        for (f, a) in fixture_exchanges.iter().zip(&actual).take(i) {
            bindings.align(f, a);
        }
        let request = bindings.denormalize(&fx["request"]);
        let child_before = child.as_ref().map_or(0, |c| c.exchanges.lock().len());
        let sent_before = server.exchanges.lock().len();
        send_raw(&server.base, &request).await;
        wait_until("the server to record the exchange", || {
            server.exchanges.lock().len() > sent_before
        })
        .await;
        let mut exchange = server.exchanges.lock()[sent_before].clone();
        if let Some(child) = &child {
            let want = fx["child"].as_array().map_or(0, Vec::len);
            wait_until("the child exchanges", || {
                child.exchanges.lock().len() >= child_before + want
            })
            .await;
            let got: Vec<Value> = child.exchanges.lock()[child_before..].to_vec();
            if !got.is_empty() {
                exchange["child"] = Value::Array(got);
            }
        }
        actual.push(exchange);
    }

    let mut unit = json!({"exchanges": actual});
    if fixture.get("session_events").is_some() {
        let body = &unit["exchanges"][0]["response"]["body"];
        // JSON-RPC wraps the send result in `result`; REST answers it bare.
        let task = body.get("result").unwrap_or(body);
        let ctx = task["task"]["contextId"]
            .as_str()
            .unwrap_or_else(|| panic!("{scenario}: the first send created no task: {unit:#}"))
            .to_owned();
        let session = server
            .sessions
            .get(GetRequest {
                app_name: "root".into(),
                user_id: format!("A2A_USER_{ctx}"),
                session_id: ctx,
                ..Default::default()
            })
            .await
            .unwrap();
        unit["session_events"] = Value::Array(
            session
                .events()
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect(),
        );
    }
    (unit, bases)
}

/// The fake server of a client unit: serves the recorded responses in order.
struct FakeServer {
    base: String,
    responses: Vec<Value>,
    bindings: Bindings,
    served: Vec<Value>,
    errors: Vec<String>,
}

type SharedFake = Arc<Mutex<FakeServer>>;

async fn start_fake(responses: Vec<Value>) -> SharedFake {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = Arc::new(Mutex::new(FakeServer {
        base: base.clone(),
        responses,
        bindings: Bindings::with_bases(vec![(base, "<base>")]),
        served: Vec::new(),
        errors: Vec::new(),
    }));
    let state = Arc::clone(&fake);
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let fake = Arc::clone(&state);
        async move { serve_recorded(fake, req).await }
    });
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    fake
}

async fn serve_recorded(fake: SharedFake, req: axum::extract::Request) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let request = record_request(&parts, &body);
    let (status, headers, payload, frames) = {
        let mut fake = fake.lock();
        let i = fake.served.len();
        let Some(fx) = fake.responses.get(i).cloned() else {
            fake.errors
                .push(format!("unexpected request {i}: {request}"));
            return axum::response::Response::builder()
                .status(500)
                .body(Body::empty())
                .unwrap();
        };
        fake.bindings.align(&fx["request"], &request);
        let want = fake.bindings.denormalize(&fx["request"]);
        if want["method"] != request["method"] || want["path"] != request["path"] {
            fake.errors.push(format!(
                "request {i}: want {} {}, got {} {}",
                want["method"], want["path"], request["method"], request["path"]
            ));
        }
        let response = fake.bindings.denormalize(&fx["response"]);
        fake.served
            .push(json!({"request": request, "response": response.clone()}));
        (
            response["status"].as_u64().unwrap() as u16,
            response["headers"].clone(),
            response["body"].clone(),
            response.get("sse").cloned(),
        )
    };
    let mut builder = axum::response::Response::builder().status(status);
    for (name, value) in headers.as_object().unwrap() {
        builder = builder.header(name.as_str(), value.as_str().unwrap());
    }
    match frames {
        Some(Value::Array(frames)) => {
            let stream = futures::stream::iter(
                frames
                    .into_iter()
                    .map(|f| Ok::<_, std::convert::Infallible>(format!("data: {f}\n\n"))),
            );
            builder.body(Body::from_stream(stream)).unwrap()
        }
        _ => {
            let bytes = match payload {
                Value::Null => Vec::new(),
                Value::String(s) => s.into_bytes(),
                v => serde_json::to_vec(&v).unwrap(),
            };
            builder.body(Body::from(bytes)).unwrap()
        }
    }
}

/// Replays one client unit; returns the actual (not normalized) unit and its base URL.
async fn run_client_unit(name: &str, fixture: &Value) -> (Value, Vec<(String, &'static str)>) {
    let runs = fixture["runs"].as_array().unwrap();
    let responses: Vec<Value> = runs
        .iter()
        .flat_map(|r| r["exchanges"].as_array().unwrap().clone())
        .collect();
    let fake = start_fake(responses).await;
    let base = fake.lock().base.clone();
    let bases = vec![(base.clone(), "<base>")];

    let mut unit = Map::new();
    let mut cfg = A2AConfig {
        name: "remote".into(),
        description: "Remote A2A agent.".into(),
        ..Default::default()
    };
    if let Some(card) = fixture.get("card") {
        let card = Bindings::with_bases(bases.clone()).denormalize(card);
        cfg.agent_card = Some(serde_json::from_value(card.clone()).unwrap());
        unit.insert("card".into(), card);
    } else {
        assert_eq!(name, "card_provider");
        cfg.agent_card_provider = Some(new_agent_card_provider(
            base.clone(),
            CardResolveOptions::default(),
        ));
    }
    let remote = new_a2a(cfg).unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "client".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: None,
        })
        .await
        .unwrap();
    let runner = Runner::new(RunnerConfig {
        app_name: "client".into(),
        agent: remote,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();

    let mut event_bindings = Bindings::with_bases(bases.clone());
    let mut actual_runs = Vec::new();
    let mut expected_served = 0;
    for run in runs {
        let new_message = event_bindings.denormalize(&run["new_message"]);
        let content: Content = serde_json::from_value(new_message.clone()).unwrap();
        let mode = match run["streaming_mode"].as_str().unwrap() {
            "none" => StreamingMode::None,
            "sse" => StreamingMode::Sse,
            other => panic!("unknown streaming mode {other}"),
        };
        let served_before = fake.lock().served.len();
        expected_served += run["exchanges"].as_array().unwrap().len();
        let mut stream = runner.run(
            "user",
            "s1",
            Some(content),
            RunConfig {
                streaming_mode: mode,
                ..Default::default()
            },
            RunOptions::default(),
        );
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(match item {
                Ok(event) => serde_json::to_value(&event).unwrap(),
                Err(err) => json!({"error": err.to_string()}),
            });
        }
        drop(stream);
        wait_until("the client's requests", || {
            fake.lock().served.len() >= expected_served
        })
        .await;
        // Let a late request (a cleanup cancel the fixture does not expect) arrive and fail.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let exchanges: Vec<Value> = fake.lock().served[served_before..].to_vec();
        event_bindings.align(&run["events"], &Value::Array(events.clone()));
        actual_runs.push(json!({
            "streaming_mode": run["streaming_mode"],
            "new_message": new_message,
            "exchanges": exchanges,
            "events": events,
        }));
    }
    let fake = fake.lock();
    assert!(fake.errors.is_empty(), "client.{name}: {:?}", fake.errors);
    assert_eq!(
        fake.served.len(),
        fake.responses.len(),
        "client.{name}: every recorded response is consumed"
    );
    unit.insert("runs".into(), Value::Array(actual_runs));
    (Value::Object(unit), bases)
}

/// Replays every unit of `scenario`; `Err` lists the undeclared differences.
async fn replay_scenario(scenario: &str, fixture: &Value) -> Result<(), String> {
    let deltas = load_deltas(scenario);
    let mut failures = Vec::new();
    for (binding, unit) in fixture["server"].as_object().unwrap() {
        let (actual, bases) = run_server_unit(scenario, unit).await;
        let actual = normalize(&actual, &bases);
        if let Err(e) = compare_unit(&format!("server.{binding}"), unit, &actual, &deltas) {
            failures.push(e);
        }
    }
    if let Some(clients) = fixture.get("client").and_then(Value::as_object) {
        for (name, unit) in clients {
            let (actual, bases) = run_client_unit(name, unit).await;
            let actual = normalize(&actual, &bases);
            if let Err(e) = compare_unit(&format!("client.{name}"), unit, &actual, &deltas) {
                failures.push(e);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("{scenario}:\n{}", failures.join("\n")))
    }
}

async fn replay(scenario: &str) {
    let fixture = load_fixture(scenario);
    assert_eq!(fixture["scenario"], scenario);
    if let Err(e) = replay_scenario(scenario, &fixture).await {
        panic!("{e}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_text_reply() {
    replay("text_reply").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_streaming_partials_per_run() {
    replay("streaming_partials_per_run").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_streaming_partials_per_event() {
    replay("streaming_partials_per_event").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_input_required_resume() {
    replay("input_required_resume").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_failed_task() {
    replay("failed_task").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_cancel_input_required_with_child() {
    replay("cancel_input_required_with_child").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_agent_card_workflow() {
    replay("agent_card_workflow").await;
}

/// An undeclared difference fails the replay: the `text_reply` fixture with one response text
/// changed no longer matches gadk-a2a's wire.
#[tokio::test(flavor = "multi_thread")]
async fn an_undeclared_difference_fails_the_replay() {
    let mut fixture = load_fixture("text_reply");
    let artifact = &mut fixture["server"]["jsonrpc"]["exchanges"][0]["response"]["body"]["result"]
        ["task"]["artifacts"][0]["parts"][0]["text"];
    assert_eq!(artifact, "Hello! How can I help?");
    *artifact = json!("Goodbye!");
    let err = replay_scenario("text_reply", &fixture).await.unwrap_err();
    assert!(
        err.contains(
            "server.jsonrpc:\n  /exchanges/0/response/body/result/task/artifacts/0/parts/0/text: fixture \"Goodbye!\" != actual \"Hello! How can I help?\""
        ),
        "{err}"
    );
}

/// Normalization follows SCENARIO.md: kinds from the first sighting in canonical (sorted-key)
/// order, embedded UUIDs, timestamps and base URLs.
#[test]
fn normalization_follows_the_scenario_rules() {
    let t = "11111111-1111-4111-8111-111111111111";
    let c = "22222222-2222-4222-8222-222222222222";
    let m = "33333333-3333-4333-8333-333333333333";
    let unit = json!({
        "z": {"id": t, "status": {"timestamp": "2026-11-10T00:00:00Z"}, "contextId": c},
        "a": {"messageId": m, "text": format!("A2A_USER_{c}"), "url": "http://127.0.0.1:9/jsonrpc"},
        "b": [format!("/rest/tasks/{t}:cancel")],
    });
    let got = normalize(&unit, &[("http://127.0.0.1:9".into(), "<base>")]);
    assert_eq!(
        got,
        json!({
            "z": {"id": "<id-2>", "status": {"timestamp": "<t>"}, "contextId": "<id-1>"},
            "a": {"messageId": "<msg-1>", "text": "A2A_USER_<id-1>", "url": "<base>/jsonrpc"},
            "b": ["/rest/tasks/<id-2>:cancel"],
        })
    );
    let mut b = Bindings::default();
    b.align(&got, &unit);
    assert_eq!(b.ids.get("<msg-1>").map(String::as_str), Some(m));
    assert_eq!(b.ids.get("<id-1>").map(String::as_str), Some(c));
    assert_eq!(b.ids.get("<id-2>").map(String::as_str), Some(t));
    let mut fresh = Bindings::default();
    let unit = json!({"y": {"contextId": c}, "z": {"id": t, "status": {}, "contextId": c}});
    assert_eq!(
        normalize(&unit, &[]),
        json!({"y": {"contextId": "<ctx-1>"}, "z": {"id": "<task-1>", "status": {}, "contextId": "<ctx-1>"}})
    );
    assert_eq!(
        fresh.denormalize(&json!({"timestamp": "<t>", "id": "x-<id-9>"}))["timestamp"],
        FIXED_TIME
    );
}
