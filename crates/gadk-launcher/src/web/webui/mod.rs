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

//! Go `cmd/launcher/web/webui`: serves the embedded ADK web UI bundle under `/ui/` (Task 10).
//!
//! The routes are Go's, registered as gorilla lays them out on the base router: the `/ui/`
//! subrouter (the generated `assets/config/runtime-config.json`, then the single-page-app handler
//! for everything else under the prefix), `/` → 302 `/ui/`, and the bare `/ui` → 301 `/ui/`; all of
//! them GET and HEAD only. The single-page-app handler answers `index.html` (its `<base href>`
//! rewritten to the prefix) for the UI root and for extension-less paths that name no file, and
//! hands everything else to a port of Go's `http.StripPrefix` + `http.FileServer` over the bundle
//! with directory listings suppressed, down to `http.ServeContent`'s preconditions and `Range`
//! handling. The embedded files have no modification time, so no `Last-Modified` is sent and
//! `If-Modified-Since`/`If-Unmodified-Since` never apply; no `ETag` is set either.

mod assets;

use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use bytes::Bytes;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::go_mime as mime;
use gadk_platform::{go_path_base, go_path_clean, go_path_join, go_url};
use gadk_server::mux::{Request, Response, handler, html_escape, http_error, not_found, redirect};
use gadk_server::rest::encode_json_response;
use http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
    ETAG, IF_MATCH, IF_NONE_MATCH, IF_RANGE, LAST_MODIFIED, LOCATION, RANGE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use regex::bytes::{NoExpand, Regex};

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::golog::Logger;
use crate::web::{WebRouter, WebSublauncher};

/// Go `indexFile`: the single-page app entry point inside the embedded bundle.
const INDEX_FILE: &str = "index.html";

/// Go `NewLauncher`'s `config.pathPrefix`.
const PATH_PREFIX: &str = "/ui/";

/// Go `baseHrefPattern` (`(?is)<base\s[^>]*?href\s*=\s*"[^"]*"`) in RE2's terms: `\s` is
/// `[\t\n\f\r ]` (no `\v`), the negated classes take any byte (Go reads an invalid UTF-8 byte as
/// one rune), and `(?i)` folds Unicode simply (`s` also matches `ſ`), as RE2 does.
const BASE_HREF_PATTERN: &str =
    r#"(?is)<base[\t\n\f\r ](?-u:[^>])*?href[\t\n\f\r ]*=[\t\n\f\r ]*"(?-u:[^"])*""#;

/// Go `defaultMaxContentRanges`: a `Range` header with more ranges is ignored.
const MAX_CONTENT_RANGES: usize = 200;

/// Go `webui.webUILauncher` (with its `webUIConfig`).
pub struct WebUiLauncher {
    flags: Mutex<FlagSet>,
    /// Go `config.backendAddress` (`-api_server_address`).
    backend_address: Flag<String>,
    /// Go `config.pathPrefix`.
    path_prefix: String,
}

/// Go `webui.NewLauncher()`.
pub fn new() -> WebUiLauncher {
    let mut fs = FlagSet::new("webui");
    let backend_address = fs.string(
        "api_server_address",
        "http://localhost:8080/api",
        "ADK REST API server address as seen from the user browser. Please specify the whole URL, i.e. 'http://localhost:8080/api'.",
    );
    WebUiLauncher {
        flags: Mutex::new(fs),
        backend_address,
        path_prefix: PATH_PREFIX.to_owned(),
    }
}

impl WebUiLauncher {
    /// Sends the flag set's usage and parse errors to `out` instead of stderr (Go
    /// `flag.FlagSet.SetOutput`).
    pub fn with_flag_output(self, out: Arc<Mutex<dyn std::io::Write + Send>>) -> WebUiLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// Go's tests' `&webUILauncher{config: &webUIConfig{pathPrefix: …}}`: the launcher serving
    /// under `path_prefix` (the backend address keeps its default, the tests' address).
    #[cfg(test)]
    pub(crate) fn with_path_prefix(path_prefix: &str) -> WebUiLauncher {
        WebUiLauncher {
            path_prefix: path_prefix.to_owned(),
            ..new()
        }
    }
}

impl WebSublauncher for WebUiLauncher {
    fn keyword(&self) -> String {
        "webui".to_owned()
    }

    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let mut fs = lock(&self.flags);
        match fs.parse(&args) {
            Err(e) => Err(Error::Parse(format!("failed to parse webui flags: {e}"))),
            // Go `err != nil || !w.flags.Parsed()` with a nil error.
            Ok(()) if !fs.parsed() => Err(Error::Parse(
                "failed to parse webui flags: <nil>".to_owned(),
            )),
            Ok(()) => Ok(fs.args()),
        }
    }

    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    fn simple_description(&self) -> String {
        "starts ADK Web UI server which provides UI for interacting with ADK REST API".to_owned()
    }

    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        add_subrouter(
            router,
            &self.path_prefix,
            &self.backend_address.get(),
            &config.log,
        )
    }

    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        printer(&format!(
            "       webui:  you can access API using {web_url}{}",
            self.path_prefix
        ));
    }
}

