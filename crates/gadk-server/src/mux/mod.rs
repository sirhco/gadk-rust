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

//! The parts of gorilla/mux v1.8.1 that gadk's routers reproduce: the handler types, the path
//! template matcher (`Route.Path` and `Route.PathPrefix`), `cleanPath` and `Router.ServeHTTP`'s
//! clean-path redirect; and the `net/http` answers they share: `Error`, `NotFound`, `Redirect`,
//! the server's own `OPTIONS *` and bad-request answers, and its HEAD body stripping.
//!
//! The REST server's route table ([`rest::Server`](crate::rest::Server)) and the launcher's web
//! router ([`router::WebRouter`]) are both built on it, and [`WriteDeadline`] is the launcher
//! server's `http.ResponseController.SetWriteDeadline`.
//!
//! This module mirrors gorilla/mux and `net/http` internals and is plumbing shared by gadk's
//! own crates, not a stable API: it changes with them.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use axum::body::{Body, HttpBody};
use futures::future::BoxFuture;
use gadk_platform::{go_path_clean, go_url};
use http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderValue, Method, StatusCode, Uri};

pub mod router;

/// A request as a handler sees it.
pub type Request = http::Request<axum::body::Body>;
/// A handler's response.
pub type Response = axum::response::Response;
/// A route's handler (Go `http.Handler`).
pub type Handler = Arc<dyn Fn(Request) -> BoxFuture<'static, Response> + Send + Sync>;

/// Wraps an async function as a [`Handler`] (Go `http.HandlerFunc`).
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

/// The path variables of the matched route (Go `mux.Vars`), as a request extension; tests insert
/// it directly (Go `mux.SetURLVars`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathVars(pub BTreeMap<String, String>);

/// The serving connection's write deadline for this request's response (Go
/// `http.NewResponseController(rw).SetWriteDeadline`), as a request extension.
///
/// A server that honours per-request write deadlines (the web launcher's) inserts it; a handler
/// that finds it may move the deadline, overriding the server's write timeout for this response.
/// `None` clears the deadline (Go: the zero time). Without the extension the server's own
/// timeouts apply unchanged.
#[derive(Clone)]
pub struct WriteDeadline(Arc<dyn Fn(Option<tokio::time::Instant>) + Send + Sync>);

impl WriteDeadline {
    /// The hook calling `set` with each new deadline.
    pub fn new(set: impl Fn(Option<tokio::time::Instant>) + Send + Sync + 'static) -> Self {
        WriteDeadline(Arc::new(set))
    }

    /// Go `SetWriteDeadline`: writes after `deadline` fail; `None` means no deadline.
    pub fn set(&self, deadline: Option<tokio::time::Instant>) {
        (self.0)(deadline);
    }
}

impl std::fmt::Debug for WriteDeadline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteDeadline")
    }
}

/// One piece of a gorilla/mux path template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// Bytes matched exactly.
    Literal(Vec<u8>),
    /// `{name}`: one or more bytes other than `/` (mux's default `[^/]+`).
    Var(String),
}

/// A gorilla/mux v1.8.1 path template (`{name}` / `{name:regexp}` subset the code base uses).
///
/// Variables match mux's default `[^/]+`; the `:regexp` of a `{name:regexp}` variable is dropped
/// (no gadk route uses one). [`Template::matches`] is a `Route.Path` template without
/// `StrictSlash` (anchored at both ends), [`Template::matches_prefix`] a `Route.PathPrefix`
/// template (anchored at the start only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<Piece>);

