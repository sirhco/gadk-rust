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

//! The subset of Go's `mime.ParseMediaType` that `decodeTextBlob` reaches, and ADK-Go's
//! `hasMIMECharsetParameter` / `isTextMediaType` (`tool/mcptoolset/tool.go`).
//!
//! Go's parser works on bytes; every slice boundary below falls on an ASCII byte (a separator,
//! a quote, or the first byte that is not a token character), so the `&str` slices are always on
//! character boundaries. Go's whitespace (`unicode.IsSpace`) and Rust's `char::is_whitespace`
//! are the same set.

use std::collections::BTreeMap;

/// Why [`parse_media_type`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaTypeError {
    /// The media type itself is malformed, or a parameter name repeats with another value.
    Invalid,
    /// Go `mime.ErrInvalidMediaParameter`: the media type parsed, a parameter did not.
    InvalidParameter,
}

/// Go `mime.ParseMediaType`: the lowercased media type and its parameters. On
/// [`MediaTypeError::InvalidParameter`] the error carries the media type (Go returns it with no
/// parameters); on [`MediaTypeError::Invalid`] it carries `""`.
pub(crate) fn parse_media_type(
    v: &str,
) -> Result<(String, BTreeMap<String, String>), (MediaTypeError, String)> {
    let base = v.split_once(';').map_or(v, |(base, _)| base);
    let mediatype = go_to_lower(base).trim().to_owned();
    if !check_media_type_disposition(&mediatype) {
        return Err((MediaTypeError::Invalid, String::new()));
    }

    let mut params: BTreeMap<String, String> = BTreeMap::new();
    // Base parameter name -> parameter name -> value, for names containing a '*'.
    let mut continuation: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    let mut v = &v[base.len()..];
    while !v.is_empty() {
        v = v.trim_start_matches(char::is_whitespace);
        if v.is_empty() {
            break;
        }
        let Some((key, value, rest)) = consume_media_param(v) else {
            if v.trim() == ";" {
                // Ignore trailing semicolons. Not an error.
                break;
            }
            return Err((MediaTypeError::InvalidParameter, mediatype));
        };

        let pmap = match key.split_once('*') {
            Some((base_name, _)) => continuation.entry(base_name.to_owned()).or_default(),
            None => &mut params,
        };
        if let Some(existing) = pmap.get(&key)
            && *existing != value
        {
            // Duplicate parameter names are incorrect, but allowed when the values are equal.
            return Err((MediaTypeError::Invalid, String::new()));
        }
        pmap.insert(key, value);
        v = rest;
    }

    // Stitch together RFC 2231 continuations and starred names ("foo*0", "foo*").
    for (key, piece_map) in &continuation {
        let single_part_key = format!("{key}*");
        if let Some(v) = piece_map.get(&single_part_key) {
            if let Some(decoded) = decode_2231_enc(v) {
                params.insert(key.clone(), decoded);
            }
            continue;
        }

        let mut buf: Vec<u8> = Vec::new();
        let mut valid = false;
        let mut n = 0usize;
        loop {
            let simple_part = format!("{key}*{n}");
            if let Some(v) = piece_map.get(&simple_part) {
                valid = true;
                buf.extend_from_slice(v.as_bytes());
                n += 1;
                continue;
            }
            let encoded_part = format!("{simple_part}*");
            let Some(v) = piece_map.get(&encoded_part) else {
                break;
            };
            valid = true;
            if n == 0 {
                if let Some(decoded) = decode_2231_enc(v) {
                    buf.extend_from_slice(decoded.as_bytes());
                }
            } else if let Some(decoded) = percent_hex_unescape(v) {
                buf.extend_from_slice(&decoded);
            }
            n += 1;
        }
        if valid {
            params.insert(key.clone(), String::from_utf8_lossy(&buf).into_owned());
        }
    }

    Ok((mediatype, params))
}

