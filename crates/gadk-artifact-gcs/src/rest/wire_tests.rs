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

//! Wire tests of [`RestBucket`] against the `gadk_model::rest::test_util` fakes: base URL and
//! authentication resolution, URLs and query strings, headers, the multipart upload body,
//! status mapping and retries.

use std::collections::HashMap;

use gadk_model::rest::test_util::{FakeResponse, FakeServer, ScriptedClient};
use gadk_platform::env::MapEnv;
use google_cloud_auth::credentials::{CacheableResource, CredentialsProvider, EntityTag};
use google_cloud_auth::errors::CredentialsError;
use pretty_assertions::assert_eq;

use super::*;
use crate::testutil::multipart_parts;

/// Credentials whose headers are `Authorization: Bearer test-token`.
#[derive(Debug)]
struct TestCreds;

impl CredentialsProvider for TestCreds {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
        Ok(CacheableResource::New {
            entity_tag: EntityTag::new(),
            data: HeaderMap::from_iter([(
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer test-token"),
            )]),
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// An environment holding exactly `vars`.
pub(crate) fn env(vars: &[(&str, &str)]) -> Arc<MapEnv> {
    Arc::new(MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<HashMap<_, _>>(),
    ))
}

/// Test credentials and an empty environment.
fn config() -> GcsConfig {
    GcsConfig {
        credentials: Some(Credentials::from(TestCreds)),
        env: env(&[]),
        ..Default::default()
    }
}

/// Bucket `bkt` on `server`, authenticated with the test credentials.
fn bucket_on(server: &FakeServer) -> RestBucket {
    RestBucket::from_config(
        "bkt",
        GcsConfig {
            endpoint: Some(server.url()),
            ..config()
        },
    )
    .unwrap()
}

/// Bucket `bkt` configured by `cfg`, sending through `client` (no socket).
fn bucket_with(client: Arc<ScriptedClient>, cfg: GcsConfig) -> RestBucket {
    RestBucket::from_config(
        "bkt",
        GcsConfig {
            http_client: Some(client),
            ..cfg
        },
    )
    .unwrap()
}

fn busy() -> FakeResponse {
    FakeResponse::json_status(503, r#"{"error":{"code":503,"message":"busy"}}"#)
}

#[tokio::test]
async fn default_base_url_is_storage_googleapis_com() {
    let client = ScriptedClient::json(r#"{"items":[{"name":"a"}]}"#);
    let bucket = bucket_with(client.clone(), config());
    assert_eq!(bucket.list("p/", false).await.unwrap(), ["a"]);
    assert_eq!(
        client.last_request().url,
        "https://storage.googleapis.com/storage/v1/b/bkt/o?prefix=p%2F"
    );
}

#[tokio::test]
async fn endpoint_with_a_storage_v1_path_is_trimmed() {
    let client = ScriptedClient::json("{}");
    let cfg = GcsConfig {
        endpoint: Some("http://127.0.0.1:9/storage/v1/".into()),
        ..config()
    };
    bucket_with(client.clone(), cfg)
        .list("p", false)
        .await
        .unwrap();
    assert_eq!(
        client.last_request().url,
        "http://127.0.0.1:9/storage/v1/b/bkt/o?prefix=p"
    );
}

#[test]
fn endpoint_with_another_path_is_rejected() {
    let cfg = GcsConfig {
        endpoint: Some("http://h/other".into()),
        ..config()
    };
    let err = GcsArtifactService::new("bkt", cfg).unwrap_err();
    assert!(matches!(err, ArtifactError::InvalidRequest(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "invalid GCS endpoint \"http://h/other\": want http(s)://host[:port], optionally ending \
         in /storage/v1"
    );
}

#[tokio::test]
async fn emulator_host_sets_the_base_url_and_turns_authentication_off() {
    for (host, endpoint, want) in [
        (
            "localhost:9023",
            None,
            "http://localhost:9023/storage/v1/b/bkt/o?prefix=p",
        ),
        (
            "https://emu.example:8443/ignored",
            None,
            "https://emu.example:8443/storage/v1/b/bkt/o?prefix=p",
        ),
        (
            "localhost:9023",
            Some("http://127.0.0.1:7"),
            "http://127.0.0.1:7/storage/v1/b/bkt/o?prefix=p",
        ),
    ] {
        let client = ScriptedClient::json("{}");
        let cfg = GcsConfig {
            endpoint: endpoint.map(str::to_owned),
            env: env(&[("STORAGE_EMULATOR_HOST", host)]),
            ..config()
        };
        bucket_with(client.clone(), cfg)
            .list("p", false)
            .await
            .unwrap();
        let req = client.last_request();
        assert_eq!(req.url, want, "{host}");
        assert_eq!(req.header("authorization"), "", "{host}");
    }
    // Without credentials the emulator needs no Application Default Credentials.
    let client = ScriptedClient::json("{}");
    let cfg = GcsConfig {
        credentials: None,
        env: env(&[("STORAGE_EMULATOR_HOST", "localhost:9023")]),
        ..config()
    };
    bucket_with(client.clone(), cfg)
        .list("p", false)
        .await
        .unwrap();
    assert_eq!(client.last_request().header("authorization"), "");
}

#[tokio::test]
async fn requests_carry_credentials_custom_headers_and_the_tracking_user_agent() {
    let server = FakeServer::json(r#"{"items":[]}"#);
    let mut headers = HeaderMap::new();
    headers.insert("x-custom", HeaderValue::from_static("1"));
    let cfg = GcsConfig {
        endpoint: Some(server.url()),
        headers,
        ..config()
    };
    RestBucket::from_config("bkt", cfg)
        .unwrap()
        .list("p", false)
        .await
        .unwrap();
    let req = server.last_request();
    assert_eq!(req.header("authorization"), "Bearer test-token");
    assert_eq!(req.header("x-custom"), "1");
    assert_eq!(req.header("user-agent"), gadk_platform::tracking_header());
}

#[test]
fn adc_failure_is_failed_to_create_gcs_service() {
    let missing = std::env::temp_dir()
        .join(format!("gadk-artifact-gcs-missing-{}", std::process::id()))
        .join("adc.json");
    let cfg = GcsConfig {
        credentials: None,
        env: env(&[("GOOGLE_APPLICATION_CREDENTIALS", &missing.to_string_lossy())]),
        ..config()
    };
    let err = GcsArtifactService::new("bkt", cfg).unwrap_err();
    assert!(matches!(err, ArtifactError::Backend(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with("failed to create gcs service: "),
        "{err}"
    );
    assert!(
        !err.to_string().contains(gadk_model::gcp_auth::NO_RUNTIME),
        "{err}"
    );
}

// A plain test: no Tokio runtime is running, which must be an error rather than a panic.
#[test]
fn adc_outside_a_runtime_is_an_error() {
    let path = std::env::temp_dir().join(format!(
        "gadk-artifact-gcs-adc-{}-{:x}.json",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::write(
        &path,
        r#"{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r"}"#,
    )
    .unwrap();
    let cfg = GcsConfig {
        credentials: None,
        env: env(&[("GOOGLE_APPLICATION_CREDENTIALS", &path.to_string_lossy())]),
        ..config()
    };
    let result = GcsArtifactService::new("bkt", cfg);
    let _ = std::fs::remove_file(&path);
    let err = result.unwrap_err();
    assert!(matches!(err, ArtifactError::Backend(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!(
            "failed to create gcs service: {}",
            gadk_model::gcp_auth::NO_RUNTIME
        )
    );
}

#[tokio::test]
async fn listing_follows_pages_and_selects_names() {
    let server = FakeServer::start(|req| {
        if req
            .query
            .as_deref()
            .is_some_and(|q| q.contains("pageToken=t2"))
        {
            FakeResponse::json(r#"{"items":[{"name":"app/u/s/c/1"}]}"#)
        } else {
            FakeResponse::json(
                r#"{"items":[{"name":"app/u/s/a/1"},{"name":"app/u/s/b/1"}],"nextPageToken":"t2"}"#,
            )
        }
    });
    let names = bucket_on(&server).list("app/u/s/", true).await.unwrap();
    assert_eq!(names, ["app/u/s/a/1", "app/u/s/b/1", "app/u/s/c/1"]);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(
        reqs[0].path_and_query(),
        "/storage/v1/b/bkt/o?prefix=app%2Fu%2Fs%2F&fields=items%28name%29%2CnextPageToken"
    );
    assert_eq!(
        reqs[1].path_and_query(),
        "/storage/v1/b/bkt/o?prefix=app%2Fu%2Fs%2F&fields=items%28name%29%2CnextPageToken&pageToken=t2"
    );
}

#[tokio::test]
async fn attrs_decode_the_object_resource() {
    let server = FakeServer::json(
        r#"{"name":"app one/u/s/f.txt/1","contentType":"image/png","metadata":{"k":"v"},"timeCreated":"2026-08-31T12:34:56.789Z","mediaLink":"https://storage.googleapis.com/download/x"}"#,
    );
    let attrs = bucket_on(&server)
        .attrs("app one/u/s/f.txt/1")
        .await
        .unwrap();
    assert_eq!(
        attrs,
        ObjectAttrs {
            content_type: "image/png".into(),
            metadata: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
            created: "2026-08-31T12:34:56.789Z".parse().unwrap(),
            media_link: "https://storage.googleapis.com/download/x".into(),
        }
    );
    let req = server.last_request();
    assert_eq!(req.method, "GET");
    assert_eq!(
        req.path_and_query(),
        "/storage/v1/b/bkt/o/app%20one%2Fu%2Fs%2Ff.txt%2F1"
    );
}

#[tokio::test]
async fn read_downloads_with_alt_media() {
    let server = FakeServer::always(
        FakeResponse::new(200)
            .header("Content-Type", "application/octet-stream")
            .chunk("bytes"),
    );
    let data = bucket_on(&server).read("a/user:é/1").await.unwrap();
    assert_eq!(data, "bytes");
    assert_eq!(
        server.last_request().path_and_query(),
        "/storage/v1/b/bkt/o/a%2Fuser%3A%C3%A9%2F1?alt=media"
    );
}

// Go's googleapi.Expand escapes path segments to the unreserved set, where url.PathEscape
// would keep `$&+:=@`.
#[tokio::test]
async fn object_paths_escape_all_but_unreserved_bytes() {
    let server = FakeServer::start(|req| match req.method.as_str() {
        "DELETE" => FakeResponse::new(204),
        _ if req.query.as_deref() == Some("alt=media") => FakeResponse::new(200).chunk("x"),
        _ => FakeResponse::json(r#"{"name":"n"}"#),
    });
    let bucket = bucket_on(&server);
    let name = "app/u/s/user:a+b=c@d$&.txt/0";
    bucket.attrs(name).await.unwrap();
    bucket.read(name).await.unwrap();
    bucket.delete(name).await.unwrap();
    let want = "/storage/v1/b/bkt/o/app%2Fu%2Fs%2Fuser%3Aa%2Bb%3Dc%40d%24%26.txt%2F0";
    let reqs = server.requests();
    let got: Vec<(String, String)> = reqs
        .iter()
        .map(|r| (r.method.clone(), r.path_and_query()))
        .collect();
    assert_eq!(
        got,
        [
            ("GET".to_owned(), want.to_owned()),
            ("GET".to_owned(), format!("{want}?alt=media")),
            ("DELETE".to_owned(), want.to_owned()),
        ]
    );
}

#[tokio::test]
async fn upload_is_one_conditional_multipart_request() {
    let server = FakeServer::json(r#"{"name":"a/b/1","generation":"1"}"#);
    let metadata = BTreeMap::from([("k".to_owned(), "v".to_owned())]);
    bucket_on(&server)
        .insert_if_absent("a/b/1", "image/png", &metadata, Bytes::from_static(b"data"))
        .await
        .unwrap();
    let req = server.last_request();
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        "/upload/storage/v1/b/bkt/o?uploadType=multipart&ifGenerationMatch=0"
    );
    let content_type = req.header("content-type");
    let boundary = content_type
        .strip_prefix("multipart/related; boundary=")
        .unwrap();
    assert_eq!(boundary.len(), 32);
    assert_eq!(
        req.body_text(),
        format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n\
             {{\"name\":\"a/b/1\",\"contentType\":\"image/png\",\"metadata\":{{\"k\":\"v\"}}}}\r\n\
             --{boundary}\r\nContent-Type: image/png\r\n\r\ndata\r\n--{boundary}--\r\n"
        )
    );
}

#[tokio::test]
async fn upload_without_content_type_or_metadata() {
    let server = FakeServer::json(r#"{"name":"x/1","generation":"1"}"#);
    bucket_on(&server)
        .insert_if_absent("x/1", "", &BTreeMap::new(), Bytes::new())
        .await
        .unwrap();
    assert_eq!(
        multipart_parts(&server.last_request()),
        [
            (
                "Content-Type: application/json; charset=UTF-8".to_owned(),
                r#"{"name":"x/1"}"#.to_owned()
            ),
            (
                "Content-Type: application/octet-stream".to_owned(),
                String::new()
            ),
        ]
    );
}

#[tokio::test]
async fn status_errors_map_to_gcs_errors() {
    let server = FakeServer::always(FakeResponse::json_status(
        404,
        r#"{"error":{"code":404,"message":"No such object: bkt/x"}}"#,
    ));
    let bucket = bucket_on(&server);
    assert!(matches!(
        bucket.attrs("x").await,
        Err(GcsError::ObjectNotExist)
    ));
    assert!(matches!(
        bucket.read("x").await,
        Err(GcsError::ObjectNotExist)
    ));
    assert!(matches!(
        bucket.delete("x").await,
        Err(GcsError::ObjectNotExist)
    ));
    // A listing's 404 (a missing bucket) is not an object error.
    let err = bucket.list("p", false).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "googleapi: Error 404: No such object: bkt/x"
    );

    let server = FakeServer::always(FakeResponse::json_status(
        412,
        r#"{"error":{"code":412,"message":"At least one of the pre-conditions you specified did not hold.","errors":[{"reason":"conditionNotMet"}]}}"#,
    ));
    let err = bucket_on(&server)
        .insert_if_absent("x/1", "text/plain", &BTreeMap::new(), Bytes::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, GcsError::PreconditionFailed { .. }),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "googleapi: Error 412: At least one of the pre-conditions you specified did not hold."
    );

    // An upload's 404 (a missing bucket) is not an object error either: Go's writer leaves it
    // a googleapi error.
    let server = FakeServer::always(FakeResponse::json_status(
        404,
        r#"{"error":{"code":404,"message":"The specified bucket does not exist."}}"#,
    ));
    let err = bucket_on(&server)
        .insert_if_absent("x/1", "text/plain", &BTreeMap::new(), Bytes::new())
        .await
        .unwrap_err();
    assert!(matches!(err, GcsError::Http { status: 404, .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "googleapi: Error 404: The specified bucket does not exist."
    );

    let server = FakeServer::always(FakeResponse::new(403).chunk("denied"));
    let err = bucket_on(&server).attrs("x").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "googleapi: got HTTP response code 403 with body: denied"
    );
}

#[tokio::test(start_paused = true)]
async fn list_retries_a_503() {
    let client = ScriptedClient::sequence(vec![
        busy(),
        FakeResponse::json(r#"{"items":[{"name":"a"}]}"#),
    ]);
    let names = bucket_with(client.clone(), config())
        .list("p", false)
        .await
        .unwrap();
    assert_eq!(names, ["a"]);
    assert_eq!(client.hits(), 2);
}

#[tokio::test(start_paused = true)]
async fn retries_stop_after_three() {
    let client = ScriptedClient::always(busy());
    let err = bucket_with(client.clone(), config())
        .attrs("a/1")
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "googleapi: Error 503: busy");
    assert_eq!(client.hits(), 4);
}

#[tokio::test(start_paused = true)]
async fn delete_is_not_retried() {
    let client = ScriptedClient::always(busy());
    let err = bucket_with(client.clone(), config())
        .delete("a/1")
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "googleapi: Error 503: busy");
    assert_eq!(client.hits(), 1);
    assert_eq!(client.last_request().method, "DELETE");
}