impl Template {
    /// Splits `pattern` into literals and `{name}` variables (mux `braceIndices`: nested braces
    /// belong to the outer variable; a `:pattern` suffix is dropped).
    pub fn parse(pattern: &str) -> Template {
        let mut pieces = Vec::new();
        let mut literal = Vec::new();
        let bytes = pattern.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'{' {
                let mut level = 0;
                let mut j = i;
                while j < bytes.len() {
                    match bytes[j] {
                        b'{' => level += 1,
                        b'}' => {
                            level -= 1;
                            if level == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner = &pattern[i + 1..j.min(bytes.len())];
                let name = inner.split_once(':').map_or(inner, |(n, _)| n);
                if !literal.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Var(name.to_owned()));
                i = j + 1;
            } else {
                literal.push(bytes[i]);
                i += 1;
            }
        }
        if !literal.is_empty() {
            pieces.push(Piece::Literal(literal));
        }
        Template(pieces)
    }

    /// Whole-path match (gorilla `Path`): the path variables, or `None`. `path` is Go's
    /// `URL.Path` (decoded). Variables are greedy and backtrack, so the submatches are those of
    /// Go's leftmost-first regexp `^…([^/]+)…$`.
    pub fn matches(&self, path: &[u8]) -> Option<PathVars> {
        let mut spans = Vec::new();
        match_from(&self.0, path, 0, true, &mut spans)?;
        Some(self.vars(path, spans))
    }

    /// Prefix match (gorilla `PathPrefix`): the variables and the matched length, or `None`.
    /// mux compiles a prefix template without the `$` anchor, so the match is the leftmost-first
    /// match of `^…([^/]+)…`: a variable takes as much as it can and a literal need not end at a
    /// `/` (`/foo` matches `/foobar`).
    pub fn matches_prefix(&self, path: &[u8]) -> Option<(PathVars, usize)> {
        let mut spans = Vec::new();
        let end = match_from(&self.0, path, 0, false, &mut spans)?;
        Some((self.vars(path, spans), end))
    }

    /// The variables named by the template, with the values at `spans` of `path`.
    fn vars(&self, path: &[u8], spans: Vec<(usize, usize)>) -> PathVars {
        let vars = self
            .0
            .iter()
            .filter_map(|p| match p {
                Piece::Var(name) => Some(name.clone()),
                Piece::Literal(_) => None,
            })
            .zip(spans)
            .map(|(name, (start, end))| {
                (
                    name,
                    String::from_utf8_lossy(&path[start..end]).into_owned(),
                )
            })
            .collect();
        PathVars(vars)
    }
}

/// Matches `pieces` against `path[pos..]`, recording each variable's span; the end of the match,
/// which is `path.len()` when `anchored` (the template's `$`).
fn match_from(
    pieces: &[Piece],
    path: &[u8],
    pos: usize,
    anchored: bool,
    spans: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let Some((first, rest)) = pieces.split_first() else {
        return (!anchored || pos == path.len()).then_some(pos);
    };
    match first {
        Piece::Literal(lit) => {
            if !path[pos..].starts_with(lit) {
                return None;
            }
            match_from(rest, path, pos + lit.len(), anchored, spans)
        }
        Piece::Var(_) => {
            let run = path[pos..].iter().take_while(|&&b| b != b'/').count();
            for end in (pos + 1..=pos + run).rev() {
                spans.push((pos, end));
                if let Some(matched) = match_from(rest, path, end, anchored, spans) {
                    return Some(matched);
                }
                spans.pop();
            }
            None
        }
    }
}

/// gorilla `cleanPath`: Go `path.Clean` of a rooted path, keeping a trailing slash.
pub fn clean_path(p: &[u8]) -> Vec<u8> {
    if p.is_empty() {
        return b"/".to_vec();
    }
    let mut rooted = Vec::with_capacity(p.len() + 1);
    if p[0] != b'/' {
        rooted.push(b'/');
    }
    rooted.extend_from_slice(p);
    let mut np = go_path_clean_rooted(&rooted);
    if rooted.ends_with(b"/") && np != b"/" {
        np.push(b'/');
    }
    np
}

/// Go `path.Clean` for a path starting with `/`, on bytes (a decoded path need not be UTF-8, so
/// `gadk_platform::go_path_clean` does not apply).
fn go_path_clean_rooted(path: &[u8]) -> Vec<u8> {
    let n = path.len();
    let mut out = vec![b'/'];
    let (mut r, dotdot) = (1, 1);
    while r < n {
        if path[r] == b'/' || (path[r] == b'.' && (r + 1 == n || path[r + 1] == b'/')) {
            // An empty or `.` element.
            r += 1;
        } else if path[r] == b'.' && path[r + 1] == b'.' && (r + 2 == n || path[r + 2] == b'/') {
            r += 2;
            // Back up to the previous `/` (Go's `out.w--` loop).
            if out.len() > dotdot {
                let mut w = out.len() - 1;
                while w > dotdot && out[w] != b'/' {
                    w -= 1;
                }
                out.truncate(w);
            }
        } else {
            if out.len() != 1 {
                out.push(b'/');
            }
            while r < n && path[r] != b'/' {
                out.push(path[r]);
                r += 1;
            }
        }
    }
    out
}

/// gorilla's clean-path redirect: 301 to `clean`, keeping the query (Router.ServeHTTP). The
/// `Location` is Go `URL.String` of the request URL with the clean path: scheme and authority
/// when the target had them, the escaped path, the query.
pub fn clean_path_redirect(uri: &Uri, clean: &[u8]) -> Response {
    let mut location = String::new();
    if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
        location.push_str(scheme);
        location.push_str("://");
        location.push_str(authority.as_str());
    }
    location.push_str(&go_url::escape_path(clean));
    if let Some(q) = uri.query() {
        location.push('?');
        location.push_str(q);
    }
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::MOVED_PERMANENTLY;
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(LOCATION, v);
    }
    resp
}

