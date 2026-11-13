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

//! A port of the parts of Go 1.27 `net/url` that gadk's crates reproduce byte for byte:
//! `url.Parse` (its acceptance rules and error texts), `URL.Hostname`, `URL.Port`,
//! `URL.EscapedPath`, `URL.EscapedFragment`, `URL.JoinPath` and `URL.String`, the `User`,
//! `Path`/`RawPath`, `RawQuery` and `Fragment`/`RawFragment` fields, the path encoding
//! (`escape`, `unescape` and `validEncoded` in `encodePath` mode), and `url.PathEscape`,
//! `url.PathUnescape` and `url.QueryUnescape` (`url.QueryEscape` is
//! [`go_query_escape`](crate::go_query_escape)). The WHATWG parser of the `url` crate accepts
//! and rejects different inputs; these checks are Go's.

use crate::go_fmt::go_quote;
use crate::go_path::go_path_join;

/// Go `*url.URL`, as far as gadk reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Url {
    /// The lower-cased scheme.
    pub scheme: String,
    /// Go `Opaque`.
    pub opaque: String,
    /// Go `User`.
    user: Option<Userinfo>,
    /// Go `Host`: the unescaped `host[:port]`.
    pub host: String,
    /// Go `Path` (unescaped bytes).
    pub path: Vec<u8>,
    /// Go `RawPath`.
    raw_path: String,
    /// Go `OmitHost`.
    omit_host: bool,
    /// Go `ForceQuery`.
    force_query: bool,
    /// Go `RawQuery`.
    raw_query: String,
    /// Go `Fragment` (unescaped bytes).
    fragment: Vec<u8>,
    /// Go `RawFragment`.
    raw_fragment: String,
}

/// Go `*url.Userinfo`: the unescaped user name and password.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Userinfo {
    username: Vec<u8>,
    password: Option<Vec<u8>>,
}

impl Userinfo {
    /// Go `Userinfo.String()`.
    fn to_go_string(&self) -> String {
        let mut s = escape(&self.username, Mode::UserPassword);
        if let Some(password) = &self.password {
            s.push(':');
            s.push_str(&escape(password, Mode::UserPassword));
        }
        s
    }
}

/// Go `*url.Error` from `url.Parse`: `parse "<url>": <err>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    url: String,
    err: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse {}: {}", go_quote(&self.url), self.err)
    }
}

impl std::error::Error for ParseError {}

/// Go's `encoding` modes the parser needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Path,
    PathSegment,
    Host,
    Zone,
    UserPassword,
    QueryComponent,
    Fragment,
}

/// Go `url.Parse(raw)`.
pub fn parse(raw: &str) -> Result<Url, ParseError> {
    let (u, frag) = match raw.split_once('#') {
        Some((u, f)) => (u, Some(f)),
        None => (raw, None),
    };
    let mut url = parse_no_fragment(u).map_err(|err| ParseError {
        url: u.to_owned(),
        err,
    })?;
    if let Some(frag) = frag.filter(|f| !f.is_empty()) {
        url.set_fragment(frag).map_err(|err| ParseError {
            url: raw.to_owned(),
            err,
        })?;
    }
    Ok(url)
}

