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

//! The OpenAI transport: request headers, the request body, and the reply, sent through
//! [`gadk_model::rest`] with openai-go v3.91.1's retry policy and ADK-Go's call deadline
//! (port of openai-go `RequestConfig.Execute`'s OpenAI specifics and `packages/ssestream`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_model::rest::{
    CallTimeout, DEFAULT_RESPONSE_HEADER_TIMEOUT, RequestHeaders, RestError, RestReply,
    RestTransport, RetryPolicy, header_str,
};
use gadk_model::{BodyStream, ReqwestClient, sse};
use gadk_platform::go_quote;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use crate::config::{ClientConfig, Resolved, UrlError, request_url, resolve};
use crate::error::{OpenAiError, go_json_error};

/// The Chat Completions path, relative to the base URL (openai-go `"chat/completions"`).
pub(crate) const CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// The Responses path, relative to the base URL (openai-go `"responses"`).
pub(crate) const RESPONSES_PATH: &str = "responses";

/// What a successful call returned.
pub(crate) enum Reply {
    /// A non-streaming reply with status < 400: its `Content-Type` (`""` when absent) and its
    /// whole body. Decode it with [`decode_body`].
    Body {
        /// The `Content-Type` header.
        content_type: String,
        /// The body.
        body: Bytes,
    },
    /// A streaming reply with status < 400: the live body, still under the call's deadline.
    /// Decode it with [`event_stream`].
    Stream(BodyStream),
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body { content_type, body } => f
                .debug_struct("Body")
                .field("content_type", content_type)
                .field("len", &body.len())
                .finish(),
            Self::Stream(_) => f.write_str("Stream(..)"),
        }
    }
}

/// One OpenAI client (Go `*openai.Client` as ADK-Go builds it): the shared transport with
/// [`RetryPolicy::OPENAI`], and the configuration resolved against the environment once.
#[derive(Clone)]
pub(crate) struct OpenAiClient {
    /// The HTTP client, the retry policy and the response-header timeout
    /// ([`DEFAULT_RESPONSE_HEADER_TIMEOUT`] when gadk built the client, `None` for a custom
    /// one, as openai-go's `defaultHTTPClient`).
    pub(crate) rest: RestTransport,
    /// The base URL, key and headers.
    pub(crate) resolved: Resolved,
}

impl fmt::Debug for OpenAiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiClient")
            .field("rest", &self.rest)
            .field("resolved", &self.resolved)
            .finish()
    }
}

/// Every attempt's headers are [`OpenAiClient::request_headers`]; its error travels boxed in
/// [`RestError::Headers`] and comes back unchanged.
#[async_trait::async_trait]
impl RequestHeaders for OpenAiClient {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        self.request_headers()
            .map_err(|e| RestError::Headers(Box::new(e)))
    }
}

/// A header value as Go's `net/http` sends it, or the transport error it fails the call with.
fn header_value(name: &str, value: &str) -> Result<HeaderValue, OpenAiError> {
    HeaderValue::from_str(value).map_err(|_| {
        OpenAiError::transport(format!(
            "net/http: invalid header field value for {}",
            go_quote(name)
        ))
    })
}

impl OpenAiClient {
    /// Builds the client for `cfg` (Go `openai.NewClient` with the options `NewModel` derives):
    /// the environment is read now; a custom [`ClientConfig::http_client`] gets no
    /// response-header timeout.
    pub(crate) fn new(cfg: &ClientConfig) -> Self {
        let (http, response_header_timeout): (Arc<dyn gadk_model::HttpClient>, Option<Duration>) =
            match &cfg.http_client {
                Some(h) => (h.clone(), None),
                None => (
                    Arc::new(ReqwestClient::default()),
                    Some(DEFAULT_RESPONSE_HEADER_TIMEOUT),
                ),
            };
        Self {
            rest: RestTransport {
                http,
                policy: RetryPolicy::OPENAI,
                response_header_timeout,
            },
            resolved: resolve(cfg),
        }
    }

