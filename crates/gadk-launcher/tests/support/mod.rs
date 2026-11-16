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

//! Test support shared by the wire replay (`tests/wire_replay.rs`) and the crate's own fixture
//! tests (included there as `crate::test_support`): the fixtures recorded from ADK-Go by the Go
//! probe, its normalizations N1–N7, raw HTTP exchanges, the declared deltas, and the probe's
//! scripted `assistant` agent.
//!
//! It names no gadk-launcher item, so it compiles both inside the crate and as part of an
//! integration test.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk_agent::{Agent, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Content, FinishReason, FunctionCall, GenerateContentResponseUsageMetadata, Part, ROLE_MODEL,
};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// `args` as owned strings.
pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// A loader with no agents, for tests that load none.
pub struct NoAgents;

impl gadk_agent::Loader for NoAgents {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }

    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, gadk_agent::LoaderError> {
        Err(gadk_agent::LoaderError(format!("no agent {name}")))
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        unimplemented!("the test loads no agent")
    }
}

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// A fixed `golog` clock for tests that inject one (N1 turns it into `<ts> ` like any other).
pub const TS: &str = "2009/11/10 23:00:00 ";

/// `crates/gadk-launcher/testdata/<dir>`.
pub fn testdata(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(dir)
}

/// The fixture `testdata/<dir>/<name>.json`.
pub fn fixture(dir: &str, name: &str) -> Value {
    let path = testdata(dir).join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// A web fixture (`testdata/wire/<name>.json`).
pub fn wire(name: &str) -> Value {
    fixture("wire", name)
}

/// A CLI fixture (`testdata/cli/<name>.json`).
pub fn cli(name: &str) -> Value {
    fixture("cli", name)
}

/// The fixture names of `testdata/<dir>` (`*.json` without `*.deltas.json`), sorted.
pub fn fixture_names(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(testdata(dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.ends_with(".json") && !f.ends_with(".deltas.json"))
        .map(|f| f.trim_end_matches(".json").to_owned())
        .collect();
    names.sort();
    names
}

/// A fixture's `log` lines.
pub fn log_lines(fixture: &Value) -> Vec<String> {
    fixture["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect()
}

/// A fixture's `args` with `<port>` replaced.
pub fn args(fixture: &Value, port: u16) -> Vec<String> {
    fixture["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().replace("<port>", &port.to_string()))
        .collect()
}

/// Whether `line` (a normalized startup log line) belongs to a web sub-launcher not in `keep`:
/// its user message (`<ts>        <keyword>: …`) or, for a trigger, its unauthenticated-endpoint
/// warning (`<ts> adk: the <keyword> trigger endpoint is unauthenticated, …`).
pub fn is_line_of_other(line: &str, keep: &[&str]) -> bool {
    if let Some(rest) = line.strip_prefix("<ts> adk: the ") {
        return rest
            .split_once(" trigger endpoint is unauthenticated")
            .is_some_and(|(k, _)| !keep.contains(&k));
    }
    // A user message is indented (`<ts>        api:  …`), unlike `<ts> adk: …` records.
    line.starts_with("<ts>  ")
        && user_message_keyword(line).is_some_and(|k| !keep.contains(&k.as_str()))
}

/// A server's startup log as a launcher with only the web sub-launchers `keep` writes it: the
/// fixture's lines minus the other sub-launchers' (see [`is_line_of_other`]).
pub fn startup_log_of(startup: &Value, keep: &[&str]) -> Vec<String> {
    log_lines(startup)
        .into_iter()
        .filter(|l| !is_line_of_other(l, keep))
        .collect()
}

/// Sends the request of the web fixture `name` to `127.0.0.1:<port>` and asserts the response
/// and the lines `log()` gained meanwhile equal the fixture's (after N1–N3, N5–N7).
pub async fn assert_exchange(port: u16, name: &str, log: impl Fn() -> String) {
    let f = wire(name);
    let logged = log().len();
    let raw = exchange(port, &f["request"]).await;
    let head = f["request"]["method"] == "HEAD";
    assert_eq!(
        parse_response(&raw, head, port),
        f["response"],
        "{name}: response"
    );
    assert_eq!(
        normalize_log(&log()[logged..], port),
        log_lines(&f),
        "{name}: log"
    );
}

// ---------------------------------------------------------------------------------------------
// Normalization (SCENARIO.md N1–N7)
// ---------------------------------------------------------------------------------------------

static LOG_TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} ").unwrap());
static REQUEST_LOG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(<ts> [A-Z]+ \S+ )(?:\d+(?:\.\d+)?(?:ns|µs|ms|s|m|h))+$").unwrap()
});
static USER_MESSAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<ts> +([a-z0-9]+):").unwrap());
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});
static TIME_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(lastUpdateTime|timestamp)":(-?[0-9][0-9.eE+-]*|"[^"]*")"#).unwrap()
});
static BASE_HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<base\s[^>]*>").unwrap());

