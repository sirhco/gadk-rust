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

//! Ports of ADK-Go `agentregistry/registry_test.go`, plus the spec's extra endpoint-selection
//! and quota-project precedence tests. Go's `t.Setenv` becomes a [`MapEnv`] passed to
//! `Client::new_with_env` / `endpoint_for` / `quota_project_id` / `adc_json`.

use std::collections::HashMap;

use gadk_platform::env::MapEnv;

use crate::client::{list_values, with_filter, with_page_size, with_page_token};
use crate::config::{
    BASE_URL_MTLS, BASE_URL_PROD, Client, Config, adc_json, endpoint_for, quota_project_id,
};
use crate::error::Error;

fn env(vars: &[(&str, &str)]) -> MapEnv {
    MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<HashMap<_, _>>(),
    )
}

/// Go `TestNew_Validation`.
#[tokio::test]
async fn test_new_validation() {
    let cases = [
        ("missing project", "", "us-central1"),
        ("missing location", "p", ""),
        ("missing both", "", ""),
    ];
    for (name, project_id, location) in cases {
        let cfg = Config {
            project_id: project_id.to_owned(),
            location: location.to_owned(),
            http_client: Some(reqwest::Client::new()),
        };
        let err = Client::new_with_env(cfg, &env(&[])).await.unwrap_err();
        assert!(matches!(err, Error::Config), "{name}: error = {err:?}");
        assert_eq!(
            err.to_string(),
            "agentregistry: ProjectID and Location must be set",
            "{name}"
        );
    }
}

/// Go `TestNew_WiresRestRequester`.
#[tokio::test]
async fn test_new_wires_rest_requester() {
    // (name, GOOGLE_API_USE_MTLS_ENDPOINT, GOOGLE_CLOUD_QUOTA_PROJECT, base URL, user project)
    let cases = [
        ("default endpoint", "", "", BASE_URL_PROD, "my-project"),
        (
            "never keeps standard endpoint",
            "never",
            "",
            BASE_URL_PROD,
            "my-project",
        ),
        (
            "always targets mTLS endpoint",
            "always",
            "",
            BASE_URL_MTLS,
            "my-project",
        ),
        (
            "ALWAYS is case-insensitive",
            "ALWAYS",
            "",
            BASE_URL_MTLS,
            "my-project",
        ),
        // Quota env var is honored even with a custom client (adk-python parity).
        (
            "quota project env honored with custom client",
            "",
            "quota-proj",
            BASE_URL_PROD,
            "quota-proj",
        ),
    ];
    for (name, mtls_env, quota_env, want_base_url, want_user_project) in cases {
        let vars = env(&[
            ("GOOGLE_API_USE_MTLS_ENDPOINT", mtls_env),
            ("GOOGLE_CLOUD_QUOTA_PROJECT", quota_env),
        ]);
        // A custom http_client avoids ADC/network in tests.
        let cfg = Config {
            project_id: "my-project".to_owned(),
            location: "us-central1".to_owned(),
            http_client: Some(reqwest::Client::new()),
        };
        let c = Client::new_with_env(cfg, &vars).await.unwrap();
        let rt = &c.requester;
        assert_eq!(rt.base_url, want_base_url, "{name}: base_url");
        assert_eq!(
            rt.base_path, "projects/my-project/locations/us-central1",
            "{name}: base_path"
        );
        assert_eq!(rt.user_project, want_user_project, "{name}: user_project");
        assert!(
            rt.auth.is_none(),
            "{name}: a supplied client is not authenticated"
        );
    }
}

/// Go `TestNewHTTPClientEndpointKeepsVersionPath` (adapted: Go drives `htransport` directly; the
/// ADC path's endpoint choice is `endpoint_for(env, false)`).
// parity: adapted: Go drives htransport directly; the port asserts the ADC path's endpoint choice, endpoint_for(env, false)
#[test]
fn test_new_http_client_endpoint_keeps_version_path() {
    for (name, mtls_env, want) in [
        ("standard endpoint", "", BASE_URL_PROD),
        ("mTLS endpoint", "always", BASE_URL_MTLS),
    ] {
        let endpoint = endpoint_for(&env(&[("GOOGLE_API_USE_MTLS_ENDPOINT", mtls_env)]), false);
        assert_eq!(endpoint, want, "{name}");
        // The version path must not be stripped.
        assert!(endpoint.ends_with("/v1"), "{name}: endpoint = {endpoint}");
    }
}