/// Go's `r.RequestURI` / `r.URL` of a request: an HTTP/2 request has only its `:path` (hyper adds
/// `:scheme` and `:authority` to the URI), an HTTP/1 request its request target as sent
/// (absolute-form included).
pub fn go_request_uri(req: &Request) -> Uri {
    match req.uri().path_and_query() {
        Some(pq) if req.version() == http::Version::HTTP_2 => Uri::from(pq.clone()),
        _ => req.uri().clone(),
    }
}

/// Go `net/http.Error`: `msg` and a newline as `text/plain; charset=utf-8`, with
/// `X-Content-Type-Options: nosniff`.
pub fn http_error(status: StatusCode, msg: &str) -> Response {
    let mut resp = Response::new(Body::from(format!("{msg}\n")));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

/// Go `net/http.NotFound`: 404 `404 page not found`.
pub fn not_found() -> Response {
    http_error(StatusCode::NOT_FOUND, "404 page not found")
}

/// Go's `net/http` server answer to a request target whose escapes do not decode: `400 Bad
/// Request` as plain text, and the connection closed.
pub fn bad_request() -> Response {
    let mut resp = Response::new(Body::from("400 Bad Request"));
    *resp.status_mut() = StatusCode::BAD_REQUEST;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(CONNECTION, HeaderValue::from_static("close"));
    resp
}

/// Go `globalOptionsHandler`: `OPTIONS *` answers 200 with `Content-Length: 0`.
pub fn options_star() -> Response {
    let mut resp = Response::new(Body::empty());
    resp.headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    resp
}

/// A HEAD response as Go's server writes it: the headers of the GET response, the would-be body
/// length as `Content-Length` when the handler wrote a body of known length (Go's `chunkWriter`
/// sets it for HEAD only when something was written), and no body.
pub fn strip_body(mut resp: Response) -> Response {
    if !resp.headers().contains_key(CONTENT_LENGTH)
        && let Some(len) = resp.body().size_hint().exact()
        && len > 0
    {
        resp.headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    *resp.body_mut() = Body::empty();
    resp
}

/// Go `URL.EscapedPath()` of a request target's path: the path as sent when it is a valid
/// encoding, else the default escaping of its decoded form.
pub fn escaped_path(uri: &Uri) -> String {
    let raw = uri.path();
    if go_url::valid_encoded_path(raw) {
        return raw.to_owned();
    }
    match go_url::unescape_path(raw) {
        Ok(decoded) => go_url::escape_path(&decoded),
        Err(_) => raw.to_owned(),
    }
}

/// Go `net/http.Redirect(w, r, url, code)` on a fresh response: a relative `url` is made absolute
/// against the request's escaped path and cleaned (keeping a trailing slash and the query);
/// `Location` is `url` with non-ASCII bytes hex-escaped; GET and HEAD get
/// `Content-Type: text/html; charset=utf-8`, and GET the short HTML body.
pub fn redirect(req: &Request, url: &str, code: StatusCode) -> Response {
    let mut url = url.to_owned();
    if let Ok(u) = go_url::parse(&url)
        && u.scheme.is_empty()
        && u.host.is_empty()
    {
        let mut old_path = escaped_path(req.uri());
        if old_path.is_empty() {
            old_path = "/".to_owned();
        }
        if !url.starts_with('/') {
            // `path.Split(oldpath)`'s directory.
            let old_dir = &old_path[..old_path.rfind('/').map_or(0, |i| i + 1)];
            url = format!("{old_dir}{url}");
        }
        let (path, query) = match url.find('?') {
            Some(i) => (url[..i].to_owned(), url[i..].to_owned()),
            None => (url.clone(), String::new()),
        };
        let trailing = path.ends_with('/');
        let mut cleaned = go_path_clean(&path);
        if trailing && !cleaned.ends_with('/') {
            cleaned.push('/');
        }
        url = cleaned + &query;
    }
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = code;
    if let Ok(v) = HeaderValue::from_str(&hex_escape_non_ascii(&url)) {
        resp.headers_mut().insert(LOCATION, v);
    }
    let method = req.method();
    if method == Method::GET || method == Method::HEAD {
        resp.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
    }
    if method == Method::GET {
        // `fmt.Fprintln(w, body)` where body already ends in a newline.
        let text = code.canonical_reason().unwrap_or_default();
        *resp.body_mut() = Body::from(format!("<a href=\"{}\">{text}</a>.\n\n", html_escape(&url)));
    }
    resp
}

/// Go `hexEscapeNonASCII`: bytes outside ASCII as `%` and lowercase hex.
pub fn hex_escape_non_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b >= 0x80 {
            out.push_str(&format!("%{b:x}"));
        } else {
            out.push(char::from(b));
        }
    }
    out
}