/// N1: every line-start `YYYY/MM/DD HH:MM:SS ` becomes `<ts> `.
pub fn n1(s: &str) -> String {
    LOG_TIMESTAMP.replace_all(s, "<ts> ").into_owned()
}

/// N2: `127.0.0.1:<P>`, `localhost:<P>` and `port:<P>` become `…:<port>`.
pub fn n2(s: &str, port: u16) -> String {
    Regex::new(&format!(r"(127\.0\.0\.1|localhost|port):{port}\b"))
        .unwrap()
        .replace_all(s, "${1}:<port>")
        .into_owned()
}

/// N1, N2 and N3 on log text, split into lines (a final empty line dropped).
pub fn normalize_log(text: &str, port: u16) -> Vec<String> {
    let text = n2(&n1(text), port);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| REQUEST_LOG_LINE.replace(l, "${1}<elapsed>").into_owned())
        .collect()
}

/// The keyword of a user-message line (`<ts>        api:  …` → `api`).
pub fn user_message_keyword(line: &str) -> Option<String> {
    USER_MESSAGE.captures(line).map(|c| c[1].to_owned())
}

/// N4: in a startup log, the user-message lines between `<ts> Web servers starts on …` and the
/// next `<ts> ` line, stably sorted by keyword in `order` (the preset's declaration order).
pub fn n4(mut lines: Vec<String>, order: &[&str]) -> Vec<String> {
    let Some(start) = lines
        .iter()
        .position(|l| l.starts_with("<ts> Web servers starts on "))
        .map(|i| i + 1)
    else {
        return lines;
    };
    let end = lines[start..]
        .iter()
        .position(|l| l == "<ts> ")
        .map_or(lines.len(), |i| start + i);
    let rank = |l: &String| {
        user_message_keyword(l)
            .and_then(|k| order.iter().position(|o| *o == k))
            .unwrap_or(usize::MAX)
    };
    lines[start..end].sort_by_key(rank);
    lines
}

