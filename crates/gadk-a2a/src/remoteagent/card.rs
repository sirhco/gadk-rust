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

//! Agent-card resolution and source checks (port of ADK-Go `agent/remoteagent/v2/a2a_agent.go`
//! `NewAgentCardProvider`, `classifyCardSource`, `validateCardInterfaceOrigins`, `urlOrigin`,
//! `isLoopbackHost`, `requireSecureCardSource`, and of a2a-go's `agentcard.Resolver`, whose
//! fetch gadk-a2a performs itself).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use gadk_platform::{go_quote, go_to_lower, go_url};
use serde_json::Value;

use super::auth::{apply_credential, current_card_fetch_client};
use crate::Error;

/// Go `AgentCardProvider`: resolves the remote agent's card.
pub type AgentCardProvider =
    Arc<dyn Fn() -> BoxFuture<'static, Result<a2a::AgentCard, Error>> + Send + Sync>;

/// Go `agentcard.ResolveOption`s that [`new_agent_card_provider`] accepts.
#[derive(Debug, Clone, Default)]
pub struct CardResolveOptions {
    /// Go `agentcard.WithPath`: the card path; `None` → `/.well-known/agent-card.json`.
    pub path: Option<String>,
    /// Go `agentcard.WithRequestHeader`: extra request headers, in order.
    pub request_headers: Vec<(String, String)>,
}

/// Go `ErrUnsupportedCardSource`'s text: every error [`classify_card_source`] returns starts
/// with it.
pub(crate) const UNSUPPORTED_CARD_SOURCE: &str = "unsupported agent card source";

/// Go `ErrUntrustedCardInterface`'s text: every error [`validate_card_interface_origins`] and
/// [`require_secure_card_source`] return starts with it.
pub(crate) const UNTRUSTED_CARD_INTERFACE: &str = "untrusted agent card interface";

/// a2a-go `agentcard.defaultAgentCardPath`.
const DEFAULT_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

/// a2a-go `agentcard.defaultClient`'s timeout.
const DEFAULT_CARD_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `NewAgentCardProvider`: a URL or file source; reads the card-fetch client and credential
/// scope from the auth scope when present.
///
/// A file source is read and parsed. An http(s) source is fetched with `GET <source><path>`
/// (a2a-go `agentcard.Resolver` URL rules, `opts.request_headers` attached); inside an auth scope
/// that carries a card-fetch client (Go `CardFetchClientFrom`) the source must be https or
/// loopback http ([`require_secure_card_source`], checked before any request), that client sends
/// it, and the scope's credential is applied. Every interface of a fetched card must share the source's
/// origin ([`validate_card_interface_origins`]).
pub fn new_agent_card_provider(
    source: impl Into<String>,
    opts: CardResolveOptions,
) -> AgentCardProvider {
    let source: Arc<str> = Arc::from(source.into());
    let opts = Arc::new(opts);
    // One client per provider, not per process: a pooled connection is bound to the runtime
    // that opened it, so a process-wide client breaks a provider used from another runtime.
    let default_client = default_card_client();
    Arc::new(move || {
        let source = Arc::clone(&source);
        let opts = Arc::clone(&opts);
        let default_client = default_client.clone();
        Box::pin(async move { resolve_card(&source, &opts, &default_client).await })
    })
}

/// The body of a [`new_agent_card_provider`] provider; `default_client` sends the fetch outside
/// an auth scope.
async fn resolve_card(
    source: &str,
    opts: &CardResolveOptions,
    default_client: &reqwest::Client,
) -> Result<a2a::AgentCard, Error> {
    if classify_card_source(source)? {
        return read_card_file(source);
    }
    // Go `iremoteagent.CardFetchClientFrom(ctx)`: present exactly when the remote agent owns the
    // auth scope of this call (`new_a2a` with `auth` always attaches one).
    let card = match current_card_fetch_client() {
        Some(client) => {
            // The fetch carries the credential, so the scheme is checked before it goes out.
            require_secure_card_source(source)?;
            fetch_card(&client, source, opts, true).await
        }
        None => fetch_card(default_client, source, opts, false).await,
    }
    .map_err(|e| Error::wrap("failed to fetch an agent card", e))?;
    validate_card_interface_origins(&card, source)?;
    Ok(card)
}