/// Go `AddSubrouter(router, pathPrefix, backendAddress)`, with Go's `log` going to `log`.
///
/// gorilla tries the `pathPrefix` subrouter (its routes in order), then `/`, then the bare prefix;
/// the flat [`WebRouter`] gets them in that order. The bare-prefix route is Go's exact-equality
/// `MatcherFunc`; here it is a `Path` route, which under `StrictSlash(true)` also matches the
/// prefix itself — but every GET/HEAD there is taken by the subrouter first, and any other method
/// is a 405 either way, so the redirect loop Go guards against cannot form. Go's `log.Fatalf` for
/// an unreadable `index.html` is an error, returned before any route is added.
fn add_subrouter(
    router: &mut WebRouter,
    path_prefix: &str,
    backend_address: &str,
    log: &Logger,
) -> Result<(), Error> {
    let base_href = Regex::new(BASE_HREF_PATTERN)
        .map_err(|e| Error::Config(format!("cannot compile the <base href> pattern: {e}")))?;
    let Some(Entry::File(index_html)) = stat(INDEX_FILE) else {
        return Err(Error::Config(format!(
            "cannot read {INDEX_FILE} from the embedded ADK Web UI files: open {INDEX_FILE}: file does not exist"
        )));
    };
    // Rewrite once at setup: index.html is ~53KB and every deep link serves it.
    let index = Bytes::from(rewrite_base_href(&base_href, index_html, path_prefix, log));
    let ui_methods = [Method::GET, Method::HEAD];

    // The subrouter: generate /assets/config/runtime-config.json at runtime (Go's `Path` inside a
    // `PathPrefix` subrouter appends to the prefix minus its trailing slashes).
    let runtime_config = serde_json::json!({ "backendUrl": backend_address });
    router.handle(
        &ui_methods,
        &format!(
            "{}/assets/config/runtime-config.json",
            path_prefix.trim_end_matches('/')
        ),
        handler(move |_req: Request| {
            let resp = encode_json_response(&runtime_config, StatusCode::OK);
            async move { resp }
        }),
    );
    let spa = Arc::new(Spa {
        prefix: path_prefix.to_owned(),
        index,
    });
    router.handle_prefix(
        &ui_methods,
        path_prefix,
        handler(move |req: Request| {
            let resp = spa.serve(&req);
            async move { resp }
        }),
    );

    // Redirect the user from / to pathPrefix (/ui/).
    let prefix = path_prefix.to_owned();
    router.handle(
        &ui_methods,
        "/",
        handler(move |req: Request| {
            let resp = redirect(&req, &prefix, StatusCode::FOUND);
            async move { resp }
        }),
    );

    // Send the bare prefix (/ui) to the prefix proper (/ui/).
    let bare = path_prefix.strip_suffix('/').unwrap_or(path_prefix);
    if !bare.is_empty() {
        let prefix = path_prefix.to_owned();
        router.handle(
            &ui_methods,
            bare,
            handler(move |req: Request| {
                let resp = redirect(&req, &prefix, StatusCode::MOVED_PERMANENTLY);
                async move { resp }
            }),
        );
    }
    Ok(())
}

/// Go `rewriteBaseHref`: points the `<base href>` of `index.html` at `path_prefix`, or logs that
/// there is none and leaves the document alone.
fn rewrite_base_href(
    pattern: &Regex,
    index_html: &[u8],
    path_prefix: &str,
    log: &Logger,
) -> Vec<u8> {
    if !pattern.is_match(index_html) {
        log.print(&format!(
            "ADK Web UI: no <base href> found in {INDEX_FILE}, deep links may fail to load assets"
        ));
        return index_html.to_vec();
    }
    let replacement = format!("<base href=\"{}\"", html_escape(path_prefix));
    pattern
        .replace_all(index_html, NoExpand(replacement.as_bytes()))
        .into_owned()
}

/// An entry of the embedded bundle (Go `fs.Sub(content, "distr")`).
#[derive(Clone, Copy)]
enum Entry {
    File(&'static [u8]),
    Dir,
}

/// Go `fs.Stat(ui, name)` for a valid `fs` path (`.` is the root): a file of the bundle, or a
/// directory holding one.
fn stat(name: &str) -> Option<Entry> {
    if name == "." {
        return Some(Entry::Dir);
    }
    if let Some((_, bytes)) = assets::ASSETS.iter().find(|(path, _)| *path == name) {
        return Some(Entry::File(bytes));
    }
    let dir = format!("{name}/");
    assets::ASSETS
        .iter()
        .any(|(path, _)| path.starts_with(&dir))
        .then_some(Entry::Dir)
}

/// Go `hasIndex`: whether `dir` contains an `index.html`.
fn has_index(dir: &str) -> bool {
    stat(&go_path_join(&[dir, INDEX_FILE])).is_some()
}

/// Go `http.FS(noListingFS{ui}).Open(name)`: `/` is the root, a leading slash is dropped, and a
/// directory without an `index.html` is missing.
fn open_no_listing(name: &str) -> Option<Entry> {
    let name = if name == "/" {
        "."
    } else {
        name.strip_prefix('/').unwrap_or(name)
    };
    match stat(name)? {
        Entry::Dir if !has_index(name) => None,
        entry => Some(entry),
    }
}

/// The parts of Go's `*url.URL` the file server reads, as Go's server parsed the request target.
struct ReqUrl {
    /// Go `URL.Path` (decoded).
    path: String,
    /// Go `URL.RawPath`: the path as sent when it is not the default escaping of `path`.
    raw_path: String,
    /// Go `URL.RawQuery`.
    raw_query: String,
}

impl ReqUrl {
    /// Go `url.setPath(target path)` and the raw query.
    fn of(req: &Request) -> ReqUrl {
        let raw = req.uri().path();
        let decoded = go_url::unescape_path(raw).unwrap_or_else(|_| raw.as_bytes().to_vec());
        let raw_path = if go_url::escape_path(&decoded) == raw {
            String::new()
        } else {
            raw.to_owned()
        };
        ReqUrl {
            path: String::from_utf8_lossy(&decoded).into_owned(),
            raw_path,
            raw_query: req.uri().query().unwrap_or_default().to_owned(),
        }
    }