/// Go `net/http`'s `htmlEscape` (its `htmlReplacer`), which is also `html.EscapeString`:
/// `&`, `<`, `>`, `"` and `'` as entities.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_like_mux() {
        let t = Template::parse("/apps/{app_name}/users/{user_id}");
        let vars = t.matches(b"/apps/a b/users/u").unwrap();
        assert_eq!(vars.0["app_name"], "a b");
        assert_eq!(vars.0["user_id"], "u");
        assert_eq!(t.matches(b"/apps//users/u"), None, "empty segment");
        assert_eq!(t.matches(b"/apps/a/b/users/u"), None, "two segments");
        assert_eq!(t.matches(b"/apps/a/users/u/"), None, "trailing slash");
        // Greedy, leftmost-first like Go's regexp `^/pair/([^/]+)-([^/]+)$`.
        let pair = Template::parse("/pair/{a}-{b}");
        let vars = pair.matches(b"/pair/x-y-z").unwrap();
        assert_eq!((vars.0["a"].as_str(), vars.0["b"].as_str()), ("x-y", "z"));
        assert_eq!(
            Template::parse("/x/{id:[0-9]+}").0[1],
            Piece::Var("id".into())
        );
    }

    #[test]
    fn clean_path_matches_go() {
        for (p, want) in [
            ("", "/"),
            ("a/b", "/a/b"),
            ("/", "/"),
            ("/a/b", "/a/b"),
            ("/a/b/", "/a/b/"),
            ("//a", "/a"),
            ("/a//b", "/a/b"),
            ("/a/./b", "/a/b"),
            ("/a/../b", "/b"),
            ("/../a", "/a"),
            ("/a/b/..", "/a"),
            ("/a/b/../", "/a/"),
            ("/a/..", "/"),
            ("/a/../", "/"),
        ] {
            assert_eq!(
                String::from_utf8(clean_path(p.as_bytes())).unwrap(),
                want,
                "cleanPath({p:?})"
            );
        }
    }

    fn get(method: &str, target: &str) -> Request {
        http::Request::builder()
            .method(method)
            .uri(target)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn redirect_helpers_match_go() {
        assert_eq!(hex_escape_non_ascii("/a\u{e9}"), "/a%c3%a9");
        assert_eq!(html_escape("<a&'\">"), "&lt;a&amp;&#39;&#34;&gt;");
        // Relative to the escaped request path (Go `r.URL.EscapedPath()`), cleaned, query kept.
        let resp = redirect(&get("GET", "/dir/sub"), "../x?y=1", StatusCode::FOUND);
        assert_eq!(resp.headers()[LOCATION], "/x?y=1");
        let resp = redirect(&get("GET", "/a%2Fb/c"), "d/", StatusCode::FOUND);
        assert_eq!(resp.headers()[LOCATION], "/a%2Fb/d/");
        assert_eq!(resp.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
        // Not GET or HEAD: no Content-Type and no body.
        let resp = redirect(&get("POST", "/a"), "/b", StatusCode::MOVED_PERMANENTLY);
        assert!(!resp.headers().contains_key(CONTENT_TYPE));
        assert_eq!(resp.body().size_hint().exact(), Some(0));
        assert_eq!(escaped_path(&"/a%20b".parse().unwrap()), "/a%20b");
        assert_eq!(escaped_path(&"/a%7Eb".parse().unwrap()), "/a%7Eb");
    }

    #[test]
    fn strip_body_sets_a_length_only_for_a_written_body() {
        let resp = strip_body(Response::new(Body::from("abcd")));
        assert_eq!(resp.headers()[CONTENT_LENGTH], "4");
        assert_eq!(resp.body().size_hint().exact(), Some(0));
        let resp = strip_body(Response::new(Body::empty()));
        assert!(!resp.headers().contains_key(CONTENT_LENGTH));
        assert_eq!(options_star().headers()[CONTENT_LENGTH], "0");
        assert_eq!(bad_request().headers()[CONNECTION], "close");
        assert_eq!(not_found().status(), StatusCode::NOT_FOUND);
    }
}