/// N2, N5 and N6 on an inline body.
pub fn normalize_body(s: &str, port: u16) -> String {
    let s = n2(s, port);
    let s = TIME_FIELD.replace_all(&s, r#""$1":"<t>""#).into_owned();
    let mut ids: Vec<String> = Vec::new();
    UUID.replace_all(&s, |c: &regex::Captures<'_>| {
        let key = c[0].to_ascii_lowercase();
        let n = match ids.iter().position(|u| *u == key) {
            Some(i) => i + 1,
            None => {
                ids.push(key);
                ids.len()
            }
        };
        format!("<uuid-{n}>")
    })
    .into_owned()
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// HTTP (SCENARIO.md "Request", "Response")
// ---------------------------------------------------------------------------------------------

/// A loopback port nothing listens on (listen on `127.0.0.1:0`, read it, close).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits until `127.0.0.1:<port>` accepts a connection.
pub async fn wait_listening(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(Instant::now() < deadline, "nothing listens on port {port}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The bytes the probe writes for a fixture's `request` (`<port>` replaced).
pub fn raw_request(request: &Value, port: u16) -> Vec<u8> {
    let mut raw = format!(
        "{} {} HTTP/1.1\r\n",
        request["method"].as_str().unwrap(),
        request["target"].as_str().unwrap()
    );
    for h in request["headers"].as_array().unwrap() {
        let value = h[1].as_str().unwrap().replace("<port>", &port.to_string());
        raw.push_str(&format!("{}: {value}\r\n", h[0].as_str().unwrap()));
    }
    raw.push_str("\r\n");
    raw.push_str(request["body"].as_str().unwrap());
    raw.into_bytes()
}

/// Sends a fixture's `request` on a new connection and reads the response to EOF.
pub async fn exchange(port: u16, request: &Value) -> Vec<u8> {
    let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    conn.write_all(&raw_request(request, port)).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(60), conn.read_to_end(&mut raw))
        .await
        .expect("the response did not end")
        .unwrap();
    raw
}

/// Go `http.CanonicalHeaderKey`.
pub fn canonical_header(name: &str) -> String {
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

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let eol = raw.windows(2).position(|w| w == b"\r\n").unwrap();
        let size = std::str::from_utf8(&raw[..eol]).unwrap();
        let size = size.split(';').next().unwrap().trim();
        let len = usize::from_str_radix(size, 16).unwrap();
        raw = &raw[eol + 2..];
        if len == 0 {
            return out;
        }
        out.extend_from_slice(&raw[..len]);
        raw = &raw[len + 2..];
    }
}

/// The probe's `hasWallClockTime`: whether `s` holds an N5 time field with fractional seconds
/// (a wall-clock time, or an Agent Engine event timestamp in Unix seconds with microseconds),
/// whose text width varies run to run, so the body's length is not recorded.
fn has_wall_clock_time(s: &str) -> bool {
    TIME_FIELD.captures_iter(s).any(|c| c[2].contains('.'))
}

/// A raw response as the fixture's `response` records it (after N2, N5–N7; header names in Go's
/// canonical form, sorted by name then value). `head_request`: the body is not read (Go's
/// `http.ReadResponse` for a HEAD request).
pub fn parse_response(raw: &[u8], head_request: bool, port: u16) -> Value {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no header block in {:?}", String::from_utf8_lossy(raw)));
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap();
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next();
    let status: u64 = parts.next().unwrap().parse().unwrap();
    let status_text = parts.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (canonical_header(k), v.trim().to_owned())
        })
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if head_request {
        body.clear();
    } else if headers
        .iter()
        .any(|(k, v)| k == "Transfer-Encoding" && v.eq_ignore_ascii_case("chunked"))
    {
        body = dechunk(&body);
    }
    let (inline, sha) = match std::str::from_utf8(&body) {
        Ok(text) if body.len() <= 8192 => (Some(normalize_body(text, port)), None),
        _ => (None, Some(sha256_hex(&body))),
    };
    let normalized = inline
        .as_deref()
        .is_some_and(|t| t.as_bytes() != body.as_slice());
    let html = headers
        .iter()
        .find(|(k, _)| k == "Content-Type")
        .is_some_and(|(_, v)| v.starts_with("text/html"));
    let base_href = if html {
        let text = String::from_utf8_lossy(&body);
        BASE_HREF.find(&text).map(|m| m.as_str().to_owned())
    } else {
        None
    };
    let mut headers: Vec<(String, String)> = headers
        .into_iter()
        .map(|(k, v)| {
            let v = match k.as_str() {
                "Date" => "<date>".to_owned(),
                "Content-Length" if normalized => "<n>".to_owned(),
                _ => n2(&v, port),
            };
            (k, v)
        })
        .collect();
    headers.sort();
    let body_length = (!has_wall_clock_time(&String::from_utf8_lossy(&body))).then_some(body.len());
    json!({
        "status": status,
        "status_text": status_text,
        "headers": headers,
        "body": inline,
        "body_length": body_length,
        "body_sha256": sha,
        "base_href": base_href,
    })
}

// ---------------------------------------------------------------------------------------------
// Comparison with declared deltas
// ---------------------------------------------------------------------------------------------

/// A response's `headers` list as an object (name → value; repeated names joined with `, `), so
/// deltas can name a header by path.
pub fn headers_as_object(response: &mut Value) {
    let Some(list) = response.get("headers").and_then(Value::as_array) else {
        return;
    };
    let mut map = Map::new();
    for h in list {
        let (k, v) = (h[0].as_str().unwrap(), h[1].as_str().unwrap());
        match map.get_mut(k) {
            Some(Value::String(prev)) => {
                prev.push_str(", ");
                prev.push_str(v);
            }
            _ => {
                map.insert(k.to_owned(), Value::String(v.to_owned()));
            }
        }
    }
    response["headers"] = Value::Object(map);
}

/// One declared delta: at `path` (a JSON pointer into the comparison document, `*` matching
/// every member or element), Go's value `fixture` (absent: Go has none there) is gadk's
/// `actual` (absent: gadk has none); `divergence` cites a `testdata/DIVERGENCES.md` line.
#[derive(Debug, Clone)]
pub struct Delta {
    pub path: Vec<String>,
    pub fixture: Option<Value>,
    pub actual: Option<Value>,
}

/// A fixture's declared deltas (`testdata/<dir>/<name>.deltas.json`):
/// `{"json_body": "<divergence>", "deltas": [{"path", "fixture", "actual", "divergence"}]}`.
/// `json_body`, when present, compares the response body as parsed JSON (key order and
/// whitespace aside; N6's placeholders renumbered in key order as `<uuid#N>`) and drops
/// `body_length` and `Content-Length` from the comparison.
#[derive(Debug, Clone, Default)]
pub struct Deltas {
    pub json_body: bool,
    pub deltas: Vec<Delta>,
}

