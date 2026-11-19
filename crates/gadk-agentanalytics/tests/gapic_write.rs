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

//! `write::gapic::WriteClient` over the in-process fake BigQuery Write server (spec R4): the
//! per-RPC metadata of every `gapic/metadata.json` case, the retry table of
//! `gapic/retry.json` (attempts and final status, backoff bounds under paused time), the call
//! timeouts, and grpc-go's AppendRows stream semantics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gadk_agentanalytics::storagepb::{
    AppendRowsRequest, AppendRowsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FinalizeWriteStreamResponse, WriteStream, write_stream,
};
use gadk_agentanalytics::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer};
use gadk_agentanalytics::write::is_eof;
use gadk_agentanalytics::{
    ClientOptions, Code, Context, ContextError, Error, Metadata, WriteClient,
};
use gadk_model::rest::test_util::bearer_credentials;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tokio::time::Instant;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gapic")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// grpc-go's and tonic's own transport headers, left out of the comparison.
const TRANSPORT_KEYS: [&str; 5] = [
    "content-type",
    "grpc-accept-encoding",
    "user-agent",
    "te",
    "grpc-encoding",
];

fn without_transport(md: &Metadata) -> Metadata {
    md.iter()
        .filter(|(k, _)| !TRANSPORT_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The fixture's metadata with gadk's `x-goog-api-client` (ledgered) in the place of Go's.
fn expected_metadata(fixture: &Value) -> Metadata {
    let mut md: Metadata = serde_json::from_value(fixture.clone()).unwrap();
    md = without_transport(&md);
    md.insert(
        "x-goog-api-client".to_owned(),
        vec![gadk_platform::tracking_header()],
    );
    md
}

fn timeout_s(t: Option<Duration>) -> Value {
    t.map_or(Value::Null, |d| Value::from(d.as_secs_f64().round() as u64))
}

/// The caller's context of each `metadata.json` case (the probe's `about` texts).
fn case_context(name: &str) -> Context {
    let bg = Context::background();
    match name {
        "create_write_stream" | "finalize_write_stream" | "append_rows_named_stream" => {
            bg.append_outgoing_metadata(&[("x-test-caller", "preserve-me")])
        }
        "create_write_stream_caller_deadline" => bg.with_timeout(Duration::from_secs(30)),
        "create_write_stream_caller_request_params" => {
            bg.append_outgoing_metadata(&[("x-goog-request-params", "caller=1")])
        }
        "create_write_stream_caller_api_client" => {
            bg.append_outgoing_metadata(&[("x-goog-api-client", "caller-lib/1.0")])
        }
        _ => bg,
    }
}

#[tokio::test(start_paused = true)]
async fn per_rpc_metadata_matches_the_probe() {
    let cases = fixture("metadata.json");
    let cases = cases["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let call = &case["calls"][0];
        assert_eq!(case["calls"].as_array().unwrap().len(), 1, "{name}");
        let server = FakeWriteServer::new();
        let client = server.client().unwrap();
        let ctx = case_context(name);
        match call["rpc"].as_str().unwrap() {
            "CreateWriteStream" => {
                let value = case["result"]["value"].as_str().unwrap().to_owned();
                server.on_create_write_stream(Arc::new(move |_| {
                    let name = value.clone();
                    Box::pin(async move {
                        Ok(WriteStream {
                            name,
                            ..WriteStream::default()
                        })
                    })
                }));
                let parent = call["request"]["parent"].as_str().unwrap();
                assert_eq!(call["request"]["writeStream"]["type"], "COMMITTED");
                let got = client
                    .create_write_stream(
                        &ctx,
                        CreateWriteStreamRequest {
                            parent: parent.to_owned(),
                            write_stream: Some(WriteStream {
                                r#type: write_stream::Type::Committed as i32,
                                ..WriteStream::default()
                            }),
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    got.name,
                    case["result"]["value"].as_str().unwrap(),
                    "{name}"
                );
                let calls = server.create_calls();
                assert_eq!(calls.len(), 1, "{name}");
                assert_eq!(calls[0].request.parent, parent, "{name}");
                assert_eq!(
                    without_transport(&calls[0].metadata),
                    expected_metadata(&call["metadata"]),
                    "{name}"
                );
                assert_eq!(timeout_s(calls[0].timeout), call["timeout_s"], "{name}");
            }
            "FinalizeWriteStream" => {
                server.on_finalize_write_stream(Arc::new(|_| {
                    Box::pin(async { Ok(FinalizeWriteStreamResponse { row_count: 3 }) })
                }));
                let stream = call["request"]["name"].as_str().unwrap();
                let got = client
                    .finalize_write_stream(
                        &ctx,
                        FinalizeWriteStreamRequest {
                            name: stream.to_owned(),
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    got.row_count.to_string(),
                    case["result"]["value"]["rowCount"].as_str().unwrap()
                );
                let calls = server.finalize_calls();
                assert_eq!(calls.len(), 1, "{name}");
                assert_eq!(calls[0].request.name, stream, "{name}");
                assert_eq!(
                    without_transport(&calls[0].metadata),
                    expected_metadata(&call["metadata"]),
                    "{name}"
                );
                assert_eq!(timeout_s(calls[0].timeout), call["timeout_s"], "{name}");
            }
            "AppendRows" => {
                // The routing header of a stream is the stream writer's (Go
                // `metadata.AppendToOutgoingContext` in `newStreamWriter`); the client adds
                // only `x-goog-api-client`.
                let stream_name = call["requests"][0]["write_stream"].as_str().unwrap();
                let ctx = if stream_name.is_empty() {
                    ctx
                } else {
                    let routing = format!(
                        "write_stream={}",
                        gadk_platform::go_query_escape(stream_name)
                    );
                    ctx.append_outgoing_metadata(&[("x-goog-request-params", &routing)])
                };
                let mut stream = client.append_rows(&ctx).await.unwrap();
                stream
                    .send(AppendRowsRequest {
                        write_stream: stream_name.to_owned(),
                        ..AppendRowsRequest::default()
                    })
                    .await
                    .unwrap();
                stream.recv().await.unwrap();
                stream.close_send();
                assert!(is_eof(&stream.recv().await.unwrap_err()), "{name}");
                let md = server.append_stream_metadata();
                assert_eq!(md.len(), 1, "{name}");
                assert_eq!(
                    without_transport(&md[0]),
                    expected_metadata(&call["metadata"]),
                    "{name}"
                );
                assert_eq!(call["timeout_s"], Value::Null, "{name}");
                assert_eq!(
                    server.append_requests()[0].request.write_stream,
                    stream_name
                );
            }
            rpc => panic!("unexpected rpc {rpc}"),
        }
    }
}

#[tokio::test]
async fn credentials_add_authorization() {
    let server = FakeWriteServer::new();
    let client = server
        .client_with_options(&ClientOptions {
            credentials: Some(bearer_credentials("tok")),
            ..ClientOptions::default()
        })
        .unwrap();
    client
        .finalize_write_stream(
            &Context::background(),
            FinalizeWriteStreamRequest { name: "s".into() },
        )
        .await
        .unwrap();
    let mut stream = client.append_rows(&Context::background()).await.unwrap();
    stream.close_send();
    assert!(is_eof(&stream.recv().await.unwrap_err()));
    assert_eq!(
        server.finalize_calls()[0].metadata["authorization"],
        vec!["Bearer tok".to_owned()]
    );
    assert_eq!(
        server.append_stream_metadata()[0]["authorization"],
        vec!["Bearer tok".to_owned()]
    );
    let anonymous = server.client().unwrap();
    anonymous
        .finalize_write_stream(
            &Context::background(),
            FinalizeWriteStreamRequest { name: "s".into() },
        )
        .await
        .unwrap();
    assert!(
        !server.finalize_calls()[1]
            .metadata
            .contains_key("authorization")
    );
}

fn code_named(name: &str) -> Code {
    (0..17)
        .filter_map(Code::from_u32)
        .find(|c| c.to_string() == name)
        .unwrap()
}

fn tonic_code(code: Code) -> tonic::Code {
    tonic::Code::from_i32(code.as_u32() as i32)
}

/// gax's largest pause before retry `n` (0-based): `min(initial * 1.3^n, max)`, grown in
/// integer nanoseconds as Go does.
fn max_pause(initial: Duration, max: Duration, n: usize) -> Duration {
    let mut cur = initial;
    for _ in 0..n {
        cur = Duration::from_nanos((cur.as_nanos() as f64 * 1.3) as u64).min(max);
    }
    cur
}

/// `retry.json`'s unary cases: attempts and final status of scripted statuses, with each
/// pause within gax's jitter bounds (`[1ns, cur]`) under paused time.
#[tokio::test(start_paused = true)]
async fn unary_retries_match_the_probe() {
    let cases = fixture("retry.json");
    let mut seen = 0;
    for case in cases["cases"].as_array().unwrap() {
        let rpc = case["rpc"].as_str().unwrap();
        if rpc == "AppendRows" {
            continue;
        }
        seen += 1;
        let script: Vec<Code> = case["script"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| code_named(s.as_str().unwrap()))
            .collect();
        let server = FakeWriteServer::new();
        let at = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let answer = {
            let at = Arc::clone(&at);
            move |n: usize| {
                at.lock().unwrap().push(Instant::now());
                match script[n - 1] {
                    Code::Ok => Ok(()),
                    code => Err(tonic::Status::new(
                        tonic_code(code),
                        format!("zz scripted {code}"),
                    )),
                }
            }
        };
        let client = server.client().unwrap();
        let ctx = Context::background();
        let (err, initial, max) = if rpc == "CreateWriteStream" {
            server.on_create_write_stream(Arc::new(move |call| {
                let res = answer(call.call_no).map(|()| WriteStream::default());
                Box::pin(async move { res })
            }));
            let res = client
                .create_write_stream(&ctx, CreateWriteStreamRequest::default())
                .await;
            assert_eq!(
                server.create_calls().len(),
                case["attempts"].as_u64().unwrap() as usize
            );
            (res.err(), Duration::from_secs(10), Duration::from_secs(120))
        } else {
            server.on_finalize_write_stream(Arc::new(move |call| {
                let res = answer(call.call_no).map(|()| FinalizeWriteStreamResponse::default());
                Box::pin(async move { res })
            }));
            let res = client
                .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
                .await;
            assert_eq!(
                server.finalize_calls().len(),
                case["attempts"].as_u64().unwrap() as usize
            );
            (
                res.err(),
                Duration::from_millis(100),
                Duration::from_secs(60),
            )
        };
        let want = &case["status"];
        match err {
            None => assert_eq!(*want, Value::Null, "{case}"),
            Some(err) => {
                // Go's CreateWriteStream cases ran through createCommittedWriteStream, which
                // wraps the client's error.
                let text = want["error"].as_str().unwrap();
                let text = text
                    .strip_prefix("failed to create committed write stream: ")
                    .unwrap_or(text);
                assert_eq!(err.to_string(), text, "{case}");
                assert!(
                    matches!(&err, Error::Status { code, .. } if *code == code_named(want["code"].as_str().unwrap())),
                    "{case}"
                );
            }
        }
        let at = at.lock().unwrap();
        for (n, pair) in at.windows(2).enumerate() {
            let pause = pair[1] - pair[0];
            assert!(
                pause >= Duration::from_nanos(1) && pause <= max_pause(initial, max, n),
                "{case}: pause {n} = {pause:?}"
            );
        }
    }
    assert_eq!(seen, 18);
}

/// `retry.json`'s AppendRows cases: grpc-go's `NewStream` does not wait for the server, so a
/// status the server answers at stream start is not seen by the open (no open retry, one
/// stream); `Recv` returns it and a later `Send` returns `EOF`.
#[tokio::test(start_paused = true)]
async fn append_rows_status_at_stream_start_matches_the_probe() {
    let cases = fixture("retry.json");
    let mut seen = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["rpc"] != "AppendRows" {
            continue;
        }
        seen += 1;
        let code = code_named(case["script"][0].as_str().unwrap());
        let server = FakeWriteServer::new();
        server.on_append_rows(Arc::new(move |_| {
            Box::pin(async move {
                Err(tonic::Status::new(
                    tonic_code(code),
                    format!("zz scripted {code}"),
                ))
            })
        }));
        let client = server.client().unwrap();
        let mut stream = client.append_rows(&Context::background()).await.unwrap();
        assert_eq!(case["open_status"], Value::Null);
        let err = stream.recv().await.unwrap_err();
        assert_eq!(err.to_string(), case["status"]["error"].as_str().unwrap());
        let err = stream.send(AppendRowsRequest::default()).await.unwrap_err();
        assert_eq!(err.to_string(), case["send"]["error"].as_str().unwrap());
        assert!(is_eof(&err));
        assert_eq!(
            server.streams_opened(),
            case["attempts"].as_u64().unwrap() as usize
        );
        // The status stays: a second Recv returns it again (grpc-go).
        assert_eq!(
            stream.recv().await.unwrap_err().to_string(),
            case["status"]["error"].as_str().unwrap()
        );
    }
    assert_eq!(seen, 3);
}

/// gax's call timeouts (1200s CreateWriteStream, 600s FinalizeWriteStream) end a call the
/// server never answers: the attempt fails with `DeadlineExceeded`, which is retryable, and
/// gax's sleep then returns the context's error.
#[tokio::test(start_paused = true)]
async fn call_timeouts_end_unanswered_calls() {
    let server = FakeWriteServer::new();
    server.on_create_write_stream(Arc::new(|_| Box::pin(std::future::pending())));
    server.on_finalize_write_stream(Arc::new(|_| Box::pin(std::future::pending())));
    let client = server.client().unwrap();
    let ctx = Context::background();

    let start = Instant::now();
    let err = client
        .create_write_stream(&ctx, CreateWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert_eq!(start.elapsed(), Duration::from_secs(1200));
    assert!(
        matches!(err, Error::Context(ContextError::DeadlineExceeded)),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "context deadline exceeded");
    assert_eq!(server.create_calls().len(), 1);
    assert_eq!(
        server.create_calls()[0].timeout,
        Some(Duration::from_secs(1200))
    );

    let start = Instant::now();
    let err = client
        .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert_eq!(start.elapsed(), Duration::from_secs(600));
    assert!(
        matches!(err, Error::Context(ContextError::DeadlineExceeded)),
        "{err:?}"
    );
    assert_eq!(server.finalize_calls().len(), 1);

    // A caller deadline wins over the call timeout.
    let start = Instant::now();
    let err = client
        .create_write_stream(
            &ctx.with_timeout(Duration::from_secs(30)),
            CreateWriteStreamRequest::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(start.elapsed(), Duration::from_secs(30));
    assert!(
        matches!(err, Error::Context(ContextError::DeadlineExceeded)),
        "{err:?}"
    );
    assert_eq!(
        server.create_calls()[1].timeout,
        Some(Duration::from_secs(30))
    );
}

/// A status that stays retryable is retried until the call timeout ends the call.
#[tokio::test(start_paused = true)]
async fn retryable_statuses_retry_until_the_timeout() {
    for code in [
        tonic::Code::Unavailable,
        tonic::Code::ResourceExhausted,
        tonic::Code::DeadlineExceeded,
    ] {
        let server = FakeWriteServer::new();
        server.on_create_write_stream(Arc::new(move |_| {
            Box::pin(async move { Err(tonic::Status::new(code, "busy")) })
        }));
        let client = server.client().unwrap();
        let start = Instant::now();
        let err = client
            .create_write_stream(&Context::background(), CreateWriteStreamRequest::default())
            .await
            .unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(1200), "{code:?}");
        assert!(
            matches!(err, Error::Context(ContextError::DeadlineExceeded)),
            "{err:?}"
        );
        // 10s, 13s, 16.9s, … up to 120s pauses within 1200s: at least 10 retries happen.
        let calls = server.create_calls();
        assert!(calls.len() > 10, "{code:?}: {} calls", calls.len());
        // Every attempt carries the remaining time of the one call deadline.
        assert_eq!(calls[0].timeout, Some(Duration::from_secs(1200)));
        assert!(calls[1].timeout < calls[0].timeout);
    }
}

#[tokio::test(start_paused = true)]
async fn canceled_and_closed_calls_fail_with_grpc_statuses() {
    let server = FakeWriteServer::new();
    server.on_finalize_write_stream(Arc::new(|_| Box::pin(std::future::pending())));
    let client = server.client().unwrap();

    let ctx = Context::background().with_cancel();
    let canceler = ctx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        canceler.cancel();
    });
    let err = client
        .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "rpc error: code = Canceled desc = context canceled"
    );
    assert_eq!(server.finalize_calls().len(), 1);

    // An ended context fails before the call reaches the server.
    let err = client
        .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "rpc error: code = Canceled desc = context canceled"
    );
    let err = client.append_rows(&ctx).await.err().unwrap();
    assert_eq!(
        err.to_string(),
        "rpc error: code = Canceled desc = context canceled"
    );
    assert_eq!(server.finalize_calls().len(), 1);
    assert_eq!(server.streams_opened(), 0);

    let clone = client.clone();
    client.close().await.unwrap();
    for err in [
        clone
            .create_write_stream(&Context::background(), CreateWriteStreamRequest::default())
            .await
            .unwrap_err(),
        clone
            .append_rows(&Context::background())
            .await
            .err()
            .unwrap(),
    ] {
        assert_eq!(
            err.to_string(),
            "rpc error: code = Canceled desc = grpc: the client connection is closing"
        );
    }
}

