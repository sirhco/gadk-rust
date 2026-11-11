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
// D1: the factory (Task 5) is these helpers' only caller outside the tests, and without the
// `a2a`/`mcp` features it is compiled out. Task 10 narrows this to
// `#![cfg_attr(not(any(feature = "a2a", feature = "mcp")), allow(dead_code))]`.
#![allow(dead_code)]

//! Go `agentregistry/resolve.go`: name cleaning and connection-URI resolution. The A2A
//! transport names are a2a's wire strings (`JSONRPC`, `HTTP+JSON`, `GRPC`), held here as
//! constants so the module needs no `a2a` feature.

use crate::types::{Interface, Protocol};

/// Go `protocolTypeA2AAgent`.
pub(crate) const PROTOCOL_TYPE_A2A_AGENT: &str = "A2A_AGENT";
/// Go `bindingJSONRPC`: registry protocol-binding wire value.
pub(crate) const BINDING_JSONRPC: &str = "JSONRPC";
/// Go `bindingHTTPJSON`.
pub(crate) const BINDING_HTTP_JSON: &str = "HTTP_JSON";
/// Go `bindingGRPC`.
pub(crate) const BINDING_GRPC: &str = "GRPC";

/// a2a `TransportProtocolJSONRPC` (= `a2a::TRANSPORT_PROTOCOL_JSONRPC`).
pub(crate) const TRANSPORT_JSONRPC: &str = "JSONRPC";
/// a2a `TransportProtocolHTTPJSON` (= `a2a::TRANSPORT_PROTOCOL_HTTP_JSON`).
pub(crate) const TRANSPORT_HTTP_JSON: &str = "HTTP+JSON";
/// a2a `TransportProtocolGRPC` (= `a2a::TRANSPORT_PROTOCOL_GRPC`).
pub(crate) const TRANSPORT_GRPC: &str = "GRPC";

/// Go `cleanName`: non-identifier bytes (`[^a-zA-Z0-9_]`, per UTF-8 character as Go's regexp)
/// become `_`, runs of `_` collapse, leading/trailing `_` are trimmed, and a leading digit gets a
/// `_` prefix.
pub(crate) fn clean_name(name: &str) -> String {
    let mut collapsed = String::with_capacity(name.len());
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        };
        if c == '_' && collapsed.ends_with('_') {
            continue;
        }
        collapsed.push(c);
    }
    let s = collapsed.trim_matches('_');
    // A valid identifier cannot start with a digit (after trimming, `s` never starts with `_`).
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{s}")
    } else {
        s.to_owned()
    }
}

/// Go `connectionURI`'s result: the interface URL, its protocol's version, and the
/// interface's A2A transport (`""` for an unrecognized binding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionUri {
    /// Go `uri`.
    pub(crate) uri: String,
    /// Go `version`: the protocol's `protocolVersion`.
    pub(crate) version: String,
    /// Go `binding`: a `TRANSPORT_*` constant or `""`.
    pub(crate) binding: &'static str,
}

/// Go `connectionURI`: the first interface with a non-empty URL whose protocol matches
/// `protocol_type` and whose mapped transport matches `transport` (an empty filter matches
/// anything); non-empty `ifaces` are tried last as a type-less protocol. `None` is Go's
/// `ok == false`.
pub(crate) fn connection_uri(
    protocols: &[Protocol],
    ifaces: &[Interface],
    protocol_type: &str,
    transport: &str,
) -> Option<ConnectionUri> {
    let trailing = (!ifaces.is_empty()).then(|| Protocol {
        interfaces: ifaces.to_vec(),
        ..Protocol::default()
    });
    for p in protocols.iter().chain(trailing.as_ref()) {
        if !protocol_type.is_empty() && p.protocol_type != protocol_type {
            continue;
        }
        for i in &p.interfaces {
            let mapped = transport_binding(&i.protocol_binding).unwrap_or("");
            if !transport.is_empty() && mapped != transport {
                continue;
            }
            if !i.url.is_empty() {
                return Some(ConnectionUri {
                    uri: i.url.clone(),
                    version: p.protocol_version.clone(),
                    binding: mapped,
                });
            }
        }
    }
    None
}

/// Go `transportBinding`: `JSONRPC` → `JSONRPC`, `HTTP_JSON` → `HTTP+JSON`, `GRPC` → `GRPC`;
/// anything else `None` (Go `("", false)`).
pub(crate) fn transport_binding(wire: &str) -> Option<&'static str> {
    match wire {
        BINDING_JSONRPC => Some(TRANSPORT_JSONRPC),
        BINDING_HTTP_JSON => Some(TRANSPORT_HTTP_JSON),
        BINDING_GRPC => Some(TRANSPORT_GRPC),
        _ => None,
    }
}