impl Deltas {
    /// The deltas of `testdata/<dir>/<name>.deltas.json`, none when the file is absent.
    pub fn load(dir: &str, name: &str) -> Deltas {
        let path = testdata(dir).join(format!("{name}.deltas.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Deltas::default();
        };
        let doc: Value = serde_json::from_str(&text).unwrap();
        let json_body = doc.get("json_body").is_some();
        if let Some(d) = doc.get("json_body") {
            assert!(
                d.as_str().is_some_and(|s| !s.is_empty()),
                "{name}: json_body cites a ledger divergence"
            );
        }
        let deltas = doc["deltas"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|d| {
                assert!(
                    d["divergence"].as_str().is_some_and(|s| !s.is_empty()),
                    "{name}: every delta cites a ledger divergence: {d}"
                );
                Delta {
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
        Deltas { json_body, deltas }
    }
}

/// Applies `delta` at every location its path names; returns how many locations held Go's value.
fn apply_delta(v: &mut Value, path: &[String], delta: &Delta) -> usize {
    let Some((head, rest)) = path.split_first() else {
        return 0;
    };
    if rest.is_empty() {
        return match v {
            Value::Object(map) => {
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
                n
            }
            Value::Array(items) => {
                let Ok(i) = head.parse::<usize>() else {
                    return 0;
                };
                match (&delta.fixture, &delta.actual) {
                    (Some(f), actual) if items.get(i) == Some(f) => {
                        match actual {
                            Some(a) => items[i] = a.clone(),
                            None => {
                                items.remove(i);
                            }
                        }
                        1
                    }
                    (None, Some(a)) if i <= items.len() => {
                        items.insert(i, a.clone());
                        1
                    }
                    _ => 0,
                }
            }
            _ => 0,
        };
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

static UUID_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<uuid-\d+>").unwrap());

/// N6's numbering redone over a parsed body, visiting object members in key order, so two
/// bodies that differ only in key order number their UUIDs alike.
fn renumber_uuids(v: &mut Value, seen: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            *s = UUID_PLACEHOLDER
                .replace_all(s, |c: &regex::Captures<'_>| {
                    let n = match seen.iter().position(|p| *p == c[0]) {
                        Some(i) => i + 1,
                        None => {
                            seen.push(c[0].to_owned());
                            seen.len()
                        }
                    };
                    format!("<uuid#{n}>")
                })
                .into_owned();
        }
        Value::Array(items) => items.iter_mut().for_each(|i| renumber_uuids(i, seen)),
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                if let Some(c) = map.get_mut(&k) {
                    renumber_uuids(c, seen);
                }
            }
        }
        _ => {}
    }
}

/// With `json_body`: the response body parsed (when it parses; its UUID placeholders renumbered
/// in key order as `<uuid#N>`), its length and `Content-Length` dropped.
fn as_json_body(doc: &mut Value) {
    let Some(resp) = doc.get_mut("response").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(mut parsed) = resp
        .get("body")
        .and_then(Value::as_str)
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
    {
        renumber_uuids(&mut parsed, &mut Vec::new());
        resp.insert("body".to_owned(), parsed);
    }
    resp.remove("body_length");
    if let Some(Value::Object(h)) = resp.get_mut("headers") {
        h.remove("Content-Length");
    }
}

/// Every difference between `want` and `got`, as `path: want != got` lines.
fn diff(path: &str, want: Option<&Value>, got: Option<&Value>, out: &mut Vec<String>) {
    match (want, got) {
        (Some(Value::Object(w)), Some(Value::Object(g))) => {
            let mut keys: Vec<&String> = w.keys().chain(g.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                diff(&format!("{path}/{k}"), w.get(k), g.get(k), out);
            }
        }
        (Some(Value::Array(w)), Some(Value::Array(g))) if w.len() == g.len() => {
            for (i, (a, b)) in w.iter().zip(g).enumerate() {
                diff(&format!("{path}/{i}"), Some(a), Some(b), out);
            }
        }
        (w, g) if w == g => {}
        (w, g) => out.push(format!(
            "{path}:\n      fixture: {}\n      gadk:    {}",
            w.map_or("(absent)".to_owned(), Value::to_string),
            g.map_or("(absent)".to_owned(), Value::to_string)
        )),
    }
}

/// Compares gadk's comparison document `got` with the fixture's `want` after the fixture's
/// deltas; `Err` lists every difference and every delta that matched nothing.
pub fn compare(name: &str, want: &Value, got: &Value, deltas: &Deltas) -> Result<(), String> {
    // Debug aid: `WIRE_REPLAY_DUMP=<dir>` writes every compared gadk document there.
    if let Ok(dir) = std::env::var("WIRE_REPLAY_DUMP") {
        let file = PathBuf::from(dir).join(format!("{}.json", name.replace('/', "_")));
        std::fs::write(file, serde_json::to_vec_pretty(got).unwrap()).unwrap();
    }
    let (mut want, mut got) = (want.clone(), got.clone());
    for doc in [&mut want, &mut got] {
        if let Some(resp) = doc.get_mut("response")
            && !resp.is_null()
        {
            headers_as_object(resp);
        }
        if deltas.json_body {
            as_json_body(doc);
        }
    }
    let mut problems = Vec::new();
    for d in &deltas.deltas {
        if apply_delta(&mut want, &d.path, d) == 0 {
            problems.push(format!(
                "delta /{} matched nothing (stale delta)",
                d.path.join("/")
            ));
        }
    }
    diff("", Some(&want), Some(&got), &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{name}:\n  {}", problems.join("\n  ")))
    }
}

// ---------------------------------------------------------------------------------------------
// The probe's agent (SCENARIO.md "Agent and model")
// ---------------------------------------------------------------------------------------------

/// The probe's `scriptedModel`: answers from a fixed function of the request's last content.
pub struct ScriptedModel;

fn final_response(parts: Vec<Part>) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_parts(parts, ROLE_MODEL)),
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            ..Default::default()
        }),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    }
}