/// a2a-go `agentcard.defaultClient`: a 30 s timeout and the default redirect policy.
fn default_card_client() -> reqwest::Client {
    // `build` fails only when reqwest cannot initialize its TLS backend. The fallback is
    // `reqwest::Client::new()`, which panics on that same failure: this panics exactly when
    // reqwest's own `Client::new` would. `new_agent_card_provider` (Go `NewAgentCardProvider`)
    // does not return an error, so there is nothing to propagate it through.
    reqwest::Client::builder()
        .timeout(DEFAULT_CARD_FETCH_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// a2a-go `Resolver.Resolve` for an http(s) source: build the URL, send the request (with the
/// scope's credential when `authenticated`), require 200 and parse the body.
async fn fetch_card(
    client: &reqwest::Client,
    source: &str,
    opts: &CardResolveOptions,
    authenticated: bool,
) -> Result<a2a::AgentCard, Error> {
    let req_url = build_card_url(source, opts.path.as_deref())?;
    let mut headers = http::HeaderMap::new();
    for (name, value) in go_request_headers(&opts.request_headers) {
        let name = http::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            card_request_failed(
                &req_url,
                Error::msg(format!(
                    "net/http: invalid header field name {}",
                    go_quote(name)
                )),
            )
        })?;
        let value = http::HeaderValue::from_str(value).map_err(|_| {
            card_request_failed(
                &req_url,
                Error::msg(format!(
                    "net/http: invalid header field value for {}",
                    go_quote(name.as_str())
                )),
            )
        })?;
        headers.append(name, value);
    }
    if authenticated {
        apply_credential(&mut headers)
            .await
            .map_err(|e| card_request_failed(&req_url, e))?;
    }
    let resp = client
        .get(&req_url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| card_request_failed(&req_url, e.without_url()))?;
    let status = resp.status();
    if status != reqwest::StatusCode::OK {
        return Err(Error::msg(format!(
            "card request failed, status: {} {}",
            status.as_str(),
            status.canonical_reason().unwrap_or_default()
        )));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| Error::wrap("failed to read card response", e.without_url()))?;
    parse_card(&body).map_err(|e| Error::wrap("card parsing failed", e))
}

/// Go `card request failed: %w` around the `*url.Error` of `http.Client.Do`
/// (`Get "<url>": <err>`).
fn card_request_failed(
    req_url: &str,
    err: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> Error {
    Error::wrap(
        "card request failed",
        Error::wrap(format!("Get {}", go_quote(req_url)), err),
    )
}

/// a2a-go `WithRequestHeader` stores headers in a map keyed by the exact name, so a later
/// header of the same name replaces an earlier one; the survivors keep their first position.
fn go_request_headers(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    let mut out: Vec<(&str, &str)> = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        match out.iter_mut().find(|(n, _)| *n == name.as_str()) {
            Some(slot) => slot.1 = value.as_str(),
            None => out.push((name.as_str(), value.as_str())),
        }
    }
    out
}

/// a2a-go `agentcard.buildURL`: a non-empty `path` is joined to the source (Go `url.JoinPath`);
/// otherwise a source with no path or the root path gets `/.well-known/agent-card.json` and any
/// other source is fetched as is.
fn build_card_url(source: &str, path: Option<&str>) -> Result<String, Error> {
    let construction = |e: String| Error::wrap("url construction failed", Error::msg(e));
    let base = go_url::parse(source).map_err(|e| construction(e.to_string()))?;
    match path.filter(|p| !p.is_empty()) {
        Some(path) => Ok(base
            .join_path(&[path])
            .map_err(construction)?
            .to_go_string()),
        None if base.path.is_empty() || base.path == b"/" => Ok(base
            .join_path(&[DEFAULT_AGENT_CARD_PATH])
            .map_err(construction)?
            .to_go_string()),
        None => Ok(source.to_owned()),
    }
}

/// Go `os.ReadFile` + `json.Unmarshal` of a file source.
fn read_card_file(source: &str) -> Result<a2a::AgentCard, Error> {
    let bytes = read_file(source).map_err(|e| {
        Error::wrap(
            format!("failed to read agent card from {}", go_quote(source)),
            e,
        )
    })?;
    parse_card(&bytes).map_err(|e| Error::wrap("failed to unmarshal an agent card", e))
}

/// Go's `*fs.PathError` text (`open <path>: no such file or directory`).
#[derive(Debug)]
struct PathError {
    op: &'static str,
    path: String,
    err: std::io::Error,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Go's `syscall.Errno` texts are the C library's, lower-cased and without the code; a
        // non-OS error's text is lower-cased too (the OS text already is, so this is a no-op).
        let text = gadk_platform::go_errno_text(&self.err);
        let mut chars = text.chars();
        let text = match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect::<String>(),
            None => String::new(),
        };
        write!(f, "{} {}: {}", self.op, self.path, text)
    }
}