#[tokio::test]
async fn invalid_caller_metadata_fails_like_grpc_go() {
    let server = FakeWriteServer::new();
    let client = server.client().unwrap();
    let ctx = Context::background().append_outgoing_metadata(&[("x-bad", "tab\there")]);
    let err = client
        .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "rpc error: code = Internal desc = header key \"x-bad\" contains value with \
         non-printable ASCII characters"
    );
    assert!(server.finalize_calls().is_empty());
}

/// Go `fakeBigQueryWriteServer`: one empty response per request until the client closes.
#[tokio::test]
async fn append_rows_streams_requests_and_responses() {
    let server = FakeWriteServer::new();
    let client = server.client().unwrap();
    let mut stream = client.append_rows(&Context::background()).await.unwrap();
    for name in ["a", "b"] {
        stream
            .send(AppendRowsRequest {
                write_stream: name.into(),
                ..AppendRowsRequest::default()
            })
            .await
            .unwrap();
    }
    server.wait_for_append_requests(2).await;
    assert_eq!(stream.recv().await.unwrap(), AppendRowsResponse::default());
    assert_eq!(stream.recv().await.unwrap(), AppendRowsResponse::default());
    stream.close_send();
    assert!(is_eof(&stream.recv().await.unwrap_err()));
    assert!(is_eof(&stream.recv().await.unwrap_err()));
    // grpc-go `SendMsg` after `CloseSend`.
    assert_eq!(
        stream
            .send(AppendRowsRequest::default())
            .await
            .unwrap_err()
            .to_string(),
        "rpc error: code = Internal desc = send called after the request stream was closed"
    );
    let got: Vec<(usize, String)> = server
        .append_requests()
        .into_iter()
        .map(|r| (r.stream_no, r.request.write_stream))
        .collect();
    assert_eq!(got, vec![(1, "a".to_owned()), (1, "b".to_owned())]);
    assert_eq!(server.streams_opened(), 1);
    // A second stream is a second server stream.
    let mut second = client.append_rows(&Context::background()).await.unwrap();
    second.send(AppendRowsRequest::default()).await.unwrap();
    assert_eq!(second.recv().await.unwrap(), AppendRowsResponse::default());
    assert_eq!(server.streams_opened(), 2);
    assert_eq!(server.append_requests()[2].stream_no, 2);
}