/// Go `TestCustomClientBaseURL`.
#[test]
fn test_custom_client_base_url() {
    for (mtls_env, want) in [
        ("", BASE_URL_PROD),
        ("never", BASE_URL_PROD),
        ("auto", BASE_URL_PROD),
        ("always", BASE_URL_MTLS),
        ("ALWAYS", BASE_URL_MTLS),
    ] {
        let got = endpoint_for(&env(&[("GOOGLE_API_USE_MTLS_ENDPOINT", mtls_env)]), true);
        assert_eq!(got, want, "endpoint_for(custom) with {mtls_env:?}");
    }
}

/// Go `TestQuotaProjectID`.
#[test]
fn test_quota_project_id() {
    let json = br#"{"quota_project_id":"json-project"}"#.as_slice();

    // env var takes precedence
    let got = quota_project_id(
        &env(&[("GOOGLE_CLOUD_QUOTA_PROJECT", "env-project")]),
        Some(json),
    );
    assert_eq!(got, "env-project");

    // from credentials JSON
    let got = quota_project_id(&env(&[("GOOGLE_CLOUD_QUOTA_PROJECT", "")]), Some(json));
    assert_eq!(got, "json-project");

    // JSON without quota project
    let got = quota_project_id(
        &env(&[("GOOGLE_CLOUD_QUOTA_PROJECT", "")]),
        Some(br#"{"type":"service_account"}"#.as_slice()),
    );
    assert_eq!(got, "");

    // nil credentials
    let got = quota_project_id(&env(&[("GOOGLE_CLOUD_QUOTA_PROJECT", "")]), None);
    assert_eq!(got, "");
}

/// Go `TestListValues`.
#[test]
fn test_list_values() {
    let got = list_values(&[
        with_filter("type=A2A"),
        with_page_size(25),
        with_page_token("tok"),
    ]);
    assert_eq!(
        got,
        [
            ("filter".to_owned(), "type=A2A".to_owned()),
            ("pageSize".to_owned(), "25".to_owned()),
            ("pageToken".to_owned(), "tok".to_owned()),
        ]
    );

    // Zero/empty options should not set parameters.
    let empty = list_values(&[with_filter(""), with_page_size(0), with_page_token("")]);
    assert!(
        empty.is_empty(),
        "list_values with empty options = {empty:?}"
    );
}

// --- Extra tests (spec § Testing): endpoint selection and quota-project precedence ---

/// Every `GOOGLE_API_USE_MTLS_ENDPOINT` value on both paths, and the ADC path's fallback to the
/// deprecated `GOOGLE_API_USE_MTLS` (google-api `getMTLSMode`), which a caller-supplied client
/// ignores (Go `customClientBaseURL`).
#[test]
fn endpoint_selection_per_variable_value() {
    // (GOOGLE_API_USE_MTLS_ENDPOINT, GOOGLE_API_USE_MTLS, ADC path, caller client)
    let cases = [
        (None, None, BASE_URL_PROD, BASE_URL_PROD),
        (Some(""), None, BASE_URL_PROD, BASE_URL_PROD),
        (Some("auto"), None, BASE_URL_PROD, BASE_URL_PROD),
        (Some("never"), None, BASE_URL_PROD, BASE_URL_PROD),
        (Some("always"), None, BASE_URL_MTLS, BASE_URL_MTLS),
        (Some("Always"), None, BASE_URL_MTLS, BASE_URL_MTLS),
        (Some("ALWAYS"), None, BASE_URL_MTLS, BASE_URL_MTLS),
        (Some(" always"), None, BASE_URL_PROD, BASE_URL_PROD),
        (Some("bogus"), None, BASE_URL_PROD, BASE_URL_PROD),
        (None, Some("always"), BASE_URL_MTLS, BASE_URL_PROD),
        (Some(""), Some("ALWAYS"), BASE_URL_MTLS, BASE_URL_PROD),
        (None, Some("never"), BASE_URL_PROD, BASE_URL_PROD),
        (Some("never"), Some("always"), BASE_URL_PROD, BASE_URL_PROD),
        (Some("always"), Some("never"), BASE_URL_MTLS, BASE_URL_MTLS),
    ];
    for (endpoint_var, legacy_var, want_adc, want_custom) in cases {
        let mut vars = Vec::new();
        if let Some(v) = endpoint_var {
            vars.push(("GOOGLE_API_USE_MTLS_ENDPOINT", v));
        }
        if let Some(v) = legacy_var {
            vars.push(("GOOGLE_API_USE_MTLS", v));
        }
        let e = env(&vars);
        assert_eq!(endpoint_for(&e, false), want_adc, "ADC path, {vars:?}");
        assert_eq!(
            endpoint_for(&e, true),
            want_custom,
            "caller client, {vars:?}"
        );
    }
}

/// `GOOGLE_CLOUD_QUOTA_PROJECT`, then the ADC JSON's `quota_project_id` (read from
/// `GOOGLE_APPLICATION_CREDENTIALS`, else the gcloud well-known file under `HOME`), then
/// nothing (`Client::new_with_env` then falls back to the project ID).
#[test]
fn quota_project_precedence_with_adc_json_file() {
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("sa.json");
    std::fs::write(
        &named,
        br#"{"type":"service_account","quota_project_id":"named-qp"}"#,
    )
    .unwrap();
    let home = dir.path().join("home");
    let gcloud = home.join(".config").join("gcloud");
    std::fs::create_dir_all(&gcloud).unwrap();
    std::fs::write(
        gcloud.join("application_default_credentials.json"),
        br#"{"type":"authorized_user","quota_project_id":"well-known-qp"}"#,
    )
    .unwrap();
    let named = named.to_str().unwrap();
    let home = home.to_str().unwrap();
    let missing = dir.path().join("missing.json");
    let missing = missing.to_str().unwrap();

    let resolve = |vars: &[(&str, &str)]| {
        let e = env(vars);
        quota_project_id(&e, adc_json(&e).as_deref())
    };

    // The variable wins over any credentials file.
    assert_eq!(
        resolve(&[
            ("GOOGLE_CLOUD_QUOTA_PROJECT", "env-qp"),
            ("GOOGLE_APPLICATION_CREDENTIALS", named),
            ("HOME", home),
        ]),
        "env-qp"
    );
    // GOOGLE_APPLICATION_CREDENTIALS wins over the well-known file.
    assert_eq!(
        resolve(&[("GOOGLE_APPLICATION_CREDENTIALS", named), ("HOME", home)]),
        "named-qp"
    );
    // An empty variable is unset.
    assert_eq!(
        resolve(&[
            ("GOOGLE_CLOUD_QUOTA_PROJECT", ""),
            ("GOOGLE_APPLICATION_CREDENTIALS", ""),
            ("HOME", home),
        ]),
        if cfg!(windows) { "" } else { "well-known-qp" }
    );
    // A named file that cannot be read carries no JSON (Go fails ADC discovery first).
    assert_eq!(
        resolve(&[("GOOGLE_APPLICATION_CREDENTIALS", missing), ("HOME", home)]),
        ""
    );
    // No credentials JSON at all (metadata-server credentials).
    assert_eq!(resolve(&[]), "");
    // Unparsable JSON is no quota project.
    assert_eq!(
        quota_project_id(&env(&[]), Some(b"{not json".as_slice())),
        ""
    );
}

/// `Client::new_with_env` falls back to the project ID when no quota project is configured, and
/// a caller-supplied client never reads the ADC JSON.
#[tokio::test]
async fn quota_project_falls_back_to_project_id_for_custom_client() {
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("sa.json");
    std::fs::write(&named, br#"{"quota_project_id":"json-qp"}"#).unwrap();
    let vars = env(&[("GOOGLE_APPLICATION_CREDENTIALS", named.to_str().unwrap())]);
    let cfg = Config {
        project_id: "my-project".to_owned(),
        location: "us-central1".to_owned(),
        http_client: Some(reqwest::Client::new()),
    };
    let c = Client::new_with_env(cfg, &vars).await.unwrap();
    assert_eq!(c.requester.user_project, "my-project");
}