impl std::error::Error for PathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.err)
    }
}

/// Go `os.ReadFile`: `open` then `read`, each failing with its own `PathError`.
fn read_file(path: &str) -> Result<Vec<u8>, PathError> {
    use std::io::Read as _;
    let path_error = |op, err| PathError {
        op,
        path: path.to_owned(),
        err,
    };
    let mut file = std::fs::File::open(path).map_err(|e| path_error("open", e))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| path_error("read", e))?;
    Ok(bytes)
}

/// Go `json.Unmarshal` into an `a2a.AgentCard`: a field the JSON omits or sets to `null` keeps
/// Go's zero value. a2a-rs requires `version`, `supportedInterfaces`, `capabilities` and an
/// interface's `url`/`protocolBinding`/`protocolVersion` and rejects `null` lists, so those are
/// filled with their zero values before decoding (a Go-encoded card writes `null` for an empty
/// list).
pub(crate) fn parse_card(bytes: &[u8]) -> Result<a2a::AgentCard, serde_json::Error> {
    let mut value: Value = serde_json::from_slice(bytes)?;
    if value.is_null() {
        value = Value::Object(serde_json::Map::new());
    }
    if let Some(card) = value.as_object_mut() {
        zero_fill(
            card,
            &["name", "description", "version"],
            &[
                "supportedInterfaces",
                "defaultInputModes",
                "defaultOutputModes",
                "skills",
            ],
        );
        if card.get("capabilities").is_none_or(Value::is_null) {
            card.insert("capabilities".into(), Value::Object(serde_json::Map::new()));
        }
        if let Some(Value::Array(ifaces)) = card.get_mut("supportedInterfaces") {
            // A Go `nil` interface is skipped by every reader; a2a-rs cannot hold one.
            ifaces.retain(|iface| !iface.is_null());
            for iface in ifaces.iter_mut().filter_map(Value::as_object_mut) {
                zero_fill(iface, &["url", "protocolBinding", "protocolVersion"], &[]);
            }
        }
        if let Some(Value::Array(skills)) = card.get_mut("skills") {
            for skill in skills.iter_mut().filter_map(Value::as_object_mut) {
                zero_fill(skill, &["id", "name", "description"], &["tags"]);
            }
        }
    }
    serde_json::from_value(value)
}

/// Sets each absent or `null` key of `strings` to `""` and of `lists` to `[]`.
fn zero_fill(obj: &mut serde_json::Map<String, Value>, strings: &[&str], lists: &[&str]) {
    for key in strings {
        if obj.get(*key).is_none_or(Value::is_null) {
            obj.insert((*key).into(), Value::String(String::new()));
        }
    }
    for key in lists {
        if obj.get(*key).is_none_or(Value::is_null) {
            obj.insert((*key).into(), Value::Array(Vec::new()));
        }
    }
}

/// Go `classifyCardSource`: `Ok(true)` for a file source, `Ok(false)` for http(s).
///
/// A source is a URL only once it carries `://`, and only if what precedes the first `://` is a
/// scheme by Go `url.Parse`'s rules (it survives a parse of `prefix + ":"` unchanged, modulo
/// case); anything else is a path. A scheme other than http(s) is refused.
pub(crate) fn classify_card_source(source: &str) -> Result<bool, Error> {
    let Some((prefix, _)) = source.split_once("://") else {
        return Ok(true);
    };
    let scheme = match go_url::parse(&format!("{prefix}:")) {
        Ok(u) if u.scheme == go_to_lower(prefix) => u.scheme,
        _ => return Ok(true),
    };
    if scheme == "http" || scheme == "https" {
        return Ok(false);
    }
    Err(Error::msg(format!(
        "{UNSUPPORTED_CARD_SOURCE} {}: scheme {} is not supported, use http(s):// or a file path",
        go_quote(source),
        go_quote(&scheme)
    )))
}