    /// The headers of one attempt, in openai-go's order (a later step replaces an earlier
    /// one's values for the same name):
    /// 1. `Accept: application/json`, `Content-Type: application/json`, `User-Agent` =
    ///    [`gadk_platform::tracking_header`] (openai-go sends `OpenAI/Go 3.91.1`);
    /// 2. the environment's headers (`OpenAI-Organization`, `OpenAI-Project`,
    ///    `OPENAI_CUSTOM_HEADERS`);
    /// 3. [`ClientConfig::headers`];
    /// 4. `Authorization: Bearer <key>` when the key is non-empty and no `Authorization` was
    ///    set after the last key assignment (openai-go `ApplySecurity`).
    ///
    /// An invalid environment header name or value fails the attempt as Go's `net/http` does.
    pub(crate) fn request_headers(&self) -> Result<HeaderMap, OpenAiError> {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            USER_AGENT,
            header_value("User-Agent", &gadk_platform::tracking_header())?,
        );
        for (name, value) in &self.resolved.env_headers {
            let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                OpenAiError::transport(format!(
                    "net/http: invalid header field name {}",
                    go_quote(name)
                ))
            })?;
            let mut v = header_value(name, value)?;
            v.set_sensitive(true);
            h.insert(header, v);
        }
        let mut user = self.resolved.headers.clone();
        for v in user.values_mut() {
            v.set_sensitive(true);
        }
        h.extend(user);
        let key = &self.resolved.api_key;
        if !self.resolved.authorization_overridden && !key.is_empty() {
            let mut v = header_value("Authorization", &format!("Bearer {key}"))?;
            v.set_sensitive(true);
            h.insert(AUTHORIZATION, v);
        }
        Ok(h)
    }

    /// The request URL for `path` ([`crate::config::request_url`]); a base without a scheme is
    /// Go's `unsupported protocol scheme ""` transport error.
    pub(crate) fn request_uri(&self, path: &str) -> Result<Uri, OpenAiError> {
        match request_url(&self.resolved.base_url, path) {
            Ok(url) => url
                .as_str()
                .parse::<Uri>()
                .map_err(|e| OpenAiError::transport(e.to_string())),
            Err(UrlError::NoScheme) => {
                Err(OpenAiError::transport("unsupported protocol scheme \"\""))
            }
            Err(UrlError::Parse(e)) => Err(OpenAiError::plain(format!(
                "requestoption: WithBaseURL failed to parse url {e}"
            ))),
        }
    }

    /// Sends `body` to `path` with openai-go's retry policy ([`RestTransport::send`]) and
    /// returns the reply that ends the call.
    ///
    /// - `timeout` (ADK-Go `shared.RequestTimeout`, D7) is one deadline computed now, when the
    ///   call starts running ([`CallTimeout::Call`]): it covers every attempt, the backoff
    ///   sleeps and a streaming body; its expiry is
    ///   [`OpenAiError::is_deadline_exceeded`], never retried.
    /// - A final status ≥ 400 is [`OpenAiError::api`] (openai-go `apierror`); when the body's
    ///   top-level `error` is present but not an object, openai-go's unmarshal error instead
    ///   (`json: cannot unmarshal string into Go value of type apierror.Error`).
    /// - A body that fails to read: the bare read error for a final status ≥ 400, else
    ///   `error reading response body: <err>` (openai-go's texts).
    /// - Errors are not wrapped: the blocking paths add Go's `openai: call failed: ` with
    ///   [`OpenAiError::call_failed`]; the streaming paths report them as they are.
    /// - Dropping the future cancels the call (no task is spawned).
    pub(crate) async fn post(
        &self,
        path: &str,
        body: Bytes,
        stream: bool,
        timeout: Option<Duration>,
    ) -> Result<Reply, OpenAiError> {
        // Go's `context.WithTimeout` saturates: a deadline past `Instant`'s range is none.
        let timeout = timeout
            .and_then(|d| tokio::time::Instant::now().checked_add(d))
            .map_or(CallTimeout::None, CallTimeout::Call);
        let uri = self.request_uri(path)?;
        match self.rest.send(&uri, body, stream, self, timeout).await? {
            RestReply::Read(parts, bytes) => {
                let status = parts.status.as_u16();
                if status >= 400 {
                    return Err(api_error(status, bytes));
                }
                Ok(Reply::Body {
                    content_type: header_str(&parts.headers, "content-type").to_string(),
                    body: bytes,
                })
            }
            RestReply::Streaming(res) => Ok(Reply::Stream(res.into_body())),
        }
    }
}

