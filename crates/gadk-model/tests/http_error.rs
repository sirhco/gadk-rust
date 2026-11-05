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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_model::*;
use pretty_assertions::assert_eq;

#[test]
fn api_error_parses_google_error_body() {
    let e = api_error_from_body(
        429,
        br#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota","details":[{"a":1}]}}"#,
    );
    match e {
        ModelError::Api {
            code,
            status,
            message,
            details,
        } => assert_eq!(
            (code, status.as_str(), message.as_str(), details.len()),
            (429, "RESOURCE_EXHAUSTED", "quota", 1)
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn api_error_unparseable_body_uses_prefix() {
    let e = api_error_from_body(500, b"oops");
    match e {
        ModelError::Api { code, message, .. } => {
            assert_eq!((code, message.as_str()), (500, "oops"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn api_error_display_format() {
    let e = api_error_from_body(
        404,
        br#"{"error":{"code":404,"status":"NOT_FOUND","message":"no model"}}"#,
    );
    assert_eq!(e.to_string(), "model API error 404 NOT_FOUND: no model");
}

#[test]
fn body_prefix_cuts_at_char_boundary() {
    let mut body = vec![b'a'; 2047];
    // "é" is two bytes: 2047..2049 straddles the 2048-byte limit.
    body.extend_from_slice("é".as_bytes());
    assert_eq!(body_prefix(&body), "a".repeat(2047));
    assert_eq!(body_prefix(b"short"), "short");
    assert_eq!(body_prefix(&[b'x'; 5000]).len(), 2048);
}

#[tokio::test]
async fn collect_body_concatenates_chunks() {
    let body: BodyStream = Box::pin(futures::stream::iter(vec![
        Ok(bytes::Bytes::from_static(b"ab")),
        Ok(bytes::Bytes::from_static(b"cd")),
    ]));
    assert_eq!(collect_body(body).await.unwrap().as_ref(), b"abcd");
}

#[tokio::test]
async fn collect_body_propagates_error() {
    let body: BodyStream = Box::pin(futures::stream::iter(vec![
        Ok(bytes::Bytes::from_static(b"ab")),
        Err(ModelError::EmptyResponse),
    ]));
    assert!(matches!(
        collect_body(body).await,
        Err(ModelError::EmptyResponse)
    ));
}

#[tokio::test]
async fn reqwest_client_round_trip() {
    use std::io::{BufRead, BufReader, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        let mut stream = stream;
        stream
            .write_all(
                b"HTTP/1.1 201 Created\r\nx-test: yes\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
            )
            .unwrap();
        (request_line, body)
    });

    // Bypass any system proxy so the request reaches the local listener.
    let client = ReqwestClient::new(reqwest::Client::builder().no_proxy().build().unwrap());
    let req = http::Request::post(format!("http://{addr}/v1/x"))
        .body(bytes::Bytes::from_static(b"{\"a\":1}"))
        .unwrap();
    let resp = client.send(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    assert_eq!(resp.headers().get("x-test").unwrap(), "yes");
    let body = collect_body(resp.into_body()).await.unwrap();
    assert_eq!(body.as_ref(), b"hello");

    let (request_line, sent) = server.join().unwrap();
    assert_eq!(request_line, "POST /v1/x HTTP/1.1\r\n");
    assert_eq!(sent, b"{\"a\":1}");
}

#[tokio::test]
async fn reqwest_client_connection_refused_is_transport_error() {
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let req = http::Request::get(format!("http://{addr}/"))
        .body(bytes::Bytes::new())
        .unwrap();
    match ReqwestClient::default().send(req).await {
        Err(ModelError::Transport(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(resp) => panic!("unexpected success: {}", resp.status()),
    }
}