/// Go `validateCardInterfaceOrigins`: every interface of a card fetched from `source` must be
/// https or loopback http, and share the origin (scheme, case-folded host, defaulted port) of
/// the configured source.
pub(crate) fn validate_card_interface_origins(
    card: &a2a::AgentCard,
    source: &str,
) -> Result<(), Error> {
    let source_url = go_url::parse(source).map_err(|e| {
        Error::msg(format!(
            "{UNTRUSTED_CARD_INTERFACE}: invalid agent card source URL {}: {e}",
            go_quote(source)
        ))
    })?;
    let source_origin = url_origin(&source_url);

    for iface in &card.supported_interfaces {
        let iface_url = go_url::parse(&iface.url).map_err(|e| {
            Error::msg(format!(
                "{UNTRUSTED_CARD_INTERFACE}: invalid interface URL {} in agent card: {e}",
                go_quote(&iface.url)
            ))
        })?;
        if iface_url.scheme != "https" && !is_loopback_host(iface_url.hostname()) {
            return Err(Error::msg(format!(
                "{UNTRUSTED_CARD_INTERFACE}: interface URL {} must use https, or http on a loopback host",
                go_quote(&iface.url)
            )));
        }
        if url_origin(&iface_url) != source_origin {
            return Err(Error::msg(format!(
                "{UNTRUSTED_CARD_INTERFACE}: interface URL {} does not share the origin of the configured agent card source ({})",
                go_quote(&iface.url),
                go_quote(source)
            )));
        }
    }
    Ok(())
}

/// Go `urlOrigin`: (scheme, case-folded hostname, port defaulted from the scheme).
fn url_origin(u: &go_url::Url) -> (String, String, String) {
    let host = go_to_lower(u.hostname());
    let port = match (u.port(), u.scheme.as_str()) {
        ("", "http") => "80",
        ("", "https") => "443",
        (port, _) => port,
    };
    (u.scheme.clone(), host, port.to_owned())
}

/// Go `isLoopbackHost`: `localhost`, a subdomain of it (RFC 6761), or a loopback IP address
/// (Go `net.ParseIP(..).IsLoopback()`, which unmaps an IPv4-mapped address). A DNS name that
/// merely starts with `127.` is not loopback.
pub(crate) fn is_loopback_host(hostname: &str) -> bool {
    let h = go_to_lower(hostname);
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.is_loopback(), |v4| v4.is_loopback()),
        Err(_) => false,
    }
}