/// Go `parse(rawURL, viaRequest=false)`.
fn parse_no_fragment(raw: &str) -> Result<Url, String> {
    if raw.bytes().any(|b| b < b' ' || b == 0x7f) {
        return Err("net/url: invalid control character in URL".into());
    }
    let mut url = Url::default();
    if raw == "*" {
        url.path = b"*".to_vec();
        return Ok(url);
    }
    let (scheme, rest) = get_scheme(raw)?;
    url.scheme = scheme.to_ascii_lowercase();
    let rest = if rest.ends_with('?') && rest.matches('?').count() == 1 {
        url.force_query = true;
        &rest[..rest.len() - 1]
    } else {
        match rest.split_once('?') {
            Some((r, q)) => {
                url.raw_query = q.to_owned();
                r
            }
            None => rest,
        }
    };
    if !rest.starts_with('/') {
        if !url.scheme.is_empty() {
            // A rootless path is opaque.
            url.opaque = rest.to_owned();
            return Ok(url);
        }
        let segment = rest.split_once('/').map_or(rest, |(s, _)| s);
        if segment.contains(':') {
            return Err("first path segment in URL cannot contain colon".into());
        }
    }
    let mut path = rest;
    if (!url.scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
        let after = &rest[2..];
        let (authority, p) = match after.find('/') {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, ""),
        };
        let (user, host) = parse_authority(&url.scheme, authority)?;
        url.user = user;
        url.host = host;
        path = p;
    } else if !url.scheme.is_empty() && rest.starts_with('/') {
        url.omit_host = true;
    }
    url.set_path(path)?;
    Ok(url)
}

/// Go `getScheme`.
fn get_scheme(raw: &str) -> Result<(&str, &str), String> {
    for (i, c) in raw.bytes().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                if i == 0 {
                    return Ok(("", raw));
                }
            }
            b':' => {
                if i == 0 {
                    return Err("missing protocol scheme".into());
                }
                return Ok((&raw[..i], &raw[i + 1..]));
            }
            _ => return Ok(("", raw)),
        }
    }
    Ok(("", raw))
}

/// Go `parseAuthority`: the userinfo and the unescaped host.
fn parse_authority(scheme: &str, authority: &str) -> Result<(Option<Userinfo>, String), String> {
    let at = authority.rfind('@');
    let host = parse_host(scheme, at.map_or(authority, |i| &authority[i + 1..]))?;
    let Some(i) = at else {
        return Ok((None, host));
    };
    let userinfo = &authority[..i];
    if !valid_userinfo(userinfo) {
        return Err("net/url: invalid userinfo".into());
    }
    let user = match userinfo.split_once(':') {
        None => Userinfo {
            username: unescape(userinfo, Mode::UserPassword)?,
            password: None,
        },
        Some((user, password)) => Userinfo {
            username: unescape(user, Mode::UserPassword)?,
            password: Some(unescape(password, Mode::UserPassword)?),
        },
    };
    Ok((Some(user), host))
}

/// Go `parseHost` (with `urlstrictcolons=1`, Go 1.26+'s default).
fn parse_host(scheme: &str, host: &str) -> Result<String, String> {
    if let Some(open) = host.rfind('[') {
        if open > 0 {
            return Err("invalid IP-literal".into());
        }
        let Some(close) = host.rfind(']') else {
            return Err("missing ']' in host".into());
        };
        let colon_port = &host[close + 1..];
        if !valid_optional_port(colon_port) {
            return Err(format!("invalid port {} after host", go_quote(colon_port)));
        }
        let colon_port = lossy(unescape(colon_port, Mode::Host)?);
        let hostname = &host[1..close];
        let unescaped = match hostname.find("%25") {
            Some(z) => {
                let mut h = lossy(unescape(&hostname[..z], Mode::Host)?);
                h.push_str(&lossy(unescape(&hostname[z..], Mode::Zone)?));
                h
            }
            None => lossy(unescape(hostname, Mode::Host)?),
        };
        // Go `netip.ParseAddr`; only an IPv6 address may sit in brackets.
        let addr = unescaped
            .split_once('%')
            .map_or(unescaped.as_str(), |(a, _)| a);
        if addr.parse::<std::net::Ipv4Addr>().is_ok() {
            return Err("invalid IP-literal".into());
        }
        if addr.parse::<std::net::Ipv6Addr>().is_err() || unescaped.ends_with('%') {
            return Err(format!(
                "invalid host: ParseAddr({}): unable to parse IP",
                go_quote(&unescaped)
            ));
        }
        return Ok(format!("[{unescaped}]{colon_port}"));
    }
    if let Some(first) = host.find(':') {
        let last = host.rfind(':').unwrap_or(first);
        let i = if last != first && !(scheme == "http" || scheme == "https") {
            last
        } else {
            first
        };
        let colon_port = &host[i..];
        if !valid_optional_port(colon_port) {
            return Err(format!("invalid port {} after host", go_quote(colon_port)));
        }
    }
    Ok(lossy(unescape(host, Mode::Host)?))
}