    /// Go `URL.EscapedPath()`.
    fn escaped_path(&self) -> String {
        if !self.raw_path.is_empty()
            && go_url::valid_encoded_path(&self.raw_path)
            && go_url::unescape_path(&self.raw_path).is_ok_and(|p| p == self.path.as_bytes())
        {
            return self.raw_path.clone();
        }
        if self.path == "*" {
            return "*".to_owned();
        }
        go_url::escape_path(self.path.as_bytes())
    }
}

/// Go `spaHandler`: the Angular single-page app out of the embedded bundle.
struct Spa {
    prefix: String,
    /// `index.html` with its `<base href>` rewritten.
    index: Bytes,
}

impl Spa {
    /// Go `spaHandler.ServeHTTP`.
    fn serve(&self, req: &Request) -> Response {
        let url = ReqUrl::of(req);
        let name = self.fs_path(&url.path);
        match stat(&name) {
            // noListingFS turns every other directory into a 404, so a directory is only ever
            // served as its index.html, with the rewritten <base href>.
            Some(Entry::Dir) if has_index(&name) => return self.serve_index(req),
            None if path_ext(&name).is_empty() => return self.serve_index(req),
            _ => {}
        }
        self.file_server(req, url)
    }

    /// Go `fsPath`: the request path inside the bundle; cleaning keeps `../` inside it.
    fn fs_path(&self, url_path: &str) -> String {
        let trimmed = url_path.strip_prefix(&self.prefix).unwrap_or(url_path);
        let cleaned = go_path_clean(&format!("/{trimmed}"));
        match &cleaned[1..] {
            "" => ".".to_owned(),
            rel => rel.to_owned(),
        }
    }

    /// Go `serveIndex`: `http.ServeContent(w, r, "index.html", time.Time{}, index)`.
    fn serve_index(&self, req: &Request) -> Response {
        serve_content(req, INDEX_FILE, self.index.clone())
    }

    /// Go `http.StripPrefix(prefix, http.FileServer(http.FS(noListingFS{ui})))`.
    fn file_server(&self, req: &Request, url: ReqUrl) -> Response {
        let p = url.path.strip_prefix(&self.prefix).unwrap_or(&url.path);
        let rp = url
            .raw_path
            .strip_prefix(&self.prefix)
            .unwrap_or(&url.raw_path);
        if !(p.len() < url.path.len() && (url.raw_path.is_empty() || rp.len() < url.raw_path.len()))
        {
            return not_found();
        }
        // fileHandler.ServeHTTP: the path made absolute, then cleaned for the file system.
        let mut stripped = ReqUrl {
            path: p.to_owned(),
            raw_path: rp.to_owned(),
            raw_query: url.raw_query,
        };
        if !stripped.path.starts_with('/') {
            stripped.path.insert(0, '/');
        }
        let name = go_path_clean(&stripped.path);
        serve_file(req, &stripped, &name)
    }
}

/// Go `serveFile(w, r, fs, name, redirect=true)` over the no-listing bundle.
fn serve_file(req: &Request, url: &ReqUrl, name: &str) -> Response {
    // Redirect .../index.html to .../ (relative, as this runs under StripPrefix).
    if url.path.ends_with("/index.html") {
        return local_redirect(url, "./");
    }
    let Some(entry) = open_no_listing(name) else {
        return serve_error(
            &HeaderMap::new(),
            StatusCode::NOT_FOUND,
            "404 page not found",
        );
    };
    // Redirect to the canonical path: / at the end of a directory URL, none after a file.
    match entry {
        Entry::Dir if !url.path.ends_with('/') => {
            return local_redirect(url, &format!("{}/", go_path_base(&url.path)));
        }
        Entry::File(_) if url.path.ends_with('/') => {
            let base = go_path_base(&url.path);
            if base == "/" || base == "." {
                return serve_error(
                    &HeaderMap::new(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "http: attempting to traverse a non-directory",
                );
            }
            return local_redirect(url, &format!("../{base}"));
        }
        _ => {}
    }
    match entry {
        Entry::File(bytes) => serve_content(req, &go_path_base(name), Bytes::from_static(bytes)),
        // A directory reaches here only with an index.html (noListingFS), which is served.
        Entry::Dir => {
            let index = format!("{}/{INDEX_FILE}", name.strip_suffix('/').unwrap_or(name));
            match open_no_listing(&index) {
                Some(Entry::File(bytes)) => {
                    serve_content(req, INDEX_FILE, Bytes::from_static(bytes))
                }
                _ => serve_error(
                    &HeaderMap::new(),
                    StatusCode::NOT_FOUND,
                    "404 page not found",
                ),
            }
        }
    }
}

/// Go `localRedirect`: a 301 to `new_path` as given (relative), with the query; a 404 when the
/// escaped path holds an encoded slash.
fn local_redirect(url: &ReqUrl, new_path: &str) -> Response {
    let escaped = url.escaped_path();
    if escaped.contains("%2f") || escaped.contains("%2F") {
        return not_found();
    }
    let mut location = new_path.to_owned();
    if !url.raw_query.is_empty() {
        location.push('?');
        location.push_str(&url.raw_query);
    }
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::MOVED_PERMANENTLY;
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(LOCATION, v);
    }
    resp
}

/// Go `http.ServeContent(w, r, name, time.Time{}, content)` (`serveContent` with a zero
/// modification time and no `ETag`): preconditions, the content type from `name`'s extension or
/// sniffed, `Range` (one range → 206 with `Content-Range`, several → `multipart/byteranges`),
/// `Accept-Ranges: bytes` and `Content-Length`; HEAD gets the headers only.
fn serve_content(req: &Request, name: &str, content: Bytes) -> Response {
    let range_header = match check_preconditions(req) {
        Ok(range) => range,
        Err(done) => return *done,
    };
    let mut headers = HeaderMap::new();
    let mut ctype = mime::type_by_extension(path_ext(name));
    if ctype.is_empty() {
        ctype = mime::detect_content_type(&content[..content.len().min(mime::SNIFF_LEN)]);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(ctype));

    let size = content.len();
    let mut ranges = match parse_range(&range_header, size) {
        Ok(ranges) => ranges,
        // Some clients add a Range header to every request; an empty file ignores it.
        Err(RangeError::NoOverlap) if size == 0 => Vec::new(),
        Err(RangeError::NoOverlap) => {
            headers.insert(CONTENT_RANGE, ascii_value(&format_no_overlap(size)));
            return serve_error(
                &headers,
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap",
            );
        }
        Err(RangeError::Invalid) => {
            return serve_error(&headers, StatusCode::RANGE_NOT_SATISFIABLE, "invalid range");
        }
    };
    if ranges.iter().map(|r| r.length).sum::<usize>() > size {
        // Larger than the file itself: an attack or a confused client; ignore the ranges.
        ranges.clear();
    }

    let mut status = StatusCode::OK;
    let body = match ranges.as_slice() {
        [] => content,
        [ra] => {
            status = StatusCode::PARTIAL_CONTENT;
            headers.insert(CONTENT_RANGE, ascii_value(&ra.content_range(size)));
            content.slice(ra.start..ra.start + ra.length)
        }
        _ => {
            status = StatusCode::PARTIAL_CONTENT;
            let boundary = random_boundary();
            headers.insert(
                CONTENT_TYPE,
                ascii_value(&format!("multipart/byteranges; boundary={boundary}")),
            );
            Bytes::from(multipart_byteranges(
                &ranges, ctype, &content, size, &boundary,
            ))
        }
    };
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));

    let body = if req.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    };
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