/// Go `requireSecureCardSource`: a credential is sent only to an https or loopback card source.
pub(crate) fn require_secure_card_source(source: &str) -> Result<(), Error> {
    let u = go_url::parse(source).map_err(|e| {
        Error::msg(format!(
            "{UNTRUSTED_CARD_INTERFACE}: invalid agent card source URL {}: {e}",
            go_quote(source)
        ))
    })?;
    if u.scheme != "https" && !is_loopback_host(u.hostname()) {
        return Err(Error::msg(format!(
            "{UNTRUSTED_CARD_INTERFACE}: agent card source {} must use https, or http on a loopback host, when A2AConfig.Auth is set",
            go_quote(source)
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// One HTTP request a [`serve`] server received.
    #[derive(Debug, Clone)]
    pub(crate) struct Recorded {
        pub(crate) method: String,
        pub(crate) target: String,
        pub(crate) headers: Vec<(String, String)>,
        pub(crate) body: Vec<u8>,
    }

    impl Recorded {
        /// Every value of header `name` (case-insensitive), in order.
        pub(crate) fn header(&self, name: &str) -> Vec<String> {
            self.headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .collect()
        }
    }

    type Handler = dyn Fn(&str, &Recorded) -> (u16, String) + Send + Sync;

    /// A minimal HTTP/1.1 server on 127.0.0.1 answering every request with
    /// `handler(base_url, request)` (a JSON body) and recording the requests.
    pub(crate) async fn serve(
        handler: impl Fn(&str, &Recorded) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<Recorded>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let (rec, base_url) = (Arc::clone(&recorded), base.clone());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (rec, handler, base_url) =
                    (Arc::clone(&rec), Arc::clone(&handler), base_url.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i;
                        }
                        let n = stream.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let mut lines = head.split("\r\n");
                    let mut request_line = lines.next().unwrap().split(' ');
                    let method = request_line.next().unwrap().to_owned();
                    let target = request_line.next().unwrap().to_owned();
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
                        .collect();
                    let len = headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, v)| v.parse::<usize>().unwrap());
                    let mut body = buf[head_end + 4..].to_vec();
                    while body.len() < len {
                        let n = stream.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                    let req = Recorded {
                        method,
                        target,
                        headers,
                        body,
                    };
                    let (status, resp) = handler(&base_url, &req);
                    rec.lock().push(req);
                    let reason = reqwest::StatusCode::from_u16(status)
                        .unwrap()
                        .canonical_reason()
                        .unwrap_or_default();
                    let out = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                        resp.len()
                    );
                    stream.write_all(out.as_bytes()).await.unwrap();
                    stream.shutdown().await.ok();
                });
            }
        });
        (base, recorded)
    }

    /// A card JSON naming `ifaces` as JSON-RPC interfaces.
    fn card_json(name: &str, ifaces: &[String]) -> String {
        let ifaces: Vec<Value> = ifaces
            .iter()
            .map(|u| serde_json::json!({"url": u, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}))
            .collect();
        serde_json::json!({
            "name": name,
            "version": "1.0",
            "supportedInterfaces": ifaces,
            "capabilities": {},
        })
        .to_string()
    }

    fn card_with(ifaces: &[&str]) -> a2a::AgentCard {
        parse_card(
            card_json(
                "test-agent",
                &ifaces.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
            .as_bytes(),
        )
        .unwrap()
    }

    /// Go `TestClassifyCardSource`'s table.
    #[test]
    fn classify_card_source_matches_go() {
        for (source, is_file) in [
            ("http://example.com", Some(false)),
            ("https://example.com/cards", Some(false)),
            ("HTTP://example.com", Some(false)),
            ("/etc/passwd", Some(true)),
            ("cards/agent.json", Some(true)),
            (r"C:\cards\agent.json", Some(true)),
            ("C:/cards/agent.json", Some(true)),
            ("file:///etc/shadow", None),
            ("ftp://example.com/card.json", None),
            ("s3://bucket/card.json", None),
            ("c://cards/agent.json", None),
            ("cards:v2/agent.json", Some(true)),
            ("weird:name.json", Some(true)),
            ("card.json:stream", Some(true)),
            ("http:card.json", Some(true)),
            ("https:", Some(true)),
            ("notes:/a://b", Some(true)),
            ("dir/sub://x", Some(true)),
            ("file\t://etc/hosts", Some(true)),
            ("https://example.com/ftp://x", Some(false)),
            ("/tmp/100%.json", Some(true)),
            ("\ncards/agent.json", Some(true)),
            ("file:///etc/shadow%", None),
            ("file:///etc/shadow\n", None),
            ("\nfile:///etc/shadow", Some(true)),
        ] {
            match is_file {
                Some(want) => assert_eq!(
                    classify_card_source(source).unwrap(),
                    want,
                    "classifyCardSource({source:?})"
                ),
                None => {
                    let err = classify_card_source(source).unwrap_err().to_string();
                    assert!(
                        err.starts_with(UNSUPPORTED_CARD_SOURCE),
                        "classifyCardSource({source:?}) = {err}"
                    );
                }
            }
        }
    }

    #[test]
    fn classify_card_source_error_text_is_gos() {
        assert_eq!(
            classify_card_source("ftp://example.test/card.json")
                .unwrap_err()
                .to_string(),
            "unsupported agent card source \"ftp://example.test/card.json\": scheme \"ftp\" is not supported, use http(s):// or a file path"
        );
        assert_eq!(
            classify_card_source("FILE:///etc/shadow")
                .unwrap_err()
                .to_string(),
            "unsupported agent card source \"FILE:///etc/shadow\": scheme \"file\" is not supported, use http(s):// or a file path"
        );
    }

    /// Go `TestIsLoopbackHost`'s table, plus the IPv4-mapped and zoned forms `net.ParseIP`
    /// decides.
    #[test]
    fn is_loopback_host_matches_go() {
        for (host, want) in [
            ("127.0.0.1", true),
            ("127.5.5.5", true),
            ("::1", true),
            ("localhost", true),
            ("LocalHost", true),
            ("app.localhost", true),
            ("127.evil.com", false),
            ("notlocalhost.example.com", false),
            ("93.184.216.34", false),
            ("", false),
            ("::ffff:127.0.0.1", true),
            ("::1%lo0", false),
        ] {
            assert_eq!(is_loopback_host(host), want, "isLoopbackHost({host:?})");
        }
    }

    /// Go `TestValidateCardInterfaceOrigins`'s table.
    #[test]
    fn validate_card_interface_origins_matches_go() {
        for (name, source, ifaces, want_err) in [
            (
                "same origin https",
                "https://good.example.com",
                vec!["https://good.example.com/rpc"],
                false,
            ),
            (
                "same origin loopback http",
                "http://127.0.0.1:8080",
                vec!["http://127.0.0.1:8080/rpc"],
                false,
            ),
            ("no interfaces", "https://good.example.com", vec![], false),
            (
                "off-origin host, same scheme",
                "https://good.example.com",
                vec!["https://attacker.example.net/rpc"],
                true,
            ),
            (
                "off-origin scheme, same host",
                "http://127.0.0.1:8080",
                vec!["https://127.0.0.1:8080/rpc"],
                true,
            ),
            (
                "second interface off-origin",
                "https://good.example.com",
                vec![
                    "https://good.example.com/rpc",
                    "https://attacker.example.net/rpc2",
                ],
                true,
            ),
            (
                "plain http on non-loopback",
                "http://example.com",
                vec!["http://example.com/rpc"],
                true,
            ),
            (
                "off-origin port",
                "https://good.example.com:8443",
                vec!["https://good.example.com:9443/rpc"],
                true,
            ),
            (
                "relative interface URL",
                "https://good.example.com",
                vec!["/rpc"],
                true,
            ),
            (
                "empty interface URL",
                "https://good.example.com",
                vec![""],
                true,
            ),
            (
                "interface host uppercase",
                "https://good.example.com",
                vec!["https://Good.Example.com/rpc"],
                false,
            ),
            (
                "source host uppercase",
                "https://Good.Example.com",
                vec!["https://good.example.com/rpc"],
                false,
            ),
            (
                "interface explicit 443",
                "https://good.example.com",
                vec!["https://good.example.com:443/rpc"],
                false,
            ),
            (
                "source explicit 443",
                "https://good.example.com:443",
                vec!["https://good.example.com/rpc"],
                false,
            ),
            (
                "loopback explicit 80",
                "http://localhost",
                vec!["http://localhost:80/rpc"],
                false,
            ),
            (
                "127 prefix dns name",
                "http://127.evil.com",
                vec!["http://127.evil.com/rpc"],
                true,
            ),
        ] {
            let result = validate_card_interface_origins(&card_with(&ifaces), source);
            assert_eq!(result.is_err(), want_err, "{name}: {result:?}");
            if let Err(e) = result {
                assert!(
                    e.to_string().starts_with(UNTRUSTED_CARD_INTERFACE),
                    "{name}: {e}"
                );
            }
        }
    }

    #[test]
    fn origin_check_error_texts_are_gos() {
        let err = |source: &str, iface: &str| {
            validate_card_interface_origins(&card_with(&[iface]), source)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            err("http://example.com", "http://example.com/rpc"),
            "untrusted agent card interface: interface URL \"http://example.com/rpc\" must use https, or http on a loopback host"
        );
        assert_eq!(
            err(
                "https://good.example.com",
                "https://attacker.example.net/rpc"
            ),
            "untrusted agent card interface: interface URL \"https://attacker.example.net/rpc\" does not share the origin of the configured agent card source (\"https://good.example.com\")"
        );
        assert_eq!(
            err("https://good.example.com", "https://x:port/rpc"),
            "untrusted agent card interface: invalid interface URL \"https://x:port/rpc\" in agent card: parse \"https://x:port/rpc\": invalid port \":port\" after host"
        );
        assert_eq!(
            err("https://g\n", "https://g/rpc"),
            "untrusted agent card interface: invalid agent card source URL \"https://g\\n\": parse \"https://g\\n\": net/url: invalid control character in URL"
        );
    }

    #[test]
    fn require_secure_card_source_matches_go() {
        for source in [
            "https://remote.example.com",
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://[::1]:9",
        ] {
            require_secure_card_source(source).unwrap();
        }
        assert_eq!(
            require_secure_card_source("http://remote.invalid")
                .unwrap_err()
                .to_string(),
            "untrusted agent card interface: agent card source \"http://remote.invalid\" must use https, or http on a loopback host, when A2AConfig.Auth is set"
        );
        assert_eq!(
            require_secure_card_source("http://h:x")
                .unwrap_err()
                .to_string(),
            "untrusted agent card interface: invalid agent card source URL \"http://h:x\": parse \"http://h:x\": invalid port \":x\" after host"
        );
    }

    /// a2a-go `buildURL`: a path is joined, an empty or root path gets the well-known path,
    /// any other path is the card URL itself.
    #[test]
    fn build_card_url_follows_a2a_go() {
        for (source, path, want) in [
            ("http://h:1", None, "http://h:1/.well-known/agent-card.json"),
            (
                "http://h:1/",
                None,
                "http://h:1/.well-known/agent-card.json",
            ),
            ("http://h:1/cards/a.json", None, "http://h:1/cards/a.json"),
            (
                "http://h:1",
                Some("/custom/card.json"),
                "http://h:1/custom/card.json",
            ),
            (
                "http://h:1/base",
                Some("card.json"),
                "http://h:1/base/card.json",
            ),
            ("http://h:1/base", Some(""), "http://h:1/base"),
            ("HTTP://h:1", None, "http://h:1/.well-known/agent-card.json"),
        ] {
            assert_eq!(
                build_card_url(source, path).unwrap(),
                want,
                "{source} {path:?}"
            );
        }
        assert_eq!(
            build_card_url("http://h:1", Some("%zz"))
                .unwrap_err()
                .to_string(),
            "url construction failed: invalid URL escape \"%zz\""
        );
    }

    #[test]
    fn parse_card_fills_gos_zero_values() {
        let card = parse_card(br#"{"name":"file-card"}"#).unwrap();
        assert_eq!(card.name, "file-card");
        assert!(card.supported_interfaces.is_empty());
        assert_eq!(card.version, "");

        let card = parse_card(
            br#"{"name":"n","supportedInterfaces":[null,{"url":"http://x","protocolBinding":"JSONRPC"}],
                "defaultInputModes":null,"skills":[{"id":"s","tags":null}],"capabilities":null}"#,
        )
        .unwrap();
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].protocol_version, "");
        assert_eq!(card.skills[0].name, "");
        assert!(card.skills[0].tags.is_empty());

        assert_eq!(parse_card(b"null").unwrap().name, "");
        assert!(parse_card(b"[1]").is_err());
        assert!(parse_card(b"{").is_err());
    }

    #[tokio::test]
    async fn provider_fetches_the_well_known_card_and_checks_its_origin() {
        let (base, recorded) =
            serve(|base, _| (200, card_json("served-card", &[format!("{base}/rpc")]))).await;
        let card = new_agent_card_provider(base.clone(), CardResolveOptions::default())()
            .await
            .unwrap();
        assert_eq!(card.name, "served-card");
        assert_eq!(card.supported_interfaces[0].url, format!("{base}/rpc"));
        let reqs = recorded.lock().clone();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            (reqs[0].method.as_str(), reqs[0].target.as_str()),
            ("GET", "/.well-known/agent-card.json")
        );
    }

    /// Go `TestNewAgentCardProvider_AcceptsUppercaseScheme`.
    #[tokio::test]
    async fn provider_accepts_an_uppercase_scheme() {
        let (base, _) =
            serve(|base, _| (200, card_json("served-card", &[format!("{base}/rpc")]))).await;
        let source = base.replacen("http://", "HTTP://", 1);
        let card = new_agent_card_provider(source, CardResolveOptions::default())()
            .await
            .unwrap();
        assert_eq!(card.name, "served-card");
    }

    #[tokio::test]
    async fn provider_sends_the_path_and_request_headers() {
        let (base, recorded) =
            serve(|base, _| (200, card_json("c", &[format!("{base}/rpc")]))).await;
        let opts = CardResolveOptions {
            path: Some("/custom/agent.json".into()),
            request_headers: vec![
                ("X-Api-Key".into(), "first".into()),
                ("X-Trace".into(), "t1".into()),
                ("X-Api-Key".into(), "second".into()),
            ],
        };
        new_agent_card_provider(base, opts)().await.unwrap();
        let req = recorded.lock()[0].clone();
        assert_eq!(req.target, "/custom/agent.json");
        assert_eq!(req.header("x-api-key"), vec!["second".to_owned()]);
        assert_eq!(req.header("x-trace"), vec!["t1".to_owned()]);
    }

    #[tokio::test]
    async fn provider_fetches_a_non_root_source_as_is() {
        let (base, recorded) =
            serve(|base, _| (200, card_json("c", &[format!("{base}/rpc")]))).await;
        new_agent_card_provider(
            format!("{base}/cards/a.json"),
            CardResolveOptions::default(),
        )()
        .await
        .unwrap();
        assert_eq!(recorded.lock()[0].target, "/cards/a.json");
    }

    /// Go `TestNewAgentCardProvider_RejectsOffOriginInterface` and `..SecondInterface`.
    #[tokio::test]
    async fn provider_rejects_an_off_origin_interface() {
        let (base, _) = serve(|base, _| {
            (
                200,
                card_json(
                    "c",
                    &[
                        format!("{base}/rpc"),
                        "https://attacker.example.net/rpc2".into(),
                    ],
                ),
            )
        })
        .await;
        let err = new_agent_card_provider(base.clone(), CardResolveOptions::default())()
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "untrusted agent card interface: interface URL \"https://attacker.example.net/rpc2\" does not share the origin of the configured agent card source (\"{base}\")"
            )
        );
    }

    #[tokio::test]
    async fn provider_reports_a_non_ok_status_and_a_bad_body_as_go_does() {
        let (base, _) = serve(|_, _| (404, "{}".into())).await;
        let err = new_agent_card_provider(base, CardResolveOptions::default())()
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to fetch an agent card: card request failed, status: 404 Not Found"
        );

        let (base, _) = serve(|_, _| (200, "{".into())).await;
        let err = new_agent_card_provider(base, CardResolveOptions::default())()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("failed to fetch an agent card: card parsing failed: "),
            "{err}"
        );
    }

    #[tokio::test]
    async fn provider_refuses_an_invalid_request_header_before_sending() {
        let (base, recorded) = serve(|_, _| (200, "{}".into())).await;
        let opts = CardResolveOptions {
            path: None,
            request_headers: vec![("Bad Name".into(), "v".into())],
        };
        let err = new_agent_card_provider(base.clone(), opts)()
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "failed to fetch an agent card: card request failed: Get \"{base}/.well-known/agent-card.json\": net/http: invalid header field name \"Bad Name\""
            )
        );
        assert!(recorded.lock().is_empty());
    }

    /// Go `TestRemoteAgent_AuthRefusesAnInsecureCardSource`, at the provider: inside an auth
    /// scope an insecure source is refused before any request (and before the scope's
    /// credential is touched).
    #[tokio::test]
    async fn provider_in_an_auth_scope_refuses_an_insecure_source_first() {
        use crate::remoteagent::auth::{AUTH_SCOPE, CallIdentity, CredentialCell, CredentialScope};

        struct NoCredential;
        #[async_trait::async_trait]
        impl gadk_auth::CredentialProvider for NoCredential {
            async fn credential(
                &self,
                _cx: &gadk_auth::AuthContext,
            ) -> Result<Arc<dyn gadk_auth::Credential>, gadk_auth::Error> {
                panic!("the credential must not be requested")
            }
        }

        let id = CallIdentity {
            scope: CredentialScope("app/user/session/agent".into()),
            identity: None,
            auth: Arc::new(NoCredential),
            card_fetch_client: Some(reqwest::Client::new()),
            cell: Arc::new(CredentialCell::new()),
        };
        let provider =
            new_agent_card_provider("http://remote.invalid", CardResolveOptions::default());
        let err = AUTH_SCOPE.scope(id, provider()).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "untrusted agent card interface: agent card source \"http://remote.invalid\" must use https, or http on a loopback host, when A2AConfig.Auth is set"
        );
    }

    /// Go `TestNewAgentCardProvider_ReadsLocalFile`, plus a directory holding a colon (an
    /// absolute path: changing the process directory would race the other tests).
    #[tokio::test]
    async fn provider_reads_a_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.json");
        std::fs::write(&path, br#"{"name":"file-card"}"#).unwrap();
        let card = new_agent_card_provider(path.to_str().unwrap(), CardResolveOptions::default())()
            .await
            .unwrap();
        assert_eq!(card.name, "file-card");

        let colon = dir.path().join("cards:v2");
        std::fs::create_dir(&colon).unwrap();
        let path = colon.join("http:card.json");
        std::fs::write(&path, br#"{"name":"colon-card"}"#).unwrap();
        let card = new_agent_card_provider(path.to_str().unwrap(), CardResolveOptions::default())()
            .await
            .unwrap();
        assert_eq!(card.name, "colon-card");
    }

    #[tokio::test]
    async fn provider_reports_file_errors_as_go_does() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let missing = missing.to_str().unwrap();
        let err = new_agent_card_provider(missing, CardResolveOptions::default())()
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "failed to read agent card from \"{missing}\": open {missing}: no such file or directory"
            )
        );

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{").unwrap();
        let err = new_agent_card_provider(bad.to_str().unwrap(), CardResolveOptions::default())()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("failed to unmarshal an agent card: "),
            "{err}"
        );
    }

    /// Go `TestNewAgentCardProvider_RejectsNonHTTPScheme`.
    #[tokio::test]
    async fn provider_rejects_a_non_http_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.json");
        std::fs::write(&path, br#"{"name":"file-card"}"#).unwrap();
        for source in [
            format!("file://{}", path.to_str().unwrap()),
            "ftp://example.test/card.json".to_owned(),
        ] {
            let err = new_agent_card_provider(source.clone(), CardResolveOptions::default())()
                .await
                .unwrap_err();
            assert!(
                err.to_string().starts_with(UNSUPPORTED_CARD_SOURCE),
                "{source}: {err}"
            );
        }
    }
}