/// Go `validOptionalPort`: empty or `:` followed by digits.
fn valid_optional_port(port: &str) -> bool {
    if port.is_empty() {
        return true;
    }
    match port.strip_prefix(':') {
        Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Go `validUserinfo`.
fn valid_userinfo(s: &str) -> bool {
    s.chars().all(|r| {
        r.is_ascii_alphanumeric()
            || matches!(
                r,
                '-' | '.'
                    | '_'
                    | ':'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | '%'
                    | '@'
            )
    })
}

/// Go `splitHostPort`: the host without a valid port and without IPv6 brackets, and the port.
fn split_host_port(host_port: &str) -> (&str, &str) {
    let mut host = host_port;
    let mut port = "";
    if let Some(colon) = host.rfind(':')
        && valid_optional_port(&host[colon..])
    {
        port = &host[colon + 1..];
        host = &host[..colon];
    }
    if host.len() >= 2 && host.starts_with('[') && host.ends_with(']') {
        host = &host[1..host.len() - 1];
    }
    (host, port)
}

impl Url {
    /// Go `URL.Hostname()`.
    pub fn hostname(&self) -> &str {
        split_host_port(&self.host).0
    }

    /// Go `URL.Port()`.
    pub fn port(&self) -> &str {
        split_host_port(&self.host).1
    }

    /// Go `u.User != nil`.
    pub fn has_user(&self) -> bool {
        self.user.is_some()
    }

    /// Go `RawQuery`.
    pub fn raw_query(&self) -> &str {
        &self.raw_query
    }

    /// Go `u.RawQuery = q`.
    pub fn set_raw_query(&mut self, q: String) {
        self.raw_query = q;
    }

    /// Go `u.Path, u.RawPath = path, raw_path` (field assignment, no validation).
    pub fn set_path_fields(&mut self, path: Vec<u8>, raw_path: String) {
        self.path = path;
        self.raw_path = raw_path;
    }

    /// Go `Fragment` (unescaped bytes).
    pub fn fragment(&self) -> &[u8] {
        &self.fragment
    }

    /// Go `u.Fragment, u.RawFragment = fragment, raw_fragment` (field assignment, no
    /// validation).
    pub fn set_fragment_fields(&mut self, fragment: Vec<u8>, raw_fragment: String) {
        self.fragment = fragment;
        self.raw_fragment = raw_fragment;
    }

    /// Go `URL.setFragment`.
    fn set_fragment(&mut self, f: &str) -> Result<(), String> {
        let frag = unescape(f, Mode::Fragment)?;
        self.raw_fragment = if escape(&frag, Mode::Fragment) == f {
            String::new()
        } else {
            f.to_owned()
        };
        self.fragment = frag;
        Ok(())
    }

    /// Go `URL.EscapedFragment()`.
    pub fn escaped_fragment(&self) -> String {
        if !self.raw_fragment.is_empty()
            && valid_encoded(&self.raw_fragment, Mode::Fragment)
            && unescape(&self.raw_fragment, Mode::Fragment).is_ok_and(|f| f == self.fragment)
        {
            return self.raw_fragment.clone();
        }
        escape(&self.fragment, Mode::Fragment)
    }

    /// Go `URL.setPath`.
    fn set_path(&mut self, p: &str) -> Result<(), String> {
        let path = unescape(p, Mode::Path)?;
        self.raw_path = if escape_path(&path) == p {
            String::new()
        } else {
            p.to_owned()
        };
        self.path = path;
        Ok(())
    }

    /// Go `URL.EscapedPath()`.
    pub fn escaped_path(&self) -> String {
        if !self.raw_path.is_empty()
            && valid_encoded_path(&self.raw_path)
            && unescape(&self.raw_path, Mode::Path).is_ok_and(|p| p == self.path)
        {
            return self.raw_path.clone();
        }
        if self.path == b"*" {
            return "*".into();
        }
        escape_path(&self.path)
    }

    /// Go `URL.joinPath(elem...)`.
    pub fn join_path(&self, elems: &[&str]) -> Result<Url, String> {
        let mut all: Vec<String> = Vec::with_capacity(elems.len() + 1);
        all.push(self.escaped_path());
        all.extend(elems.iter().map(|e| (*e).to_owned()));
        let mut p = if all[0].starts_with('/') {
            go_path_join(&all)
        } else {
            all[0] = format!("/{}", all[0]);
            go_path_join(&all)[1..].to_owned()
        };
        if all.last().is_some_and(|e| e.ends_with('/')) && !p.ends_with('/') {
            p.push('/');
        }
        let mut url = self.clone();
        url.set_path(&p)?;
        Ok(url)
    }

    /// Go `URL.String()`.
    pub fn to_go_string(&self) -> String {
        let mut buf = String::new();
        if !self.opaque.is_empty() {
            buf.push_str(&self.scheme);
            buf.push(':');
            buf.push_str(&self.opaque);
        } else {
            if !self.scheme.is_empty() {
                buf.push_str(&self.scheme);
                buf.push(':');
            }
            let has_user = self.user.is_some();
            if (!self.scheme.is_empty() || !self.host.is_empty() || has_user)
                && !(self.omit_host && self.host.is_empty() && !has_user)
            {
                if !self.host.is_empty() || !self.path.is_empty() || has_user {
                    buf.push_str("//");
                }
                if let Some(user) = &self.user {
                    buf.push_str(&user.to_go_string());
                    buf.push('@');
                }
                buf.push_str(&escape(self.host.as_bytes(), Mode::Host));
            }
            let path = self.escaped_path();
            if !path.is_empty() && !path.starts_with('/') && !self.host.is_empty() {
                buf.push('/');
            }
            if buf.is_empty() {
                let segment = path.split_once('/').map_or(path.as_str(), |(s, _)| s);
                if segment.contains(':') {
                    buf.push_str("./");
                }
            }
            buf.push_str(&path);
        }
        if self.force_query || !self.raw_query.is_empty() {
            buf.push('?');
            buf.push_str(&self.raw_query);
        }
        if !self.fragment.is_empty() {
            buf.push('#');
            buf.push_str(&self.escaped_fragment());
        }
        buf
    }
}

/// Go `shouldEscape(c, mode)` for the modes above.
fn should_escape(c: u8, mode: Mode) -> bool {
    if c.is_ascii_alphanumeric() {
        return false;
    }
    if matches!(mode, Mode::Host | Mode::Zone)
        && matches!(
            c,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'['
                | b']'
                | b'<'
                | b'>'
                | b'"'
        )
    {
        return false;
    }
    match c {
        b'-' | b'_' | b'.' | b'~' => return false,
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => match mode {
            Mode::Path => return c == b'?',
            Mode::PathSegment => return matches!(c, b'/' | b';' | b',' | b'?'),
            Mode::UserPassword => return matches!(c, b'@' | b'/' | b'?' | b':'),
            Mode::QueryComponent => return true,
            Mode::Fragment => return false,
            Mode::Host | Mode::Zone => {}
        },
        _ => {}
    }
    if mode == Mode::Fragment && matches!(c, b'!' | b'(' | b')' | b'*') {
        return false;
    }
    true
}

/// Go `escape(s, encodePath)`, upper-case hex.
pub fn escape_path(s: &[u8]) -> String {
    escape(s, Mode::Path)
}

/// Go `url.PathEscape(s)` (`escape(s, encodePathSegment)`): `/`, `;`, `,` and `?` are escaped
/// too.
pub fn path_escape(s: &[u8]) -> String {
    escape(s, Mode::PathSegment)
}

/// Go `escape(s, mode)` for the modes without a space rule (every mode but
/// `encodeQueryComponent`, which [`go_query_escape`](crate::go_query_escape) ports).
fn escape(s: &[u8], mode: Mode) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    // Every byte from 0x80 up is escaped in every mode, so the result is ASCII.
    let mut out = String::with_capacity(s.len());
    for &c in s {
        if should_escape(c, mode) {
            out.push('%');
            out.push(char::from(HEX[usize::from(c >> 4)]));
            out.push(char::from(HEX[usize::from(c & 15)]));
        } else {
            out.push(char::from(c));
        }
    }
    out
}

/// Go `validEncoded(s, encodePath)`: `s` holds no byte that escaping would change.
pub fn valid_encoded_path(s: &str) -> bool {
    valid_encoded(s, Mode::Path)
}

/// Go `validEncoded(s, mode)`.
fn valid_encoded(s: &str, mode: Mode) -> bool {
    s.bytes().all(|c| {
        matches!(
            c,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'['
                | b']'
                | b'%'
        ) || !should_escape(c, mode)
    })
}

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Bytes that are not UTF-8 after decoding become U+FFFD.
fn lossy(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).into_owned()
}

