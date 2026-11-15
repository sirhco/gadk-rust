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

//! Ports of ADK-Go `server/adkrest/internal/models/event_test.go`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::{
    EventActions, EventCompaction, NodeInfo, RequestInput, ToolConfirmation, toolauth, zero_time,
};
use serde_json::{Value, json};

use crate::rest::models;

#[test]
fn test_event_round_trip_preserves_workflow_fields() {
    let ev = gadk_session::Event {
        id: "e1".into(),
        timestamp: DateTime::from_timestamp_micros(1733040000123456).unwrap(),
        author: "agent".into(),
        isolation_scope: "scope-1".into(),
        routes: vec!["next-node".into(), "fallback".into()],
        requested_input: Some(RequestInput {
            interrupt_id: "approve".into(),
            message: "ok?".into(),
            ..Default::default()
        }),
        node_info: Some(NodeInfo {
            path: "root/child".into(),
            message_as_output: true,
            ..Default::default()
        }),
        output: Some(json!("hello")),
        actions: EventActions {
            requested_tool_confirmations: BTreeMap::from([(
                "call-1".to_owned(),
                ToolConfirmation {
                    hint: "please confirm".into(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
        ..Default::default()
    };

    let rest_event = models::Event::from_session_event(&ev);
    assert_eq!(
        rest_event.timestamp, 1733040000.123456,
        "FromSessionEvent().Timestamp, want 1733040000.123456"
    );
    let raw_json = gadk_model::to_go_json(&rest_event).unwrap();
    let wire: Value = serde_json::from_slice(&raw_json).unwrap();
    assert_eq!(
        wire["timestamp"].as_f64(),
        Some(1733040000.123456),
        "wire[\"timestamp\"] = {}, want 1733040000.123456",
        wire["timestamp"]
    );
    let decoded_rest_event: models::Event = serde_json::from_slice(&raw_json).unwrap();

    let back = decoded_rest_event
        .to_session_event()
        .unwrap_or_else(|e| panic!("ToSessionEvent() failed: {e}"));

    assert_eq!(back.timestamp, ev.timestamp, "Timestamp");
    let zero_back = models::Event::from_session_event(&gadk_session::Event::default())
        .to_session_event()
        .unwrap_or_else(|e| panic!("ToSessionEvent(zero event) failed: {e}"));
    assert_eq!(
        zero_back.timestamp,
        zero_time(),
        "zero Timestamp round-trip, want zero time"
    );

    assert_eq!(back.isolation_scope, ev.isolation_scope, "IsolationScope");
    assert!(
        back.routes.len() == ev.routes.len() && back.routes[0] == "next-node",
        "Routes = {:?}, want {:?}",
        back.routes,
        ev.routes
    );
    assert!(
        back.requested_input
            .as_ref()
            .is_some_and(|r| r.interrupt_id == "approve"),
        "RequestedInput = {:?}, want InterruptID=approve",
        back.requested_input
    );
    assert!(
        back.node_info
            .as_ref()
            .is_some_and(|n| n.path == "root/child"),
        "NodeInfo = {:?}, want Path=root/child",
        back.node_info
    );
    assert!(
        back.actions
            .requested_tool_confirmations
            .get("call-1")
            .is_some_and(|c| c.hint == "please confirm"),
        "RequestedToolConfirmations = {:?}, want call-1 hint",
        back.actions.requested_tool_confirmations
    );
}

#[test]
fn test_compaction_is_read_only_over_rest() {
    let record = EventCompaction {
        start_timestamp: DateTime::from_timestamp(1, 0).unwrap(),
        end_timestamp: DateTime::from_timestamp(9, 0).unwrap(),
        compacted_content: Some(Content::from_text("planted", "model")),
        excluded_events: vec![],
    };

    // Inbound: a client-supplied record must not survive.
    let inbound = models::Event {
        actions: models::EventActions {
            compaction: Some(record.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let got = inbound
        .to_session_event()
        .unwrap_or_else(|e| panic!("ToSessionEvent() failed: {e}"));
    assert!(
        got.actions.compaction.is_none(),
        "ToSessionEvent kept a client-supplied compaction record; it must be dropped"
    );

    // Outbound: a stored record must be returned.
    let stored = gadk_session::Event {
        actions: EventActions {
            compaction: Some(record.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let out = models::Event::from_session_event(&stored);
    let Some(c) = out.actions.compaction else {
        panic!("FromSessionEvent dropped the stored compaction record; reads should show it");
    };
    assert_eq!(c.end_timestamp, record.end_timestamp, "EndTimestamp");
}

#[test]
fn test_requested_auth_configs_go_out_without_secrets() {
    // (name, a toolauth.Config as JSON, it without its secrets)
    let tests = [
        (
            "oauth2 client and flow state",
            r#"{
				"authScheme": {"type": "oauth2", "flows": {"authorizationCode": {
					"authorizationUrl": "https://idp.example/auth", "tokenUrl": "https://idp.example/token",
					"scopes": {"files": "Read files"}}}},
				"rawAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "clientSecret": "s3cret", "tokenEndpointAuthMethod": "client_secret_post"}},
				"exchangedAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "authUri": "https://idp.example/auth?state=st", "state": "st",
					"authResponseUri": "https://app.example/cb?code=c0de", "authCode": "c0de",
					"accessToken": "at", "refreshToken": "rt", "idToken": "it", "codeVerifier": "cv",
					"expiresAt": 1700000000, "tokenEndpointAuthMethod": "client_secret_post"}},
				"credentialKey": "drive"
			}"#,
            r#"{
				"authScheme": {"type": "oauth2", "flows": {"authorizationCode": {
					"authorizationUrl": "https://idp.example/auth", "tokenUrl": "https://idp.example/token",
					"scopes": {"files": "Read files"}}}},
				"rawAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "tokenEndpointAuthMethod": "client_secret_post"}},
				"exchangedAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "authUri": "https://idp.example/auth?state=st", "state": "st",
					"expiresAt": 1700000000, "tokenEndpointAuthMethod": "client_secret_post"}},
				"credentialKey": "drive"
			}"#,
        ),
        (
            "http password, token and headers",
            r#"{
				"authScheme": {"type": "http", "scheme": "basic"},
				"rawAuthCredential": {"authType": "http", "http": {"scheme": "basic",
					"credentials": {"username": "ann", "password": "pw", "token": "tk"},
					"additionalHeaders": {"X-Api-Key": "hk"}}},
				"credentialKey": "basic"
			}"#,
            r#"{
				"authScheme": {"type": "http", "scheme": "basic"},
				"rawAuthCredential": {"authType": "http", "http": {"scheme": "basic",
					"credentials": {"username": "ann"}}},
				"credentialKey": "basic"
			}"#,
        ),
        (
            "api key next to a service account key",
            r#"{
				"authScheme": {"type": "apiKey", "in": "header", "name": "X-Key"},
				"rawAuthCredential": {"authType": "apiKey", "apiKey": "ak"},
				"exchangedAuthCredential": {"authType": "serviceAccount", "serviceAccount": {
					"serviceAccountCredential": {"type": "service_account", "projectId": "p",
						"privateKeyId": "kid", "privateKey": "pk", "clientEmail": "sa@p.example",
						"clientId": "123", "authUri": "https://accounts.example/auth",
						"tokenUri": "https://accounts.example/token",
						"authProviderX509CertUrl": "https://certs.example/p",
						"clientX509CertUrl": "https://certs.example/c", "universeDomain": "example.com"},
					"scopes": ["cloud-platform"], "useDefaultCredential": false, "useIdToken": false}},
				"credentialKey": "mixed"
			}"#,
            r#"{
				"authScheme": {"type": "apiKey", "in": "header", "name": "X-Key"},
				"rawAuthCredential": {"authType": "apiKey"},
				"exchangedAuthCredential": {"authType": "serviceAccount", "serviceAccount": {
					"serviceAccountCredential": {"type": "service_account", "projectId": "p",
						"clientEmail": "sa@p.example",
						"clientId": "123", "authUri": "https://accounts.example/auth",
						"tokenUri": "https://accounts.example/token",
						"authProviderX509CertUrl": "https://certs.example/p",
						"clientX509CertUrl": "https://certs.example/c", "universeDomain": "example.com"},
					"scopes": ["cloud-platform"], "useDefaultCredential": false, "useIdToken": false}},
				"credentialKey": "mixed"
			}"#,
        ),
        (
            // Unknown keys keep the spelling they arrived with, so each secret name is dropped
            // here only if the list holds that exact spelling.
            "every secret key name as an unknown key",
            r#"{
				"authScheme": {"type": "apiKey", "in": "header", "name": "X-Key"},
				"credentialKey": "extra",
				"keep": "me",
				"password": "s", "token": "s",
				"additionalHeaders": "s", "additional_headers": "s",
				"clientSecret": "s", "client_secret": "s",
				"authResponseUri": "s", "auth_response_uri": "s",
				"authCode": "s", "auth_code": "s",
				"accessToken": "s", "access_token": "s",
				"refreshToken": "s", "refresh_token": "s",
				"idToken": "s", "id_token": "s",
				"codeVerifier": "s", "code_verifier": "s",
				"privateKeyId": "s", "private_key_id": "s",
				"privateKey": "s", "private_key": "s",
				"apiKey": "s", "api_key": "s"
			}"#,
            r#"{
				"authScheme": {"type": "apiKey", "in": "header", "name": "X-Key"},
				"credentialKey": "extra",
				"keep": "me"
			}"#,
        ),
        (
            "unknown key keeps its digits and loses nested secrets",
            r#"{
				"authScheme": {"type": "oauth2", "flows": {"clientCredentials": {
					"tokenUrl": "https://idp.example/token", "scopes": {}}}},
				"exchangedAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "tokenEndpointAuthMethod": "client_secret_post",
					"provider": {"user": "ann", "id": 9007199254740993, "refresh_token": "rt",
						"sessions": [{"access_token": "at", "scope": "files"}]}}},
				"credentialKey": "cc"
			}"#,
            r#"{
				"authScheme": {"type": "oauth2", "flows": {"clientCredentials": {
					"tokenUrl": "https://idp.example/token", "scopes": {}}}},
				"exchangedAuthCredential": {"authType": "oauth2", "oauth2": {
					"clientId": "app", "tokenEndpointAuthMethod": "client_secret_post",
					"provider": {"user": "ann", "id": 9007199254740993,
						"sessions": [{"scope": "files"}]}}},
				"credentialKey": "cc"
			}"#,
        ),
    ];
    for (name, input, want) in tests {
        let cfg: toolauth::Config = serde_json::from_str(input)
            .unwrap_or_else(|e| panic!("{name}: json.Unmarshal(in) failed: {e}"));
        let ev = gadk_session::Event {
            actions: EventActions {
                requested_auth_configs: BTreeMap::from([("call-1".to_owned(), cfg)]),
                ..Default::default()
            },
            ..Default::default()
        };

        let b = gadk_model::to_go_json(&models::Event::from_session_event(&ev))
            .unwrap_or_else(|e| panic!("{name}: json.Marshal(event) failed: {e}"));

        // serde_json keeps an integer such as 9007199254740993 exact (Go: decodeKeepingNumbers).
        let got: Value = serde_json::from_slice(&b).unwrap();
        let want: Value = serde_json::from_str(want).unwrap();
        assert_eq!(
            want, got["actions"]["requestedAuthConfigs"]["call-1"],
            "{name}: requestedAuthConfigs[call-1] mismatch (want, got)"
        );
    }
}