fn chunk(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_text(text, ROLE_MODEL)),
        partial: true,
        ..Default::default()
    }
}

/// Go `textReply`: the final text, preceded in streaming mode by the chunks.
fn text_reply(stream: bool, chunks: &[&str]) -> Vec<LlmResponse> {
    let mut out = Vec::new();
    if stream {
        out.extend(chunks.iter().map(|c| chunk(c)));
    }
    out.push(final_response(vec![Part::from_text(chunks.concat())]));
    out
}

/// Go `reply`.
fn reply(last: Option<&Content>, stream: bool) -> Result<Vec<LlmResponse>, ModelError> {
    if let Some(last) = last {
        for p in &last.parts {
            if let Some(fr) = &p.function_response {
                return Ok(if fr.response.contains_key("error") {
                    text_reply(stream, &["Transfer ", "cancelled."])
                } else {
                    text_reply(stream, &["Transfer ", "done."])
                });
            }
        }
    }
    let text = last
        .and_then(|c| c.parts.iter().find(|p| !p.text.is_empty()))
        .map(|p| p.text.as_str())
        .unwrap_or_default();
    Ok(match text {
        "hello" => text_reply(stream, &["Hello! ", "How can I help?"]),
        "revise" if stream => vec![
            chunk("Draft "),
            chunk("answer."),
            final_response(vec![Part::from_text("Final answer.")]),
        ],
        "revise" => vec![final_response(vec![Part::from_text("Final answer.")])],
        "fail" => return Err(ModelError::Other("scripted model failure".into())),
        "transfer" => vec![final_response(vec![Part {
            function_call: Some(FunctionCall {
                name: "transfer_funds".into(),
                args: json!({"amount": 100})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }])],
        _ => text_reply(stream, &["I can only ", "say hello."]),
    })
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted-assistant"
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let items: Vec<Result<LlmResponse, ModelError>> = match reply(req.contents.last(), stream) {
            Ok(responses) => responses.into_iter().map(Ok).collect(),
            Err(e) => vec![Err(e)],
        };
        futures::stream::iter(items).boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The probe's `assistant` (`newAgent`): `transfer_funds` requires confirmation and answers
/// `{"status": "transferred <amount>"}`.
pub fn probe_agent() -> Arc<dyn Agent> {
    probe_agent_with(Arc::new(ScriptedModel))
}

/// [`probe_agent`] over another model (a test scripting what the probe's model does not).
pub fn probe_agent_with(model: Arc<dyn Llm>) -> Arc<dyn Agent> {
    let transfer_funds = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(json!({
                "type": "object",
                "properties": {"amount": {"type": "integer"}},
            })),
            require_confirmation: true,
            ..FunctionToolConfig::new("transfer_funds", "Transfers funds.")
        },
        |_ctx, args: Map<String, Value>| async move {
            let amount = args.get("amount").cloned().unwrap_or_default();
            Ok::<Value, ToolError>(json!({"status": format!("transferred {amount}")}))
        },
    )
    .unwrap();
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        description: "Answers greetings and transfers funds.".into(),
        instruction: "Answer briefly. Use transfer_funds to transfer funds.".into(),
        model: Some(model),
        tools: vec![transfer_funds],
        ..Default::default()
    })
    .unwrap()
}