/// Go `EscapeError`: `invalid URL escape "<up to 3 bytes>"`.
fn escape_error(s: &[u8]) -> String {
    let end = s.len().min(3);
    format!(
        "invalid URL escape {}",
        go_quote(&String::from_utf8_lossy(&s[..end]))
    )
}

/// Go `unescape(s, encodePath)` (how `URL.Path` is decoded), with Go's `EscapeError` text.
pub fn unescape_path(s: &str) -> Result<Vec<u8>, String> {
    unescape(s, Mode::Path)
}

/// Go `url.PathUnescape(s)` (`unescape(s, encodePathSegment)`), with Go's `EscapeError` text.
pub fn path_unescape(s: &str) -> Result<Vec<u8>, String> {
    unescape(s, Mode::PathSegment)
}

/// Go `url.QueryUnescape(s)` (`unescape(s, encodeQueryComponent)`: `+` is a space), with Go's
/// `EscapeError` text.
pub fn query_unescape(s: &str) -> Result<Vec<u8>, String> {
    unescape(s, Mode::QueryComponent)
}

/// Go `unescape(s, mode)`, with Go's `EscapeError` / `InvalidHostError` texts.
fn unescape(s: &str, mode: Mode) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let (Some(hi), Some(lo)) = (
                    b.get(i + 1).copied().and_then(unhex),
                    b.get(i + 2).copied().and_then(unhex),
                ) else {
                    return Err(escape_error(&b[i..]));
                };
                let is_25 = &b[i..i + 3] == b"%25";
                if mode == Mode::Host && hi < 8 && !is_25 {
                    return Err(escape_error(&b[i..i + 3]));
                }
                let v = (hi << 4) | lo;
                if mode == Mode::Zone && !is_25 && v != b' ' && should_escape(v, Mode::Host) {
                    return Err(escape_error(&b[i..i + 3]));
                }
                out.push(v);
                i += 3;
            }
            b'+' if mode == Mode::QueryComponent => {
                out.push(b' ');
                i += 1;
            }
            c => {
                if matches!(mode, Mode::Host | Mode::Zone) && c < 0x80 && should_escape(c, mode) {
                    return Err(format!(
                        "invalid character {} in host name",
                        go_quote(&char::from(c).to_string())
                    ));
                }
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Go 1.27 `url.Parse` errors on the inputs the card checks see.
    #[test]
    fn parse_errors_match_go() {
        for (raw, want) in [
            (
                "http://x\n",
                "parse \"http://x\\n\": net/url: invalid control character in URL",
            ),
            (
                "://nonsense",
                "parse \"://nonsense\": missing protocol scheme",
            ),
            (
                "http://host:port",
                "parse \"http://host:port\": invalid port \":port\" after host",
            ),
            (
                "http://a:b:80",
                "parse \"http://a:b:80\": invalid port \":b:80\" after host",
            ),
            ("http://[::1", "parse \"http://[::1\": missing ']' in host"),
            (
                "http://x[::1]",
                "parse \"http://x[::1]\": invalid IP-literal",
            ),
            (
                "http://[1.2.3.4]",
                "parse \"http://[1.2.3.4]\": invalid IP-literal",
            ),
            (
                "http://a b",
                "parse \"http://a b\": invalid character \" \" in host name",
            ),
            (
                "http://x/%zz",
                "parse \"http://x/%zz\": invalid URL escape \"%zz\"",
            ),
            (
                "http://x/#%zz",
                "parse \"http://x/#%zz\": invalid URL escape \"%zz\"",
            ),
            (
                "http://bad user@x",
                "parse \"http://bad user@x\": net/url: invalid userinfo",
            ),
            (
                "1a:b/c",
                "parse \"1a:b/c\": first path segment in URL cannot contain colon",
            ),
        ] {
            assert_eq!(
                parse(raw).unwrap_err().to_string(),
                want,
                "url.Parse({raw:?})"
            );
        }
        assert_eq!(parse("a/b:c").map(|u| u.path), Ok(b"a/b:c".to_vec()));
    }

    #[test]
    fn parse_reads_scheme_host_and_port_as_go_does() {
        for (raw, scheme, hostname, port) in [
            ("HTTP://LocalHost:80/", "http", "LocalHost", "80"),
            (
                "https://ui.example.com/app?x=1",
                "https",
                "ui.example.com",
                "",
            ),
            ("http://user:pw@host:1", "http", "host", "1"),
            ("http://[::1]:8080", "http", "::1", "8080"),
            ("mailto:x@y", "mailto", "", ""),
            ("/rpc", "", "", ""),
            ("", "", "", ""),
            ("localhost:50051", "localhost", "", ""),
        ] {
            let u = parse(raw).unwrap();
            assert_eq!(
                (u.scheme.as_str(), u.hostname(), u.port()),
                (scheme, hostname, port),
                "url.Parse({raw:?})"
            );
        }
    }

    /// Hosts of Go 1.27 `url.Parse`, and the inputs it refuses (gadk-server's origin guard).
    #[test]
    fn parse_hosts_match_go() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            ("http://localhost:8080", Some(("http", "localhost:8080"))),
            ("HTTP://LocalHost:80/", Some(("http", "LocalHost:80"))),
            (
                "https://ui.example.com/app?x=1",
                Some(("https", "ui.example.com")),
            ),
            ("http://user:pw@host:1", Some(("http", "host:1"))),
            ("http://[::1]:8080", Some(("http", "[::1]:8080"))),
            ("http://[fe80::1%25en0]", Some(("http", "[fe80::1%en0]"))),
            ("http://ex%C3%A4mple.com", Some(("http", "exämple.com"))),
            ("mailto:x@y", Some(("mailto", ""))),
            ("http:///path", Some(("http", ""))),
            ("://nonsense", None),
            ("http://a:b:80", None),
            ("http://[1.2.3.4]", None),
            ("http://x[::1]", None),
            ("http://host:port", None),
            ("http://%41.com", None),
            ("http://a b", None),
            ("http://x/%zz", None),
            ("http://x/#%zz", None),
            ("http://x\n", None),
            ("http://bad user@x", None),
        ];
        for (raw, want) in cases {
            let got = parse(raw).ok().map(|u| (u.scheme, u.host));
            let want = want.map(|(s, h)| (s.to_owned(), h.to_owned()));
            assert_eq!(got, want, "url.Parse({raw:?})");
        }
    }

    #[test]
    fn hostname_matches_go() {
        for (raw, want) in [
            ("http://localhost:8080", "localhost"),
            ("http://[::1]:80", "::1"),
            ("http://[::1]", "::1"),
            ("http://example.com", "example.com"),
            ("http://example.com:", "example.com"),
        ] {
            assert_eq!(parse(raw).unwrap().hostname(), want, "Hostname({raw:?})");
        }
    }

    /// Go 1.26 `URL.String()` after `url.Parse`, including userinfo, escaped hosts and
    /// fragments that Go re-escapes, and the `Fragment` / `RawFragment` fields.
    #[test]
    fn string_and_fragment_match_go() {
        for (raw, want, frag, raw_frag) in [
            ("http://u:p%40ss@h/x", "http://u:p%40ss@h/x", "", ""),
            ("http://u@h", "http://u@h", "", ""),
            (
                "http://ex%C3%A4mple.com/",
                "http://ex%C3%A4mple.com/",
                "",
                "",
            ),
            ("http://exämple.com/", "http://ex%C3%A4mple.com/", "", ""),
            ("http://h/p#a%20b", "http://h/p#a%20b", "a b", ""),
            ("http://h/p#a b", "http://h/p#a%20b", "a b", "a b"),
            ("http://h/p#x=%2F", "http://h/p#x=%2F", "x=/", "x=%2F"),
            ("http://h/p#\"q\"", "http://h/p#%22q%22", "\"q\"", "\"q\""),
            ("/a?b#", "/a?b", "", ""),
            ("http:///p", "http:///p", "", ""),
        ] {
            let u = parse(raw).unwrap();
            assert_eq!(u.to_go_string(), want, "String({raw:?})");
            assert_eq!(u.fragment(), frag.as_bytes(), "Fragment({raw:?})");
            assert_eq!(u.raw_fragment, raw_frag, "RawFragment({raw:?})");
        }
        assert!(parse("http://u:p@h").unwrap().has_user());
        assert!(!parse("http://h/@x").unwrap().has_user());
    }

    /// Go `url.PathEscape`, `url.PathUnescape` and `url.QueryUnescape`.
    #[test]
    fn segment_and_query_escaping_match_go() {
        assert_eq!(
            path_escape(b"a b/c;d,e?f:g@h&i=j+k$l[m]"),
            "a%20b%2Fc%3Bd%2Ce%3Ff:g@h&i=j+k$l%5Bm%5D"
        );
        assert_eq!(path_escape("é".as_bytes()), "%C3%A9");
        assert_eq!(path_unescape("a+b%2F"), Ok(b"a+b/".to_vec()));
        assert_eq!(query_unescape("a+b%2F"), Ok(b"a b/".to_vec()));
        assert_eq!(
            query_unescape("%zz"),
            Err("invalid URL escape \"%zz\"".into())
        );
        assert_eq!(
            query_unescape("%4"),
            Err("invalid URL escape \"%4\"".into())
        );
    }

    /// Go `url.JoinPath(base, elem)` results.
    #[test]
    fn join_path_matches_go() {
        for (base, elem, want) in [
            (
                "http://h",
                "/.well-known/agent-card.json",
                "http://h/.well-known/agent-card.json",
            ),
            (
                "http://h/",
                "/.well-known/agent-card.json",
                "http://h/.well-known/agent-card.json",
            ),
            ("http://h/a", "card.json", "http://h/a/card.json"),
            ("http://h/a/", "/card.json", "http://h/a/card.json"),
            ("http://h/a", "../b", "http://h/b"),
            ("http://h/a", "x/", "http://h/a/x/"),
            ("http://h/a?q=1#f", "c", "http://h/a/c?q=1#f"),
            ("HTTP://h:8080", "c", "http://h:8080/c"),
            ("http://h/a%2Fb", "c", "http://h/a%2Fb/c"),
            ("http://h/a b", "c", "http://h/a%20b/c"),
        ] {
            let got = parse(base)
                .unwrap()
                .join_path(&[elem])
                .unwrap()
                .to_go_string();
            assert_eq!(got, want, "url.JoinPath({base:?}, {elem:?})");
        }
        assert_eq!(
            parse("http://h").unwrap().join_path(&["%zz"]).unwrap_err(),
            "invalid URL escape \"%zz\""
        );
    }
}