/// The stream ends with its context: `recv` returns the context's status, `send` `EOF`, and
/// the server sees the stream canceled.
#[tokio::test(start_paused = true)]
async fn append_rows_streams_end_with_their_context() {
    for (deadline, want) in [
        (false, "rpc error: code = Canceled desc = context canceled"),
        (
            true,
            "rpc error: code = DeadlineExceeded desc = context deadline exceeded",
        ),
    ] {
        let server = FakeWriteServer::new();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let seen_tx = Arc::new(Mutex::new(Some(seen_tx)));
        server.on_append_rows(Arc::new(move |stream: AppendRowsServerStream| {
            let seen_tx = Arc::clone(&seen_tx);
            Box::pin(async move {
                stream.cancelled().await;
                if let Some(tx) = seen_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                Ok(())
            })
        }));
        let client = server.client().unwrap();
        let ctx = if deadline {
            Context::background().with_timeout(Duration::from_secs(5))
        } else {
            Context::background().with_cancel()
        };
        let mut stream = client.append_rows(&ctx).await.unwrap();
        stream.send(AppendRowsRequest::default()).await.unwrap();
        if !deadline {
            let canceler = ctx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                canceler.cancel();
            });
        }
        let err = stream.recv().await.unwrap_err();
        assert_eq!(err.to_string(), want);
        assert!(is_eof(
            &stream.send(AppendRowsRequest::default()).await.unwrap_err()
        ));
        seen_rx.await.unwrap();
    }
}