// parity: skipped TestRequestedAuthConfigsEncodeFailureFailsTheEvent: a gadk toolauth::Config keeps every unknown value as JSON text, so it cannot hold Go's NaN and always encodes; the encode-failure path has no input

#[test]
fn test_requested_auth_configs_are_not_taken_from_requests() {
    let body = r#"{"events": [{"author": "user", "actions": {"requestedAuthConfigs": {
		"call-1": {"authScheme": {"type": "apiKey", "in": "header", "name": "X-Key"}}}}}]}"#;
    let req: models::CreateSessionRequest =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("json.Unmarshal(body) failed: {e}"));
    let events = req.events.unwrap_or_default();
    assert!(
        events.len() == 1 && events[0].actions.requested_auth_configs.len() == 1,
        "decoded {events:?}, want one event carrying one credential request"
    );

    let got = events[0]
        .to_session_event()
        .unwrap_or_else(|e| panic!("ToSessionEvent() failed: {e}"));
    assert!(
        got.actions.requested_auth_configs.is_empty(),
        "ToSessionEvent kept a client-supplied credential request; it must be dropped"
    );
}

// parity: skipped TestRequestCredentialPartsGoOutWithoutSecrets: 8c — sign-in (Go 1523862f #2231: adk_request_credential parts reach REST clients without credential secrets)
// parity: skipped TestOtherFunctionPartsKeepSecretLookingFields: 8c — sign-in (Go 1523862f #2231: the scope of the adk_request_credential part redaction)
// parity: skipped TestRequestCredentialRedactionLeavesTheStoredEventAlone: 8c — sign-in (Go 1523862f #2231: the part redaction copies, never strips, the stored event)
// parity: skipped TestStateDeltaCredentialsGoOutWithoutSecrets: 8c — sign-in (Go 1523862f #2231: credentials in a state delta reach REST clients without secrets)

