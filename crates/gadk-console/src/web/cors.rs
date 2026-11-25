// Copyright 2026 Chris Olson
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

//! The opt-in CORS allow-list of the JSON API (`--api-origin`).

use std::net::{Ipv4Addr, Ipv6Addr};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::header::{
    ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ORIGIN, VARY,
};
use http::{HeaderValue, Method, StatusCode};

use super::AppState;

/// Whether `s` is an exact origin as a browser sends it: lowercase `http://` or `https://`, a
/// lowercase host name, IPv4 address or bracketed IPv6 literal, and an optional port that is
/// not the scheme's default; no path, query, user info or trailing slash.
pub(crate) fn is_origin(s: &str) -> bool {
    let (rest, default_port) = if let Some(rest) = s.strip_prefix("http://") {
        (rest, 80)
    } else if let Some(rest) = s.strip_prefix("https://") {
        (rest, 443)
    } else {
        return false;
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let Some((addr, after)) = v6.split_once(']') else {
            return false;
        };
        if addr.parse::<Ipv6Addr>().is_err() || addr.bytes().any(|b| b.is_ascii_uppercase()) {
            return false;
        }
        match after {
            "" => (addr, None),
            _ => match after.strip_prefix(':') {
                Some(port) => (addr, Some(port)),
                None => return false,
            },
        }
    } else {
        match rest.split_once(':') {
            Some((host, port)) => {
                if !is_host(host) {
                    return false;
                }
                (host, Some(port))
            }
            None if is_host(rest) => (rest, None),
            None => return false,
        }
    };
    !host.is_empty() && port.is_none_or(|p| is_port(p, default_port))
}

/// A lowercase host name (dot-separated labels of `a-z`, `0-9` and inner `-`), or an IPv4
/// address when every label is a number.
fn is_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels
        .iter()
        .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
    {
        return host.parse::<Ipv4Addr>().is_ok();
    }
    labels.iter().all(|l| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// A port number as a browser writes it (1-65535, no leading zero) other than `default`.
fn is_port(port: &str, default: u16) -> bool {
    !port.is_empty()
        && !port.starts_with('0')
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != default)
}

/// axum middleware (from_fn_with_state) on the `/api` router. With a non-empty allow-list every
/// response carries `Vary: Origin`; a request whose `Origin` equals a listed origin also gets
/// `Access-Control-Allow-Origin: <origin>`. `OPTIONS` is answered here with 204, plus
/// `Access-Control-Allow-Methods: GET` and `Access-Control-Max-Age: 600` for a listed origin.
pub(crate) async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let allowed = req
        .headers()
        .get(ORIGIN)
        .and_then(|o| {
            state
                .api_origins
                .iter()
                .find(|a| o.as_bytes() == a.as_bytes())
        })
        .and_then(|a| HeaderValue::from_str(a).ok());
    let mut resp = if req.method() == Method::OPTIONS {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        if allowed.is_some() {
            let headers = resp.headers_mut();
            headers.insert(
                ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET"),
            );
            headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        }
        resp
    } else {
        next.run(req).await
    };
    let headers = resp.headers_mut();
    if !state.api_origins.is_empty() {
        headers.append(VARY, HeaderValue::from_static("Origin"));
    }
    if let Some(origin) = allowed {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    resp
}

#[cfg(test)]
mod tests;