/// A server that ends the stream with OK while the client still sends: `recv` → `EOF`,
/// `send` → `EOF`.
#[tokio::test]
async fn append_rows_server_ok_end_is_eof() {
    let server = FakeWriteServer::new();
    server.on_append_rows(Arc::new(|_| Box::pin(async { Ok(()) })));
    let client = server.client().unwrap();
    let mut stream = client.append_rows(&Context::background()).await.unwrap();
    assert!(is_eof(&stream.recv().await.unwrap_err()));
    assert!(is_eof(
        &stream.send(AppendRowsRequest::default()).await.unwrap_err()
    ));
}

#[tokio::test]
async fn new_builds_channels_from_client_options() {
    for endpoint in [
        None,
        Some("http://127.0.0.1:1"),
        Some("https://bigquerystorage.googleapis.com:443"),
        Some("bigquerystorage.googleapis.com:443"),
    ] {
        WriteClient::new(ClientOptions {
            without_authentication: true,
            endpoint: endpoint.map(str::to_owned),
            ..ClientOptions::default()
        })
        .await
        .unwrap();
    }
    let err = WriteClient::new(ClientOptions {
        credentials: Some(bearer_credentials("t")),
        without_authentication: true,
        ..ClientOptions::default()
    })
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "ClientOptions.without_authentication is incompatible with any option that provides \
         credentials"
    );
    assert!(
        WriteClient::new(ClientOptions {
            without_authentication: true,
            endpoint: Some("http://bad host".into()),
            ..ClientOptions::default()
        })
        .await
        .is_err()
    );
}

/// A plaintext endpoint nobody listens on: the open succeeds (grpc-go `NewStream` does not
/// wait either) and the connection failure surfaces at `recv` as `Unavailable` (ledgered:
/// grpc-go fails the open fast and gax retries it); unary calls see `Unavailable` and retry.
#[tokio::test]
async fn unreachable_endpoints_fail_with_unavailable() {
    let client = WriteClient::new(ClientOptions {
        without_authentication: true,
        endpoint: Some("http://127.0.0.1:1".into()),
        ..ClientOptions::default()
    })
    .await
    .unwrap();
    let mut stream = client.append_rows(&Context::background()).await.unwrap();
    let err = stream.recv().await.unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Status {
                code: Code::Unavailable,
                ..
            }
        ),
        "{err:?}"
    );
    let ctx = Context::background().with_timeout(Duration::from_millis(300));
    let err = client
        .finalize_write_stream(&ctx, FinalizeWriteStreamRequest::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Context(ContextError::DeadlineExceeded)),
        "{err:?}"
    );
}
