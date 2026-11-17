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

//! The request plumbing every handler repeats: the path variables, the request's caller and
//! platform, the authorization check, decoding an optional JSON body and answering with an empty
//! JSON response. JSON responses and status errors live in the crate-private `rest::http_error`.

use axum::body::Body;
use gadk_platform::{Platform, go_quote};
use gadk_session::SessionError;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, StatusCode};
use serde::de::DeserializeOwned;

use crate::authn::Caller;
use crate::authz::{Authorizer, write_http_status_for_auth_error};
use crate::rest::routing::{PathVars, Request, Response};

/// The matched route's path variables (empty when the request carries none).
pub(crate) fn path_vars(req: &Request) -> PathVars {
    req.extensions()
        .get::<PathVars>()
        .cloned()
        .unwrap_or_default()
}

/// The request's [`Platform`] extension (inserted by the
/// server from `ServerConfig::platform`), else [`Platform::default`].
pub(crate) fn request_platform(req: &Request) -> Platform {
    req.extensions()
        .get::<Platform>()
        .cloned()
        .unwrap_or_default()
}

/// `Some(403 response)` (see [`write_http_status_for_auth_error`]) when the [`Caller`] among the
/// request's `extensions` may not act as `user_id`.
pub(crate) fn forbid_unless_can_act_as(
    authorizer: &dyn Authorizer,
    extensions: &http::Extensions,
    user_id: &str,
) -> Option<Response> {
    let caller = extensions.get::<Caller>();
    authorizer
        .can_act_as_user(caller, user_id)
        .err()
        .map(|e| write_http_status_for_auth_error(Some(&e)))
}

/// Decodes an optional JSON request body: an empty (or all-whitespace) body and a JSON `null` leave
/// `T` at its zero value, only the first JSON value is read (trailing bytes are not looked at), and
/// a truncated value is the error `unexpected EOF`. A body read failure (e.g. the server's body
/// limit) is reported with its own text.
pub(crate) async fn decode_optional_json_body<T: DeserializeOwned + Default>(
    body: Body,
) -> Result<T, String> {
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| e.to_string())?;
    // Nesting deeper than 127 levels is serde's recursion-limit error.
    let mut values = serde_json::Deserializer::from_slice(&bytes).into_iter::<Option<T>>();
    match values.next() {
        None => Ok(T::default()),
        Some(Ok(v)) => Ok(v.unwrap_or_default()),
        // Syntax and type errors carry serde's text.
        Some(Err(e)) if e.is_eof() => Err("unexpected EOF".to_owned()),
        Some(Err(e)) => Err(e.to_string()),
    }
}

/// The JSON `Content-Type`, the status and no body.
pub(crate) fn encode_nil_json_response(status: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    resp
}

/// The text of a [`gadk_session::SessionService`] error about session `session_id`. The service
/// reports a missing session as the bare [`SessionError::NotFound`]; the text adds the quoted
/// session ID (`: "<id>"` on get, `: "<id>", cannot apply event` on append), byte-identical to an
/// ADK server's error text.
pub(crate) fn session_error_text(err: &SessionError, session_id: &str, append: bool) -> String {
    match err {
        SessionError::NotFound if append => {
            format!("{err}: {}, cannot apply event", go_quote(session_id))
        }
        SessionError::NotFound => format!("{err}: {}", go_quote(session_id)),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    struct Req {
        #[serde(default)]
        a: Option<i64>,
    }

    async fn decode(body: &'static str) -> Result<Req, String> {
        decode_optional_json_body::<Req>(Body::from(body)).await
    }

    /// Go `json.NewDecoder(r).Decode(&v)`: EOF (ignored by the handlers) for an empty body, the
    /// zero value for `null`, the first value only, `unexpected EOF` when truncated.
    #[tokio::test]
    async fn decode_follows_go_json_decoder() {
        assert_eq!(decode("").await.unwrap(), Req::default());
        assert_eq!(decode(" \n").await.unwrap(), Req::default());
        assert_eq!(decode("null").await.unwrap(), Req::default());
        assert_eq!(
            decode(r#"{"a":1} trailing"#).await.unwrap(),
            Req { a: Some(1) }
        );
        assert_eq!(decode(r#"{"a":"#).await.unwrap_err(), "unexpected EOF");
        assert!(decode("x").await.is_err());
    }

    #[tokio::test]
    async fn nil_json_response_has_no_body() {
        let resp = encode_nil_json_response(StatusCode::OK);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()[CONTENT_TYPE],
            "application/json; charset=UTF-8"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[test]
    fn session_errors_carry_go_in_memory_text() {
        assert_eq!(
            session_error_text(&SessionError::NotFound, "missing", false),
            "session not found: \"missing\""
        );
        assert_eq!(
            session_error_text(&SessionError::NotFound, "s1", true),
            "session not found: \"s1\", cannot apply event"
        );
        assert_eq!(
            session_error_text(&SessionError::Backend("boom".into()), "s1", false),
            "boom"
        );
    }
}