/// openai-go's handling of a final ≥ 400 body: `gjson.GetBytes(contents, "error").Raw`
/// unmarshalled into `apierror.Error`. Absent, `null`, an object, or a body that is not a
/// JSON object all give the API error; a top-level `error` that is a string, number, boolean
/// or array fails the unmarshal, whose error is returned instead.
fn api_error(status: u16, body: Bytes) -> OpenAiError {
    if let Some(raw) = serde_json::from_slice::<BTreeMap<String, Box<RawValue>>>(&body)
        .ok()
        .and_then(|mut top| top.remove("error"))
    {
        let kind = match raw.get().as_bytes().first() {
            Some(b'"') => Some("string"),
            Some(b'[') => Some("array"),
            Some(b't' | b'f') => Some("bool"),
            Some(b'-' | b'0'..=b'9') => Some("number"),
            _ => None,
        };
        if let Some(kind) = kind {
            return OpenAiError::plain(format!(
                "json: cannot unmarshal {kind} into Go value of type apierror.Error"
            ));
        }
    }
    OpenAiError::api(status, body)
}

/// The request body openai-go sends for `params`: the params as
/// [`gadk_model::to_go_json`] writes them, and for a streaming call `"stream":true` appended
/// after every key (openai-go's `option.WithJSONSet("stream", true)`).
pub(crate) fn encode_body<T: Serialize>(params: &T, stream: bool) -> Result<Bytes, OpenAiError> {
    let mut body = gadk_model::to_go_json(params).map_err(|e| OpenAiError::plain(e.to_string()))?;
    if stream {
        match body.as_slice() {
            b"{}" => body = br#"{"stream":true}"#.to_vec(),
            _ => {
                body.pop();
                body.extend_from_slice(br#","stream":true}"#);
            }
        }
    }
    Ok(Bytes::from(body))
}

/// openai-go's decoding of a non-streaming body into `T` (`RequestConfig.Execute`):
/// - a `Content-Type` whose media type is neither `application/json`-containing nor `+json`
///   fails with openai-go's `expected destination type of 'string' or '[]byte' for responses
///   with content-type '<ct>' that is not 'application/json'`;
/// - otherwise Go's `json.NewDecoder(…).Decode`: the first JSON value is decoded and anything
///   after it is ignored; an empty body is `error parsing response json: EOF`, a truncated one
///   `error parsing response json: unexpected EOF`, any other failure
///   `error parsing response json: <serde_json's text>` (an accepted divergence).
pub(crate) fn decode_body<T: DeserializeOwned>(
    content_type: &str,
    body: &[u8],
) -> Result<T, OpenAiError> {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !(media_type.contains("application/json") || media_type.ends_with("+json")) {
        return Err(OpenAiError::plain(format!(
            "expected destination type of 'string' or '[]byte' for responses with content-type '{content_type}' that is not 'application/json'"
        )));
    }
    let mut de = serde_json::Deserializer::from_slice(body);
    T::deserialize(&mut de).map_err(|e| {
        let text = if !e.is_eof() {
            e.to_string()
        } else if body.iter().all(u8::is_ascii_whitespace) {
            "EOF".to_string()
        } else {
            "unexpected EOF".to_string()
        };
        OpenAiError::plain(format!("error parsing response json: {text}"))
    })
}

/// openai-go `ssestream.Stream[T]` over a streaming body: each SSE event's data decoded into
/// `T`, in order, as `Stream.Next` does:
/// - data starting with `[DONE]` ends the stream;
/// - data whose top-level JSON has an `error` key ends it with openai-go's `StreamError`:
///   `received error while streaming: <error>` (a string's value, else the raw JSON);
/// - data that does not decode ends it with Go `json.Unmarshal`'s error
///   ([`go_json_error`]: `unexpected end of JSON input` for truncated data);
/// - a body error ends it with [`OpenAiError::from_body_error`] (a passed call deadline is
///   [`OpenAiError::is_deadline_exceeded`]).
///
/// Nothing follows the first error. Uses [`gadk_model::sse::sse_events`] (3a's accepted SSE
/// divergences apply).
pub(crate) fn event_stream<T>(body: BodyStream) -> BoxStream<'static, Result<T, OpenAiError>>
where
    T: DeserializeOwned + Send + 'static,
{
    let events = sse::sse_events(body);
    futures::stream::unfold(Some(events), |state| async move {
        let mut events = state?;
        let item = match events.next().await? {
            Err(e) => Err(OpenAiError::from_body_error(e)),
            Ok(event) => {
                let data = event.data;
                if data.starts_with("[DONE]") {
                    return None;
                }
                let error = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(&data)
                    .ok()
                    .and_then(|mut top| top.remove("error"));
                match error {
                    Some(raw) => {
                        // gjson `Result.String()`: a string's value, `""` for null, else
                        // the raw JSON.
                        let text = match raw.get() {
                            "null" => String::new(),
                            other => serde_json::from_str::<String>(other)
                                .unwrap_or_else(|_| other.to_string()),
                        };
                        Err(OpenAiError::plain(format!(
                            "received error while streaming: {text}"
                        )))
                    }
                    None => serde_json::from_str::<T>(&data)
                        .map_err(|e| OpenAiError::plain(go_json_error(&e))),
                }
            }
        };
        let next = if item.is_ok() { Some(events) } else { None };
        Some((item, next))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::HashMap;

    use super::*;
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient};
    use gadk_platform::env::{EnvProvider, MapEnv};
    use pretty_assertions::assert_eq;

    fn env(vars: &[(&str, &str)]) -> Arc<dyn EnvProvider> {
        Arc::new(MapEnv(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        ))
    }

    fn client(cfg: ClientConfig, fake: &Arc<ScriptedClient>) -> OpenAiClient {
        OpenAiClient::new(&ClientConfig {
            http_client: Some(fake.clone()),
            ..cfg
        })
    }

    fn base_cfg(vars: &[(&str, &str)]) -> ClientConfig {
        ClientConfig {
            env: env(vars),
            base_url: "http://127.0.0.1:1/v1".into(),
            ..ClientConfig::default()
        }
    }

    fn headers(c: &OpenAiClient) -> Vec<(String, String)> {
        let h = c.request_headers().unwrap();
        let mut out: Vec<(String, String)> = h
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn default_built_client_has_the_header_timeout_custom_has_none() {
        let built = OpenAiClient::new(&base_cfg(&[]));
        assert_eq!(
            built.rest.response_header_timeout,
            Some(DEFAULT_RESPONSE_HEADER_TIMEOUT)
        );
        assert_eq!(built.rest.policy, RetryPolicy::OPENAI);
        let fake = ScriptedClient::json("{}");
        assert_eq!(
            client(base_cfg(&[]), &fake).rest.response_header_timeout,
            None
        );
    }

    #[test]
    fn headers_follow_openai_go_order() {
        let fake = ScriptedClient::json("{}");
        let tracking = gadk_platform::tracking_header();
        let mut cfg = base_cfg(&[
            ("OPENAI_API_KEY", "env-key"),
            ("OPENAI_ORG_ID", "org-1"),
            ("OPENAI_PROJECT_ID", ""),
            ("OPENAI_CUSTOM_HEADERS", "X-Env: e\nX-Both: env"),
        ]);
        cfg.headers
            .insert("x-both", HeaderValue::from_static("cfg"));
        cfg.headers.append("x-cfg", HeaderValue::from_static("1"));
        assert_eq!(
            headers(&client(cfg, &fake)),
            vec![
                ("accept".into(), "application/json".into()),
                ("authorization".into(), "Bearer env-key".into()),
                ("content-type".into(), "application/json".into()),
                ("openai-organization".into(), "org-1".into()),
                ("openai-project".into(), String::new()),
                ("user-agent".into(), tracking),
                ("x-both".into(), "cfg".into()),
                ("x-cfg".into(), "1".into()),
                ("x-env".into(), "e".into()),
            ]
        );
        let mut cfg = base_cfg(&[]);
        cfg.headers
            .insert("x-cfg", HeaderValue::from_static("secret"));
        let h = client(cfg, &fake).request_headers().unwrap();
        assert!(
            h["x-cfg"].is_sensitive(),
            "config header values are sensitive"
        );
    }

    /// Spec R7: env `OPENAI_CUSTOM_HEADERS: Authorization: X` + config `api_key` → the config
    /// key's bearer; config `headers` `Authorization` → that value; an empty final key → none.
    #[test]
    fn authorization_override_rule() {
        let fake = ScriptedClient::json("{}");
        let auth = |c: &OpenAiClient| {
            c.request_headers()
                .unwrap()
                .get(AUTHORIZATION)
                .map(|v| v.to_str().unwrap().to_string())
        };
        let env_auth = [("OPENAI_CUSTOM_HEADERS", "Authorization: X")];
        assert_eq!(
            auth(&client(base_cfg(&env_auth), &fake)).as_deref(),
            Some("X")
        );
        let mut cfg = base_cfg(&env_auth);
        cfg.api_key = "cfg-key".into();
        assert_eq!(auth(&client(cfg, &fake)).as_deref(), Some("Bearer cfg-key"));

        let mut cfg = base_cfg(&[("OPENAI_API_KEY", "env-key")]);
        cfg.api_key = "cfg-key".into();
        cfg.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer caller"));
        assert_eq!(auth(&client(cfg, &fake)).as_deref(), Some("Bearer caller"));

        assert_eq!(
            auth(&client(base_cfg(&[("OPENAI_API_KEY", "")]), &fake)),
            None
        );
        assert_eq!(auth(&client(base_cfg(&[]), &fake)), None);
    }

    #[test]
    fn invalid_environment_headers_fail_as_net_http_does() {
        let fake = ScriptedClient::json("{}");
        let bad_value = client(
            base_cfg(&[("OPENAI_CUSTOM_HEADERS", "X-A: a\u{7f}b")]),
            &fake,
        );
        assert_eq!(
            bad_value.request_headers().unwrap_err().to_string(),
            "net/http: invalid header field value for \"X-A\""
        );
        let bad_name = client(base_cfg(&[("OPENAI_CUSTOM_HEADERS", "Bad Name: v")]), &fake);
        let err = bad_name.request_headers().unwrap_err();
        assert_eq!(
            err.to_string(),
            "net/http: invalid header field name \"Bad Name\""
        );
        assert!(err.is_transport());
    }

    #[test]
    fn request_uri_errors() {
        let fake = ScriptedClient::json("{}");
        let empty = client(base_cfg(&[("OPENAI_BASE_URL", "")]).with_base(""), &fake);
        let err = empty.request_uri(RESPONSES_PATH).unwrap_err();
        assert_eq!(err.to_string(), "unsupported protocol scheme \"\"");
        assert!(err.is_transport());
        let ok = client(base_cfg(&[]), &fake);
        assert_eq!(
            ok.request_uri(CHAT_COMPLETIONS_PATH).unwrap().to_string(),
            "http://127.0.0.1:1/v1/chat/completions"
        );
    }

    impl ClientConfig {
        fn with_base(mut self, base: &str) -> Self {
            self.base_url = base.into();
            self
        }
    }

    #[tokio::test(start_paused = true)]
    async fn post_sends_headers_body_and_maps_the_reply() {
        let fake = ScriptedClient::always(
            FakeResponse::new(200)
                .header("Content-Type", "application/json; charset=utf-8")
                .chunk(r#"{"id":"r"}"#),
        );
        let mut cfg = base_cfg(&[]);
        cfg.api_key = "k".into();
        let c = client(cfg, &fake);
        let reply = c
            .post(
                RESPONSES_PATH,
                Bytes::from_static(b"{\"model\":\"m\"}"),
                false,
                None,
            )
            .await
            .unwrap();
        let Reply::Body { content_type, body } = reply else {
            panic!("want a body");
        };
        assert_eq!(content_type, "application/json; charset=utf-8");
        assert_eq!(body, Bytes::from_static(br#"{"id":"r"}"#));
        let seen = fake.last_request();
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.url, "http://127.0.0.1:1/v1/responses");
        assert_eq!(seen.header("authorization"), "Bearer k");
        assert_eq!(seen.body_text(), "{\"model\":\"m\"}");
    }

    #[tokio::test(start_paused = true)]
    async fn post_maps_api_errors_after_openai_retries() {
        let fake = ScriptedClient::always(FakeResponse::json_status(
            429,
            r#"{"error":{"message":"slow down"}}"#,
        ));
        let c = client(base_cfg(&[]), &fake);
        let err = c
            .post(
                CHAT_COMPLETIONS_PATH,
                Bytes::from_static(b"{}"),
                false,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(fake.hits(), 3, "openai-go retries a 429 twice");
        assert_eq!(err.status(), Some(429));
        assert_eq!(err.to_string(), "OpenAI API error: 429 Too Many Requests");
        assert_eq!(
            err.call_failed().to_string(),
            "openai: call failed: OpenAI API error: 429 Too Many Requests"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn post_api_error_body_shapes_follow_openai_go() {
        for (body, want, api) in [
            ("", "OpenAI API error: 400 Bad Request", true),
            ("not json", "OpenAI API error: 400 Bad Request", true),
            (r#"{"x":1}"#, "OpenAI API error: 400 Bad Request", true),
            ("[1]", "OpenAI API error: 400 Bad Request", true),
            (
                r#"{"error":{"message":"m"}}"#,
                "OpenAI API error: 400 Bad Request",
                true,
            ),
            (
                r#"{"error":null}"#,
                "OpenAI API error: 400 Bad Request",
                true,
            ),
            (
                r#"{"error":"str"}"#,
                "json: cannot unmarshal string into Go value of type apierror.Error",
                false,
            ),
            (
                r#"{"error":5}"#,
                "json: cannot unmarshal number into Go value of type apierror.Error",
                false,
            ),
        ] {
            let fake = ScriptedClient::always(FakeResponse::json_status(400, body));
            let err = client(base_cfg(&[]), &fake)
                .post(RESPONSES_PATH, Bytes::new(), false, None)
                .await
                .unwrap_err();
            assert_eq!(err.to_string(), want, "{body}");
            assert_eq!(err.status().is_some(), api, "{body}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn post_body_read_failures_use_openai_go_texts() {
        let ok = ScriptedClient::always(FakeResponse::json("").chunk("{").fail("unexpected EOF"));
        let err = client(base_cfg(&[]), &ok)
            .post(RESPONSES_PATH, Bytes::new(), false, None)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "error reading response body: unexpected EOF"
        );
        assert_eq!(ok.hits(), 1, "openai-go never retries a dropped body");

        let bad = ScriptedClient::always(
            FakeResponse::json_status(400, "")
                .chunk("{")
                .fail("unexpected EOF"),
        );
        let err = client(base_cfg(&[]), &bad)
            .post(RESPONSES_PATH, Bytes::new(), false, None)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unexpected EOF");
    }

    #[tokio::test(start_paused = true)]
    async fn post_deadline_is_one_call_deadline() {
        let fake = ScriptedClient::always(FakeResponse::never());
        let c = client(base_cfg(&[]), &fake);
        let err = c
            .post(
                RESPONSES_PATH,
                Bytes::new(),
                false,
                Some(Duration::from_millis(50)),
            )
            .await
            .unwrap_err();
        assert!(err.is_deadline_exceeded());
        assert_eq!(
            err.call_failed().to_string(),
            "openai: call failed: context deadline exceeded"
        );
        assert_eq!(fake.hits(), 1, "a passed call deadline is never retried");
    }

    /// Go's `context.WithTimeout` saturates: a timeout past `Instant`'s range is no deadline,
    /// never a panic.
    #[tokio::test(start_paused = true)]
    async fn post_duration_max_timeout_is_no_deadline() {
        let fake = ScriptedClient::json("{}");
        let reply = client(base_cfg(&[]), &fake)
            .post(RESPONSES_PATH, Bytes::new(), false, Some(Duration::MAX))
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Body { ref body, .. } if body.as_ref() == b"{}"));

        let fake = ScriptedClient::always(FakeResponse::sse("data: a\n\n").hang());
        let reply = client(base_cfg(&[]), &fake)
            .post(RESPONSES_PATH, Bytes::new(), true, Some(Duration::MAX))
            .await
            .unwrap();
        let Reply::Stream(mut body) = reply else {
            panic!("a streaming reply");
        };
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err(),
            "no deadline ends the body"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn post_transport_failures_are_transport_errors() {
        let fake = ScriptedClient::always(FakeResponse::reset("connection refused"));
        let err = client(base_cfg(&[]), &fake)
            .post(RESPONSES_PATH, Bytes::new(), false, None)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "connection refused");
        assert!(err.is_transport());
        assert_eq!(fake.hits(), 3, "connection errors are retried");

        let none = ScriptedClient::json("{}");
        let err = client(base_cfg(&[("OPENAI_BASE_URL", "")]).with_base(""), &none)
            .post(RESPONSES_PATH, Bytes::new(), false, None)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unsupported protocol scheme \"\"");
        assert_eq!(none.hits(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn post_streams_the_live_body() {
        let fake = ScriptedClient::always(FakeResponse::sse("data: {\"a\":1}\n\ndata: [DONE]\n\n"));
        let c = client(base_cfg(&[]), &fake);
        let Reply::Stream(body) = c
            .post(RESPONSES_PATH, Bytes::from_static(b"{}"), true, None)
            .await
            .unwrap()
        else {
            panic!("want a stream");
        };
        let items: Vec<Result<serde_json::Value, OpenAiError>> =
            event_stream::<serde_json::Value>(body).collect().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_ref().unwrap(), &serde_json::json!({"a": 1}));
    }

    #[test]
    fn encode_body_appends_the_stream_flag() {
        #[derive(Serialize)]
        struct P {
            model: &'static str,
            temperature: f64,
            text: &'static str,
        }
        let p = P {
            model: "m",
            temperature: 0.0,
            text: "<a&b>",
        };
        assert_eq!(
            encode_body(&p, false).unwrap(),
            Bytes::from_static(br#"{"model":"m","temperature":0,"text":"\u003ca\u0026b\u003e"}"#)
        );
        assert_eq!(
            encode_body(&p, true).unwrap(),
            Bytes::from_static(
                br#"{"model":"m","temperature":0,"text":"\u003ca\u0026b\u003e","stream":true}"#
            )
        );
        #[derive(Serialize)]
        struct Empty {}
        assert_eq!(
            encode_body(&Empty {}, true).unwrap(),
            Bytes::from_static(br#"{"stream":true}"#)
        );
    }

    /// openai-go v3.91.1's texts for the same bodies (`json.NewDecoder(…).Decode`).
    #[test]
    fn decode_body_follows_openai_go() {
        #[derive(serde::Deserialize, Debug)]
        struct R {
            #[serde(default)]
            id: String,
        }
        let json = "application/json";
        assert_eq!(
            decode_body::<R>(json, b"").unwrap_err().to_string(),
            "error parsing response json: EOF"
        );
        assert_eq!(
            decode_body::<R>(json, b"  ").unwrap_err().to_string(),
            "error parsing response json: EOF"
        );
        assert_eq!(
            decode_body::<R>(json, b"{").unwrap_err().to_string(),
            "error parsing response json: unexpected EOF"
        );
        assert_eq!(
            decode_body::<R>(json, br#"{"id":"x"} trailing"#)
                .unwrap()
                .id,
            "x"
        );
        assert_eq!(
            decode_body::<R>("application/problem+json", br#"{"id":"y"}"#)
                .unwrap()
                .id,
            "y"
        );
        assert_eq!(
            decode_body::<R>("text/plain; charset=utf-8", b"{}")
                .unwrap_err()
                .to_string(),
            "expected destination type of 'string' or '[]byte' for responses with content-type 'text/plain; charset=utf-8' that is not 'application/json'"
        );
    }

    fn body_of(chunks: &[&str]) -> BodyStream {
        let chunks: Vec<Result<Bytes, gadk_model::ModelError>> = chunks
            .iter()
            .map(|c| Ok(Bytes::from(c.to_string())))
            .collect();
        futures::stream::iter(chunks).boxed()
    }

    #[tokio::test]
    async fn event_stream_follows_ssestream() {
        let items: Vec<_> = event_stream::<serde_json::Value>(body_of(&[
            "data: {\"a\":1}\n\n",
            ": comment\n\nevent: x\ndata: {\"b\":\n",
            "data: 2}\n\n",
            "data: [DONE]\n\n",
            "data: {\"never\":true}\n\n",
        ]))
        .collect()
        .await;
        let values: Vec<serde_json::Value> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            values,
            vec![serde_json::json!({"a": 1}), serde_json::json!({"b": 2})]
        );

        let errs: Vec<_> = event_stream::<serde_json::Value>(body_of(&[
            "data: {\"a\":1}\n\n",
            "data: {\"error\":{\"message\":\"boom\"}}\n\n",
            "data: {\"a\":2}\n\n",
        ]))
        .collect()
        .await;
        assert_eq!(errs.len(), 2);
        assert_eq!(
            errs[1].as_ref().unwrap_err().to_string(),
            r#"received error while streaming: {"message":"boom"}"#
        );
        let errs: Vec<_> =
            event_stream::<serde_json::Value>(body_of(&["data: {\"error\":\"plain\"}\n\n"]))
                .collect()
                .await;
        assert_eq!(
            errs[0].as_ref().unwrap_err().to_string(),
            "received error while streaming: plain"
        );
        let errs: Vec<_> =
            event_stream::<serde_json::Value>(body_of(&["data: {\"error\":null}\n\n"]))
                .collect()
                .await;
        assert_eq!(
            errs[0].as_ref().unwrap_err().to_string(),
            "received error while streaming: "
        );
        let array: Vec<_> = event_stream::<serde_json::Value>(body_of(&["data: [1]\n\n"]))
            .collect()
            .await;
        assert_eq!(array[0].as_ref().unwrap(), &serde_json::json!([1]));

        let truncated: Vec<_> =
            event_stream::<serde_json::Value>(body_of(&["data: {\"type\":\"resp\n\n"]))
                .collect()
                .await;
        assert_eq!(
            truncated[0].as_ref().unwrap_err().to_string(),
            "unexpected end of JSON input"
        );

        let failing: BodyStream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: {\"a\":1}\n\n")),
            Err(gadk_model::ModelError::Transport("unexpected EOF".into())),
        ])
        .boxed();
        let items: Vec<_> = event_stream::<serde_json::Value>(failing).collect().await;
        assert_eq!(items.len(), 2);
        let err = items[1].as_ref().unwrap_err();
        assert_eq!(err.to_string(), "unexpected EOF");

        let deadline: BodyStream = futures::stream::iter(vec![Err(gadk_model::ModelError::Other(
            Box::new(RestError::DeadlineExceeded),
        ))])
        .boxed();
        let items: Vec<_> = event_stream::<serde_json::Value>(deadline).collect().await;
        assert!(items[0].as_ref().unwrap_err().is_deadline_exceeded());
    }
}