/// A header value of ASCII text built here (digits, hex and fixed words, always valid).
fn ascii_value(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Go `fmt.Sprintf("bytes */%d", size)`.
fn format_no_overlap(size: usize) -> String {
    format!("bytes */{size}")
}

/// Go `condResult`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cond {
    None,
    True,
    False,
}

/// The response's `ETag`: ServeContent's caller never sets one here.
const ETAG_VALUE: &[u8] = b"";

/// Go `checkPreconditions` (RFC 7232 § 6) with a zero modification time: the `Range` header to
/// honor (empty for none), or the 412/304 answer. The date preconditions (`If-Unmodified-Since`,
/// `If-Modified-Since`) never apply to a zero modification time.
fn check_preconditions(req: &Request) -> Result<String, Box<Response>> {
    if check_if_match(req) == Cond::False {
        return Err(Box::new(status_only(StatusCode::PRECONDITION_FAILED)));
    }
    if check_if_none_match(req) == Cond::False {
        let status = if req.method() == Method::GET || req.method() == Method::HEAD {
            // Go `writeNotModified`: none of the representation headers were set yet.
            StatusCode::NOT_MODIFIED
        } else {
            StatusCode::PRECONDITION_FAILED
        };
        return Err(Box::new(status_only(status)));
    }
    let range = header(req, &RANGE);
    if range.is_empty() || check_if_range(req) == Cond::False {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(range).into_owned())
}

/// Go `r.Header.Get(name)`: the first value, or empty.
fn header<'a>(req: &'a Request, name: &HeaderName) -> &'a [u8] {
    req.headers().get(name).map_or(&[], HeaderValue::as_bytes)
}

/// Go `checkIfMatch`.
fn check_if_match(req: &Request) -> Cond {
    let mut im = header(req, &IF_MATCH);
    if im.is_empty() {
        return Cond::None;
    }
    loop {
        im = trim(im);
        match im.first() {
            None => break,
            Some(b',') => {
                im = &im[1..];
                continue;
            }
            Some(b'*') => return Cond::True,
            Some(_) => {}
        }
        let (etag, remain) = scan_etag(im);
        if etag.is_empty() {
            break;
        }
        if etag_strong_match(etag, ETAG_VALUE) {
            return Cond::True;
        }
        im = remain;
    }
    Cond::False
}

/// Go `checkIfNoneMatch`.
fn check_if_none_match(req: &Request) -> Cond {
    let mut buf = header(req, &IF_NONE_MATCH);
    if buf.is_empty() {
        return Cond::None;
    }
    loop {
        buf = trim(buf);
        match buf.first() {
            None => break,
            Some(b',') => {
                buf = &buf[1..];
                continue;
            }
            Some(b'*') => return Cond::False,
            Some(_) => {}
        }
        let (etag, remain) = scan_etag(buf);
        if etag.is_empty() {
            break;
        }
        if etag_weak_match(etag, ETAG_VALUE) {
            return Cond::False;
        }
        buf = remain;
    }
    Cond::True
}

/// Go `checkIfRange` with a zero modification time: an `If-Range` date never matches.
fn check_if_range(req: &Request) -> Cond {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Cond::None;
    }
    let ir = header(req, &IF_RANGE);
    if ir.is_empty() {
        return Cond::None;
    }
    let (etag, _) = scan_etag(ir);
    if !etag.is_empty() && etag_strong_match(etag, ETAG_VALUE) {
        return Cond::True;
    }
    Cond::False
}

/// Go `scanETag`: a syntactically valid `W/"…"` or `"…"` at the start of `s`, and the rest.
fn scan_etag(s: &[u8]) -> (&[u8], &[u8]) {
    let s = trim(s);
    let start = if s.starts_with(b"W/") { 2 } else { 0 };
    if s.len() - start < 2 || s[start] != b'"' {
        return (&[], &[]);
    }
    for i in start + 1..s.len() {
        match s[i] {
            0x21 | 0x23..=0x7E | 0x80..=0xFF => {}
            b'"' => return (&s[..=i], &s[i + 1..]),
            _ => return (&[], &[]),
        }
    }
    (&[], &[])
}

/// Go `etagStrongMatch`.
fn etag_strong_match(a: &[u8], b: &[u8]) -> bool {
    a == b && a.first() == Some(&b'"')
}

/// Go `etagWeakMatch`.
fn etag_weak_match(a: &[u8], b: &[u8]) -> bool {
    a.strip_prefix(b"W/").unwrap_or(a) == b.strip_prefix(b"W/").unwrap_or(b)
}

/// Go `textproto.TrimString`: ASCII space, tab, CR and LF off both ends.
fn trim(s: &[u8]) -> &[u8] {
    let is_space = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');
    let start = s.iter().position(|b| !is_space(b)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !is_space(b))
        .map_or(start, |i| i + 1);
    &s[start..end]
}