/// Go `models.ToSessionEvent(models.Event{Timestamp: secs})`'s timestamp.
fn rest_timestamp(secs: f64) -> Result<DateTime<Utc>, String> {
    let ev = models::Event {
        timestamp: secs,
        ..Default::default()
    };
    Ok(ev.to_session_event()?.timestamp)
}

#[test]
fn test_to_session_event_timestamp_bounds_match_session_decoder() {
    for secs in [
        0.5,
        -0.5,
        1.0,
        1733040000.123456,
        1733040000123.0, // epoch milliseconds, as JavaScript's Date.now() returns
        1e13,            // overflows the int64 conversion without a bound
        -1e13,
        1e300,
        -1e300,
        -62135596800.0,      // first second of year 1
        -62135596800.000001, // just before it
        253402300799.0,      // last second of year 9999
        253402300799.999999, // rounds up into year 10000
        253402300800.0,      // first second of year 10000
    ] {
        let rest = rest_timestamp(secs);
        let decoded = serde_json::from_value::<gadk_session::Event>(json!({ "timestamp": secs }));
        assert_eq!(
            rest.is_ok(),
            decoded.is_ok(),
            "timestamp {secs}: ToSessionEvent = {rest:?}, session.Event decoder = {:?}; want both ok or both errors",
            decoded
                .as_ref()
                .map(|e| e.timestamp)
                .map_err(ToString::to_string)
        );
        if let (Ok(got), Ok(decoded)) = (rest, decoded) {
            assert_eq!(
                got, decoded.timestamp,
                "timestamp {secs}: ToSessionEvent vs session.Event decoder"
            );
        }
    }
}