/// Go `checkMediaTypeDisposition`: `token` or `token "/" token`, nothing else.
fn check_media_type_disposition(s: &str) -> bool {
    let (typ, rest) = consume_token(s);
    if typ.is_empty() {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    let Some(rest) = rest.strip_prefix('/') else {
        return false;
    };
    let (subtype, rest) = consume_token(rest);
    !subtype.is_empty() && rest.is_empty()
}

/// Go `decode2231Enc`: `charset'language'percent-encoded`, for `us-ascii` and `utf-8` only.
/// Go keeps the decoded bytes as they are; invalid UTF-8 becomes U+FFFD here, which no charset
/// comparison accepts either way.
fn decode_2231_enc(v: &str) -> Option<String> {
    let (charset, v) = v.split_once('\'')?;
    // The language part is ignored, as in Go.
    let (_, ext_other_vals) = v.split_once('\'')?;
    match go_to_lower(charset).as_str() {
        "us-ascii" | "utf-8" => {}
        // Empty or unsupported encoding.
        _ => return None,
    }
    percent_hex_unescape(ext_other_vals).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Go `consumeToken`: the longest prefix of token characters, and the rest.
fn consume_token(v: &str) -> (&str, &str) {
    let end = v.bytes().position(|c| !is_token_char(c)).unwrap_or(v.len());
    v.split_at(end)
}

/// Go `consumeValue`: a token or a quoted string (unquoted, with Go's MSIE-tolerant escapes).
/// `None` is Go's `("", v)` failure.
fn consume_value(v: &str) -> Option<(String, &str)> {
    let bytes = v.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if bytes[0] != b'"' {
        let (token, rest) = consume_token(v);
        if token.is_empty() {
            return None;
        }
        return Some((token.to_owned(), rest));
    }

    let mut buffer: Vec<u8> = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        let r = bytes[i];
        if r == b'"' {
            return Some((String::from_utf8_lossy(&buffer).into_owned(), &v[i + 1..]));
        }
        // An unnecessary backslash escape (MSIE file paths) is a literal backslash.
        if r == b'\\' && i + 1 < bytes.len() && is_tspecial(bytes[i + 1]) {
            buffer.push(bytes[i + 1]);
            i += 2;
            continue;
        }
        if r == b'\r' || r == b'\n' {
            return None;
        }
        buffer.push(r);
        i += 1;
    }
    // No end quote.
    None
}

/// Go `consumeMediaParam`: `; name = value`. `None` is Go's `("", "", v)` failure; the
/// value may be empty only when it is a quoted `""`.
fn consume_media_param(v: &str) -> Option<(String, String, &str)> {
    let rest = v.trim_start_matches(char::is_whitespace);
    let rest = rest.strip_prefix(';')?;
    let rest = rest.trim_start_matches(char::is_whitespace);
    let (param, rest) = consume_token(rest);
    let param = param.to_ascii_lowercase();
    if param.is_empty() {
        return None;
    }
    let rest = rest.trim_start_matches(char::is_whitespace);
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start_matches(char::is_whitespace);
    let (value, rest) = consume_value(rest)?;
    Some((param, value, rest))
}

/// Go `percentHexUnescape`: every `%` must start a two-digit hex escape.
fn percent_hex_unescape(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        if i + 2 >= b.len() || !b[i + 1].is_ascii_hexdigit() || !b[i + 2].is_ascii_hexdigit() {
            return None;
        }
        i += 3;
    }

    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push((unhex(b[i + 1]) << 4) | unhex(b[i + 2]));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

fn unhex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Go `isTSpecial` (RFC 2045 tspecials).
fn is_tspecial(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')'
            | b'<'
            | b'>'
            | b'@'
            | b','
            | b';'
            | b':'
            | b'\\'
            | b'"'
            | b'/'
            | b'['
            | b']'
            | b'?'
            | b'='
    )
}

/// Go `isTokenChar`: US-ASCII except space, controls and tspecials.
fn is_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
        )
}

/// Go `strings.ToLower`: per-rune simple lowercase. Rust's `str::to_lowercase` differs for
/// U+0130 (Go maps it to `i`, Rust to `i` plus U+0307) and for a final `Σ` (Go `σ`, Rust `ς`).
pub(crate) fn go_to_lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{130}' => 'i',
            c => c.to_lowercase().next().unwrap_or(c),
        })
        .collect()
}