/// Go `textproto.TrimString` on a string.
fn trim_str(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
}

/// Go `httpRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HttpRange {
    start: usize,
    length: usize,
}

impl HttpRange {
    /// Go `httpRange.contentRange`.
    fn content_range(self, size: usize) -> String {
        format!(
            "bytes {}-{}/{size}",
            self.start,
            self.start + self.length - 1
        )
    }
}

/// Go `parseRange`'s errors: `errors.New("invalid range")` and `errNoOverlap`.
#[derive(Debug, PartialEq, Eq)]
enum RangeError {
    Invalid,
    NoOverlap,
}

/// Go `parseRange(s, size)` (RFC 7233), with Go's default limit of 200 ranges.
fn parse_range(s: &str, size: usize) -> Result<Vec<HttpRange>, RangeError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let Some(specs) = s.strip_prefix("bytes=") else {
        return Err(RangeError::Invalid);
    };
    if specs.matches(',').count() + 1 > MAX_CONTENT_RANGES {
        return Ok(Vec::new());
    }
    // Sizes fit an i64 (the bundle's files are small); Go's arithmetic is int64.
    let size_i = i64::try_from(size).map_err(|_| RangeError::Invalid)?;
    let mut ranges = Vec::new();
    let mut no_overlap = false;
    for ra in specs.split(',') {
        let ra = trim_str(ra);
        if ra.is_empty() {
            continue;
        }
        let Some((start, end)) = ra.split_once('-') else {
            return Err(RangeError::Invalid);
        };
        let (start, end) = (trim_str(start), trim_str(end));
        let (r_start, r_length) = if start.is_empty() {
            // A suffix length: the last `end` bytes.
            if end.is_empty() || end.starts_with('-') {
                return Err(RangeError::Invalid);
            }
            let i = parse_int(end)
                .filter(|i| *i >= 0)
                .ok_or(RangeError::Invalid)?;
            let i = i.min(size_i);
            (size_i - i, i)
        } else {
            let i = parse_int(start)
                .filter(|i| *i >= 0)
                .ok_or(RangeError::Invalid)?;
            if i >= size_i {
                // Begins after the content: does not overlap.
                no_overlap = true;
                continue;
            }
            if end.is_empty() {
                (i, size_i - i)
            } else {
                let e = parse_int(end)
                    .filter(|e| i <= *e)
                    .ok_or(RangeError::Invalid)?;
                (i, e.min(size_i - 1) - i + 1)
            }
        };
        ranges.push(HttpRange {
            start: usize::try_from(r_start).map_err(|_| RangeError::Invalid)?,
            length: usize::try_from(r_length).map_err(|_| RangeError::Invalid)?,
        });
    }
    if no_overlap && ranges.is_empty() {
        return Err(RangeError::NoOverlap);
    }
    Ok(ranges)
}

/// Go `strconv.ParseInt(s, 10, 64)` (an optional sign, decimal digits, no overflow).
fn parse_int(s: &str) -> Option<i64> {
    s.parse().ok()
}

/// The `multipart/byteranges` body Go's `mime/multipart.Writer` writes for `ranges`: each part's
/// `Content-Range` and `Content-Type` headers (sorted by name), its bytes, and the closing
/// boundary.
fn multipart_byteranges(
    ranges: &[HttpRange],
    ctype: &str,
    content: &[u8],
    size: usize,
    boundary: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, ra) in ranges.iter().enumerate() {
        let delimiter = if i == 0 { "" } else { "\r\n" };
        out.extend_from_slice(
            format!(
                "{delimiter}--{boundary}\r\nContent-Range: {}\r\nContent-Type: {ctype}\r\n\r\n",
                ra.content_range(size)
            )
            .as_bytes(),
        );
        out.extend_from_slice(&content[ra.start..ra.start + ra.length]);
    }
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

/// Go `multipart.randomBoundary`: 30 random bytes in lower-case hex. The bytes come from the
/// standard library's randomly keyed hasher (gadk-launcher has no RNG dependency).
fn random_boundary() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut out = String::with_capacity(64);
    for i in 0u64..4 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(nanos);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(60);
    out
}

/// Go `serveError`: `http.Error(w, text, code)` keeping the headers set so far except
/// `Cache-Control`, `Content-Encoding`, `ETag`, `Last-Modified` (and `http.Error`'s own
/// `Content-Length`/`Content-Type` resets).
fn serve_error(set: &HeaderMap, status: StatusCode, text: &str) -> Response {
    let mut resp = http_error(status, text);
    for (name, value) in set {
        if [
            CACHE_CONTROL,
            CONTENT_ENCODING,
            ETAG,
            LAST_MODIFIED,
            CONTENT_LENGTH,
            CONTENT_TYPE,
        ]
        .contains(name)
        {
            continue;
        }
        resp.headers_mut().insert(name.clone(), value.clone());
    }
    resp
}

/// A response with only a status (Go `w.WriteHeader(code)`; the server adds `Content-Length: 0`
/// where a body is allowed).
fn status_only(status: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = status;
    resp
}