#[test]
// parity: adapted: a Rust content holds no nil parts (and a JSON `null` part fails gadk-genai's
// decode), so the remaining contract is pinned: the real parts keep their order and content, no
// part is `null`, and the function call carries an empty args object
fn test_marshal_event_drops_nil_parts() {
    let ev = gadk_session::Event {
        id: "e1".into(),
        response: gadk_model::LlmResponse {
            content: Some(Content::from_parts(
                vec![
                    Part::from_text("kept"),
                    Part {
                        function_call: Some(FunctionCall {
                            name: "f".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                "",
            )),
            ..Default::default()
        },
        ..Default::default()
    };
    let raw_json = gadk_model::to_go_json(&models::Event::from_session_event(&ev)).unwrap();
    let raw = String::from_utf8(raw_json).unwrap();
    let got: Value = serde_json::from_str(&raw).unwrap();
    let parts = got["content"]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "parts count, want 2: {raw}");
    for (i, p) in parts.iter().enumerate() {
        assert!(
            !p.is_null(),
            "parts[{i}] is null; output must never carry a null part: {raw}"
        );
    }
    let part = |i: usize| serde_json::to_string(&parts[i]).unwrap();
    assert!(
        part(0).contains(r#""kept""#),
        "parts[0] = {}, want the kept text part first",
        part(0)
    );
    assert!(
        part(1).contains(r#""name":"f""#) && part(1).contains(r#""args":{}"#),
        "parts[1] = {}, want the function call with empty args object",
        part(1)
    );
}