/// Go `hasMIMECharsetParameter`: whether `value` declares a charset parameter, or is ambiguous
/// enough that it might (a quote that does not open a parameter value, a quoted parameter Go's
/// parser rejects, or an unterminated quote).
pub(crate) fn has_mime_charset_parameter(value: &str) -> bool {
    let Some((_, params)) = value.split_once(';') else {
        return false;
    };
    let bytes = params.as_bytes();

    let mut start = 0;
    let mut in_quote = false;
    let mut i = 0;
    while i <= bytes.len() {
        if i != bytes.len() {
            match bytes[i] {
                b'\\' => {
                    if in_quote && i + 1 < bytes.len() {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                b'"' => {
                    if in_quote {
                        // Validate the complete quoted parameter before trusting its boundaries.
                        let candidate = format!("text/plain;{}", &params[start..=i]);
                        if parse_media_type(&candidate).is_err() {
                            return true;
                        }
                    } else {
                        // Only a parameter value may open a quote; other positions are ambiguous.
                        match params[start..i].split_once('=') {
                            Some((_, prefix)) if prefix.trim().is_empty() => {}
                            _ => return true,
                        }
                    }
                    in_quote = !in_quote;
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        if i != bytes.len() && (bytes[i] != b';' || in_quote) {
            i += 1;
            continue;
        }

        let parameter = &params[start..i];
        let name = parameter
            .split_once('=')
            .map_or(parameter, |(name, _)| name);
        let name = go_to_lower(name.trim());
        if name == "charset" || name.starts_with("charset*") {
            return true;
        }
        start = i + 1;
        i += 1;
    }
    in_quote
}

/// Go `isTextMediaType`.
pub(crate) fn is_text_media_type(media_type: &str) -> bool {
    if media_type.starts_with("text/")
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
    {
        return true;
    }
    matches!(
        media_type,
        "application/json"
            | "application/javascript"
            | "application/toml"
            | "application/x-www-form-urlencoded"
            | "application/xml"
            | "application/x-yaml"
            | "application/yaml"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values printed by Go 1.27 `mime.ParseMediaType` for the same inputs.
    #[test]
    fn parse_media_type_matches_go() {
        type Want = Result<
            (&'static str, &'static [(&'static str, &'static str)]),
            (MediaTypeError, &'static str),
        >;
        let cases: &[(&str, Want)] = &[
            ("text/plain", Ok(("text/plain", &[]))),
            (
                " Text/HTML ; Charset=UTF-8",
                Ok(("text/html", &[("charset", "UTF-8")])),
            ),
            ("text/plain;", Ok(("text/plain", &[]))),
            (
                "text/plain; ;",
                Err((MediaTypeError::InvalidParameter, "text/plain")),
            ),
            ("text/plain; a=1; a=1", Ok(("text/plain", &[("a", "1")]))),
            ("text/plain; a=1; a=2", Err((MediaTypeError::Invalid, ""))),
            ("text/plain; a=1; A=2", Err((MediaTypeError::Invalid, ""))),
            (
                r#"text/plain; name="a\"b""#,
                Ok(("text/plain", &[("name", "a\"b")])),
            ),
            (
                r#"text/plain; name="C:\dev\go""#,
                Ok(("text/plain", &[("name", r"C:\dev\go")])),
            ),
            (
                r#"text/plain; name="""#,
                Ok(("text/plain", &[("name", "")])),
            ),
            (
                "text/plain; title*=us-ascii'en-us'This%20is%20%2A%2A%2Afun%2A%2A%2A",
                Ok(("text/plain", &[("title", "This is ***fun***")])),
            ),
            (
                "text/plain; title*0*=us-ascii'en'This%20is%20even%20more%20; \
                 title*1*=%2A%2A%2Afun%2A%2A%2A%20; title*2=\"isn't it!\"",
                Ok((
                    "text/plain",
                    &[("title", "This is even more ***fun*** isn't it!")],
                )),
            ),
            (
                "text/plain; charset*=utf-8''UTF-8",
                Ok(("text/plain", &[("charset", "UTF-8")])),
            ),
            ("text/plain; charset*=utf-16''x", Ok(("text/plain", &[]))),
            ("text/plain; charset*=utf-8''%ZZ", Ok(("text/plain", &[]))),
            (
                "text/plain; charset*0=utf; charset*1=-8",
                Ok(("text/plain", &[("charset", "utf-8")])),
            ),
            ("text/plain; x*=a; x*=b", Err((MediaTypeError::Invalid, ""))),
            ("text", Ok(("text", &[]))),
            ("text/", Err((MediaTypeError::Invalid, ""))),
            ("/plain", Err((MediaTypeError::Invalid, ""))),
            ("text/plain/x", Err((MediaTypeError::Invalid, ""))),
            ("", Err((MediaTypeError::Invalid, ""))),
            (
                "text/plain; name=my file.txt",
                Err((MediaTypeError::InvalidParameter, "text/plain")),
            ),
            ("text/plain extra", Err((MediaTypeError::Invalid, ""))),
            (
                "text/plain;charset=utf-8;",
                Ok(("text/plain", &[("charset", "utf-8")])),
            ),
            (
                "application/json; profile=https://example.com/schema",
                Err((MediaTypeError::InvalidParameter, "application/json")),
            ),
            ("text/pla\u{130}n", Ok(("text/plain", &[]))),
            ("te\u{212a}xt/plain", Ok(("tekxt/plain", &[]))),
        ];
        for (input, want) in cases {
            let want = want
                .map(|(media_type, params)| {
                    let params = params
                        .iter()
                        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
                        .collect::<BTreeMap<_, _>>();
                    (media_type.to_owned(), params)
                })
                .map_err(|(kind, media_type)| (kind, media_type.to_owned()));
            assert_eq!(parse_media_type(input), want, "{input:?}");
        }
    }

    #[test]
    fn is_text_media_type_matches_go_list() {
        for media_type in [
            "text/plain",
            "application/problem+json",
            "image/svg+xml",
            "application/json",
            "application/javascript",
            "application/toml",
            "application/x-www-form-urlencoded",
            "application/xml",
            "application/x-yaml",
            "application/yaml",
        ] {
            assert!(is_text_media_type(media_type), "{media_type}");
        }
        for media_type in [
            "application/pdf",
            "image/png",
            "application/octet-stream",
            "text",
        ] {
            assert!(!is_text_media_type(media_type), "{media_type}");
        }
    }
}