/// Go `path.Ext`: the suffix from the last `.` in the final element, or empty.
fn path_ext(p: &str) -> &str {
    for (i, b) in p.bytes().enumerate().rev() {
        match b {
            b'/' => break,
            b'.' => return &p[i..],
            _ => {}
        }
    }
    ""
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::golog::{Captured, capture};
    use crate::launcher::SubLauncher;
    use crate::test_support::{self as ts, NoAgents, TS, strings};
    /// The bundle's `adk_favicon.svg` length.
    const FAVICON_LEN: usize = 3132;

    fn test_config() -> (Config, Captured) {
        let (log, out) = capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(|| TS.to_owned());
        (config, out)
    }

    /// The UI routes of `launcher` on a `StrictSlash(true)` router.
    fn router_of(launcher: &WebUiLauncher) -> WebRouter {
        let mut router = WebRouter::new(true);
        launcher
            .setup_subrouters(&mut router, &mut test_config().0)
            .unwrap();
        router
    }

    async fn send(
        router: &WebRouter,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut req = http::Request::builder().method(method).uri(target);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = router.serve(req.body(Body::empty()).unwrap()).await;
        let status = resp.status().as_u16();
        let mut headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
            .collect();
        headers.sort();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        v.sort();
        v
    }

    fn favicon() -> &'static [u8] {
        match stat("adk_favicon.svg") {
            Some(Entry::File(b)) => b,
            _ => panic!("the bundle has adk_favicon.svg"),
        }
    }

    #[test]
    fn launcher_texts_and_flags_are_go() {
        let mut l = new();
        assert_eq!(l.keyword(), "webui");
        assert_eq!(
            l.simple_description(),
            "starts ADK Web UI server which provides UI for interacting with ADK REST API"
        );
        assert_eq!(
            l.command_line_syntax(),
            "  -api_server_address string\n    \tADK REST API server address as seen from the user browser. Please specify the whole URL, i.e. 'http://localhost:8080/api'. (default \"http://localhost:8080/api\")\n"
        );
        let mut printed = Vec::new();
        l.user_message("http://localhost:8080", &mut |line: &str| {
            printed.push(line.to_owned());
        });
        assert_eq!(
            printed,
            ["       webui:  you can access API using http://localhost:8080/ui/"]
        );

        assert_eq!(
            l.parse(strings(&[
                "-api_server_address",
                "https://x/api",
                "a2a",
                "-x"
            ]))
            .unwrap(),
            strings(&["a2a", "-x"])
        );
        assert_eq!(l.backend_address.get(), "https://x/api");

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        lock(&l.flags).set_output(out.clone());
        assert_eq!(
            l.parse(strings(&["-nope"])).unwrap_err().to_string(),
            "failed to parse webui flags: flag provided but not defined: -nope"
        );
        let usage = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(
            usage.starts_with("flag provided but not defined: -nope\nUsage of webui:\n"),
            "{usage}"
        );
    }

    #[tokio::test]
    async fn runtime_config_is_go_json_with_go_headers() {
        let mut l = new();
        l.parse(strings(&[
            "-api_server_address=http://h/?a=<b>&c=\"\\\u{2028}\u{1}",
        ]))
        .unwrap();
        let router = router_of(&l);
        let (status, headers, body) =
            send(&router, "GET", "/ui/assets/config/runtime-config.json", &[]).await;
        assert_eq!(status, 200);
        assert_eq!(
            headers,
            h(&[("content-type", "application/json; charset=UTF-8")])
        );
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"backendUrl\":\"http://h/?a=\\u003cb\\u003e\\u0026c=\\\"\\\\\\u2028\\u0001\"}\n"
        );
        // StrictSlash: the other slash form redirects; other methods are 405.
        let (status, headers, _) = send(
            &router,
            "GET",
            "/ui/assets/config/runtime-config.json/",
            &[],
        )
        .await;
        assert_eq!(status, 301);
        assert!(headers.contains(&(
            "location".to_owned(),
            "/ui/assets/config/runtime-config.json".to_owned()
        )));
        assert_eq!(
            send(
                &router,
                "POST",
                "/ui/assets/config/runtime-config.json",
                &[]
            )
            .await
            .0,
            405
        );
    }

    #[test]
    fn rewrite_base_href_is_go() {
        let re = Regex::new(BASE_HREF_PATTERN).unwrap();
        let (log, out) = capture();
        let log = log.with_clock(|| TS.to_owned());
        let rewrite = |doc: &str, prefix: &str| {
            String::from_utf8(rewrite_base_href(&re, doc.as_bytes(), prefix, &log)).unwrap()
        };
        assert_eq!(
            rewrite("<head><base href=\"./\"></head>", "/ui/"),
            "<head><base href=\"/ui/\"></head>"
        );
        // Tolerant of spacing, case and attribute order; every tag is rewritten.
        assert_eq!(
            rewrite(
                "<BASE\n target=\"_top\" HREF = \"x\"/><base  href=\"\">",
                "/ui/"
            ),
            "<base href=\"/ui/\"/><base href=\"/ui/\">"
        );
        // `(?i)` folds `s` to `ſ`; the classes take any byte.
        assert_eq!(
            rewrite("<ba\u{17f}e href=\"./\">", "/p/"),
            "<base href=\"/p/\">"
        );
        assert_eq!(
            rewrite_base_href(&re, b"<base \xff href=\"a\">", "/p/", &log),
            b"<base href=\"/p/\">"
        );
        // The prefix is HTML-escaped.
        assert_eq!(
            rewrite("<base href=\"./\">", "/a&'<>\"/"),
            "<base href=\"/a&amp;&#39;&lt;&gt;&#34;/\">"
        );
        assert_eq!(out.contents(), "");
        let missing = format!(
            "{TS}ADK Web UI: no <base href> found in index.html, deep links may fail to load assets\n"
        );
        assert_eq!(rewrite("<head></head>", "/ui/"), "<head></head>");
        assert_eq!(out.contents(), missing);
        // RE2's `\s` has no vertical tab: no match, logged.
        assert_eq!(
            rewrite("<base\u{b}href=\"./\">", "/p/"),
            "<base\u{b}href=\"./\">"
        );
        assert_eq!(out.contents(), missing.repeat(2));
    }

    #[tokio::test]
    async fn spa_and_file_server_paths_are_go() {
        let router = router_of(&new());
        // index.html under any directory: http.FileServer's relative redirect, query kept.
        assert_eq!(
            send(&router, "GET", "/ui/index.html?x=1", &[]).await,
            (301, h(&[("location", "./?x=1")]), Vec::new())
        );
        assert_eq!(
            send(&router, "GET", "/ui/no/such/index.html", &[]).await.1,
            h(&[("location", "./")])
        );
        // A file named with a trailing slash goes back to the file.
        assert_eq!(
            send(&router, "GET", "/ui/adk_favicon.svg/", &[]).await,
            (301, h(&[("location", "../adk_favicon.svg")]), Vec::new())
        );
        // Directories without an index.html are missing, with or without the slash.
        for target in ["/ui/assets", "/ui/assets/", "/ui/assets/config"] {
            let (status, headers, body) = send(&router, "GET", target, &[]).await;
            assert_eq!(
                (status, headers, body),
                (
                    404,
                    h(&[
                        ("content-type", "text/plain; charset=utf-8"),
                        ("x-content-type-options", "nosniff")
                    ]),
                    b"404 page not found\n".to_vec()
                ),
                "{target}"
            );
        }
        // The bundle's own runtime-config.json is shadowed by the generated one.
        let (_, _, body) = send(&router, "GET", "/ui/assets/config/runtime-config.json", &[]).await;
        assert_eq!(body, b"{\"backendUrl\":\"http://localhost:8080/api\"}\n");
        // Extension-less misses (also behind an encoded slash) get the app.
        for target in ["/ui/eval", "/ui/a%2Fb", "/ui/x.y/z"] {
            let (status, headers, body) = send(&router, "GET", target, &[]).await;
            assert_eq!(status, 200, "{target}");
            assert!(headers.contains(&("content-length".to_owned(), "53361".to_owned())));
            assert!(
                String::from_utf8(body)
                    .unwrap()
                    .contains("<base href=\"/ui/\">")
            );
        }
        // http.StripPrefix compares the raw path: an escaped prefix is not stripped.
        assert_eq!(
            send(&router, "GET", "/u%69/adk_favicon.svg", &[]).await.0,
            404
        );
        assert_eq!(send(&router, "GET", "/u%69/eval", &[]).await.0, 200);
        // The router cleans the path before the UI sees it.
        assert_eq!(
            send(&router, "GET", "/ui/../ui/adk_favicon.svg", &[])
                .await
                .1,
            h(&[("location", "/ui/adk_favicon.svg")])
        );
        // GET and HEAD only; HEAD has the length and no body.
        assert_eq!(
            send(&router, "POST", "/ui/", &[]).await,
            (405, vec![], vec![])
        );
        assert_eq!(send(&router, "DELETE", "/", &[]).await.0, 405);
        assert_eq!(send(&router, "PUT", "/ui", &[]).await.0, 405);
        assert_eq!(
            send(&router, "HEAD", "/ui/adk_favicon.svg", &[]).await,
            (
                200,
                h(&[
                    ("accept-ranges", "bytes"),
                    ("content-length", "3132"),
                    ("content-type", "image/svg+xml")
                ]),
                Vec::new()
            )
        );
        // The redirects of `/` and `/ui` on HEAD: the type, no body.
        assert_eq!(
            send(&router, "HEAD", "/", &[]).await,
            (
                302,
                h(&[
                    ("content-type", "text/html; charset=utf-8"),
                    ("location", "/ui/")
                ]),
                Vec::new()
            )
        );
    }

    #[tokio::test]
    async fn preconditions_are_go_for_a_zero_modtime_without_etag() {
        let router = router_of(&new());
        let get = |headers: &'static [(&'static str, &'static str)]| {
            let router = router.clone();
            async move { send(&router, "GET", "/ui/adk_favicon.svg", headers).await }
        };
        assert_eq!(get(&[("If-None-Match", "*")]).await, (304, vec![], vec![]));
        assert_eq!(get(&[("If-None-Match", " , \"a\", W/\"b\"")]).await.0, 200);
        assert_eq!(get(&[("If-Match", "\"a\"")]).await, (412, vec![], vec![]));
        assert_eq!(get(&[("If-Match", ",")]).await.0, 412);
        assert_eq!(get(&[("If-Match", "\"a\", *")]).await.0, 200);
        assert_eq!(
            get(&[("If-Unmodified-Since", "Sat, 01 Jan 2000 00:00:00 GMT")])
                .await
                .0,
            200
        );
        assert_eq!(
            get(&[("If-Modified-Since", "Sat, 01 Jan 2100 00:00:00 GMT")])
                .await
                .0,
            200
        );
        // The index takes the same path.
        assert_eq!(
            send(&router, "HEAD", "/ui/eval", &[("If-None-Match", "*")]).await,
            (304, vec![], vec![])
        );
    }

    #[tokio::test]
    async fn ranges_are_go() {
        let router = router_of(&new());
        let range = |value: &'static str| {
            let router = router.clone();
            async move { send(&router, "GET", "/ui/adk_favicon.svg", &[("Range", value)]).await }
        };
        let file = favicon();
        assert_eq!(file.len(), FAVICON_LEN);

        let single = |start: usize, end: usize| {
            (
                206,
                h(&[
                    ("accept-ranges", "bytes"),
                    ("content-length", &(end - start + 1).to_string()),
                    (
                        "content-range",
                        &format!("bytes {start}-{end}/{FAVICON_LEN}"),
                    ),
                    ("content-type", "image/svg+xml"),
                ]),
                file[start..=end].to_vec(),
            )
        };
        assert_eq!(range("bytes=0-9").await, single(0, 9));
        assert_eq!(range("bytes=-5").await, single(3127, 3131));
        assert_eq!(range("bytes=3100-").await, single(3100, 3131));
        assert_eq!(range("bytes= 10 - 99999 ,").await, single(10, 3131));
        assert_eq!(range("bytes=+3-+4").await, single(3, 4));
        assert_eq!(range("bytes=-99999").await, single(0, 3131));

        // Not satisfiable: Go's http.Error text, Content-Range only for "failed to overlap".
        let unsatisfiable = |text: &str, content_range: Option<&str>| {
            let mut headers = vec![
                ("content-type", "text/plain; charset=utf-8"),
                ("x-content-type-options", "nosniff"),
            ];
            if let Some(cr) = content_range {
                headers.push(("content-range", cr));
            }
            (416, h(&headers), format!("{text}\n").into_bytes())
        };
        assert_eq!(
            range("bytes=3132-").await,
            unsatisfiable("invalid range: failed to overlap", Some("bytes */3132"))
        );
        for bad in [
            "bytes=x-1",
            "items=0-1",
            "bytes=5",
            "bytes=--5",
            "bytes=-",
            "bytes=9-3",
            "bytes=-1-2",
        ] {
            assert_eq!(
                range(bad).await,
                unsatisfiable("invalid range", None),
                "{bad}"
            );
        }

        // Ignored: more than the file, more than 200 ranges, or any If-Range (no ETag, no
        // modification time).
        let whole = || {
            (
                200,
                h(&[
                    ("accept-ranges", "bytes"),
                    ("content-length", "3132"),
                    ("content-type", "image/svg+xml"),
                ]),
                file.to_vec(),
            )
        };
        assert_eq!(range("bytes=0-,0-").await, whole());
        assert_eq!(range("").await, whole());
        let many: &'static str =
            Box::leak(format!("bytes={}", ["0-0"; 201].join(",")).into_boxed_str());
        assert_eq!(range(many).await, whole());
        assert_eq!(
            send(
                &router,
                "GET",
                "/ui/adk_favicon.svg",
                &[("Range", "bytes=0-9"), ("If-Range", "\"x\"")]
            )
            .await,
            whole()
        );

        // Several ranges: multipart/byteranges as mime/multipart writes it.
        let (status, headers, body) = range("bytes=0-1,10-12").await;
        assert_eq!(status, 206);
        let ctype = headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap();
        let boundary = ctype
            .strip_prefix("multipart/byteranges; boundary=")
            .unwrap()
            .to_owned();
        assert_eq!(boundary.len(), 60);
        assert!(
            boundary
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let want = [
            format!("--{boundary}\r\nContent-Range: bytes 0-1/3132\r\nContent-Type: image/svg+xml\r\n\r\n").into_bytes(),
            file[0..2].to_vec(),
            format!("\r\n--{boundary}\r\nContent-Range: bytes 10-12/3132\r\nContent-Type: image/svg+xml\r\n\r\n").into_bytes(),
            file[10..13].to_vec(),
            format!("\r\n--{boundary}--\r\n").into_bytes(),
        ]
        .concat();
        assert_eq!(body, want);
        assert_eq!(
            headers,
            h(&[
                ("accept-ranges", "bytes"),
                ("content-length", &want.len().to_string()),
                ("content-type", &ctype),
            ])
        );
        assert_ne!(random_boundary(), random_boundary());
    }

    #[test]
    fn go_path_and_etag_helpers() {
        assert_eq!(path_ext("a/b.c/d"), "");
        assert_eq!(path_ext("a/b.tar.gz"), ".gz");
        assert_eq!(path_ext("."), ".");
        assert_eq!(scan_etag(b" W/\"a\" ,x"), (&b"W/\"a\""[..], &b" ,x"[..]));
        assert_eq!(scan_etag(b"\"a b\""), (&b""[..], &b""[..]));
        assert_eq!(scan_etag(b"\"a"), (&b""[..], &b""[..]));
        assert!(etag_weak_match(b"W/\"a\"", b"\"a\""));
        assert!(!etag_strong_match(b"W/\"a\"", b"W/\"a\""));
        let spa = Spa {
            prefix: "/ui/".to_owned(),
            index: Bytes::new(),
        };
        assert_eq!(spa.fs_path("/ui/"), ".");
        assert_eq!(spa.fs_path("/ui/../../etc/passwd"), "etc/passwd");
        assert_eq!(spa.fs_path("/ui/a/./b/"), "a/b");
    }

    #[test]
    fn sha256_matches_the_bundle_manifest() {
        assert_eq!(
            ts::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let manifest =
            std::fs::read_to_string(format!("{}/webui/SHA256SUMS", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let mut checked = 0;
        for line in manifest.lines() {
            let (hash, path) = line.split_once("  ").unwrap();
            let Some(Entry::File(bytes)) = stat(path) else {
                panic!("{path} is in the manifest but not embedded");
            };
            assert_eq!(ts::sha256_hex(bytes), hash, "{path}");
            checked += 1;
        }
        assert_eq!(checked, assets::ASSETS.len());
    }

    /// The `full` server's web UI exchanges (`full_32`–`full_45`) and the web UI's startup line,
    /// byte for byte after the SCENARIO.md normalizations, from a web launcher started with the
    /// fixture's `web` flags and only `webui` active (`tests/wire_replay.rs` replays the whole
    /// server).
    #[tokio::test]
    async fn the_web_ui_matches_go_wire_fixtures() {
        let startup = ts::wire("full_00_startup");
        let port = ts::free_port();
        let args: Vec<String> = ts::args(&startup, port)[1..]
            .iter()
            .take_while(|a| *a != "api")
            .cloned()
            .chain(["webui".to_owned()])
            .collect();
        let mut l = crate::web::new(vec![Box::new(new()) as Box<dyn WebSublauncher>]);
        assert_eq!(
            SubLauncher::parse(&mut l, args).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (config, out) = test_config();
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        ts::wait_listening(port).await;

        // Startup: Go's lines without the other sub-launchers' (trigger warnings, user messages).
        let want = ts::startup_log_of(&startup, &["webui"]);
        assert!(want.contains(
            &"<ts>        webui:  you can access API using http://localhost:<port>/ui/".to_owned()
        ));
        assert_eq!(
            ts::normalize_log(&out.contents(), port),
            want,
            "startup log"
        );

        let names: Vec<String> = ts::fixture_names("wire")
            .into_iter()
            .filter(|n| (32..=45).any(|i| n.starts_with(&format!("full_{i}_webui_"))))
            .collect();
        assert_eq!(names.len(), 14);
        for name in &names {
            ts::assert_exchange(port, name, || out.contents()).await;
        }

        cancel.cancel();
        running.await.unwrap().unwrap();
    }
}
