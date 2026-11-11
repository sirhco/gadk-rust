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
//! clean-path redirect.
//!
//! The REST server's route table ([`rest::Server`](crate::rest::Server)) and the launcher's web
//! router are both built on it.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use axum::body::Body;
use futures::future::BoxFuture;
use gadk_platform::go_url;
use http::header::LOCATION;
use http::{HeaderValue, StatusCode, Uri};

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
    pub fn matches(&self, path: &[u8]) -> Option<crate::rest::PathVars> {
        let mut spans = Vec::new();
        match_from(&self.0, path, 0, true, &mut spans)?;
        Some(self.vars(path, spans))
    }

    /// Prefix match (gorilla `PathPrefix`): the variables and the matched length, or `None`.
    /// mux compiles a prefix template without the `$` anchor, so the match is the leftmost-first
    /// match of `^…([^/]+)…`: a variable takes as much as it can and a literal need not end at a
    /// `/` (`/foo` matches `/foobar`).
    pub fn matches_prefix(&self, path: &[u8]) -> Option<(crate::rest::PathVars, usize)> {
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
}
