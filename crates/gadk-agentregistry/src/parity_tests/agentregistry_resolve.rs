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

//! Ports of ADK-Go `agentregistry/resolve_test.go`.

use crate::resolve::{
    BINDING_GRPC, BINDING_HTTP_JSON, BINDING_JSONRPC, ConnectionUri, TRANSPORT_GRPC,
    TRANSPORT_HTTP_JSON, TRANSPORT_JSONRPC, clean_name, connection_uri, transport_binding,
};
use crate::types::{Interface, Protocol};

fn iface(url: &str, binding: &str) -> Interface {
    Interface {
        url: url.to_owned(),
        protocol_binding: binding.to_owned(),
    }
}

/// Go `TestCleanName`.
#[test]
fn test_clean_name() {
    let cases = [
        ("already valid", "my_agent", "my_agent"),
        ("spaces and dashes", "My Cool-Agent", "My_Cool_Agent"),
        ("collapse underscores", "a  b--c", "a_b_c"),
        ("trim underscores", "__agent__", "agent"),
        ("leading digit", "123agent", "_123agent"),
        (
            "resource path",
            "projects/p/locations/l/agents/foo",
            "projects_p_locations_l_agents_foo",
        ),
        ("empty", "", ""),
        ("only specials", "!!!", ""),
    ];
    for (name, input, want) in cases {
        assert_eq!(clean_name(input), want, "{name}: clean_name({input:?})");
    }
    // Go's regexp matches per UTF-8 character: a multi-byte character is one `_`.
    assert_eq!(clean_name("café-bar"), "caf_bar");
}

/// Go `TestConnectionURI`.
#[test]
fn test_connection_uri() {
    let agent_protocols = vec![
        Protocol {
            protocol_type: "A2A_AGENT".to_owned(),
            protocol_version: "0.3.0".to_owned(),
            interfaces: vec![
                iface("https://a.example/jsonrpc", BINDING_JSONRPC),
                iface("https://a.example/http", BINDING_HTTP_JSON),
            ],
        },
        Protocol {
            protocol_type: "CUSTOM".to_owned(),
            protocol_version: String::new(),
            interfaces: vec![iface("https://custom.example", BINDING_GRPC)],
        },
    ];
    let found = |uri: &str, version: &str, binding: &'static str| {
        Some(ConnectionUri {
            uri: uri.to_owned(),
            version: version.to_owned(),
            binding,
        })
    };

    /// (name, protocols, ifaces, protocol type, transport, want)
    type Case = (
        &'static str,
        Vec<Protocol>,
        Vec<Interface>,
        &'static str,
        &'static str,
        Option<ConnectionUri>,
    );
    let cases: Vec<Case> = vec![
        (
            "type filter picks first interface",
            agent_protocols.clone(),
            vec![],
            "A2A_AGENT",
            "",
            found("https://a.example/jsonrpc", "0.3.0", TRANSPORT_JSONRPC),
        ),
        (
            // Filter and result are A2A transports: wire HTTP_JSON -> a2a HTTP+JSON.
            "transport filter picks matching interface (wire->a2a mapping)",
            agent_protocols.clone(),
            vec![],
            "",
            TRANSPORT_HTTP_JSON,
            found("https://a.example/http", "0.3.0", TRANSPORT_HTTP_JSON),
        ),
        (
            "type and transport filter",
            agent_protocols.clone(),
            vec![],
            "A2A_AGENT",
            TRANSPORT_HTTP_JSON,
            found("https://a.example/http", "0.3.0", TRANSPORT_HTTP_JSON),
        ),
        (
            "no filters returns first",
            agent_protocols.clone(),
            vec![],
            "",
            "",
            found("https://a.example/jsonrpc", "0.3.0", TRANSPORT_JSONRPC),
        ),
        (
            "top-level interfaces treated as typeless protocol",
            vec![],
            vec![iface("https://top.example", BINDING_JSONRPC)],
            "",
            TRANSPORT_JSONRPC,
            found("https://top.example", "", TRANSPORT_JSONRPC),
        ),
        (
            "type filter excludes typeless top-level interfaces",
            vec![],
            vec![iface("https://top.example", BINDING_JSONRPC)],
            "A2A_AGENT",
            "",
            None,
        ),
        (
            // Unrecognized binding -> empty transport (Python's None), still returned unfiltered.
            "unknown binding returns empty transport",
            vec![Protocol {
                protocol_type: String::new(),
                protocol_version: "1.0".to_owned(),
                interfaces: vec![iface("https://x.example", "WEIRD")],
            }],
            vec![],
            "",
            "",
            found("https://x.example", "1.0", ""),
        ),
        (
            "no match returns not ok",
            agent_protocols.clone(),
            vec![],
            "",
            "UNKNOWN",
            None,
        ),
        ("empty returns not ok", vec![], vec![], "", "", None),
    ];
    for (name, protocols, ifaces, protocol_type, transport, want) in cases {
        assert_eq!(
            connection_uri(&protocols, &ifaces, protocol_type, transport),
            want,
            "{name}"
        );
    }
}

/// Go `TestTransportBinding`.
#[test]
fn test_transport_binding() {
    let cases = [
        ("JSONRPC", Some(TRANSPORT_JSONRPC)),
        ("HTTP_JSON", Some(TRANSPORT_HTTP_JSON)),
        ("GRPC", Some(TRANSPORT_GRPC)),
        ("http+json", None),
        ("", None),
    ];
    for (input, want) in cases {
        assert_eq!(
            transport_binding(input),
            want,
            "transport_binding({input:?})"
        );
    }
}
