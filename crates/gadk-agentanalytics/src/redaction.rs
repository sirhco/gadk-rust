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

//! Port of ADK-Go `plugin/agentanalytics/redaction.go`: sensitive-key detection, URI / path /
//! query sanitizing, credential-marker canonicalization, JSON-text redaction (fail-closed),
//! error-message and raw-text scrubbing, the content-formatter guard, the payload-column
//! denylist and the custom-metadata filter. Go's `session.KeyPrefixUser/App/Temp` are
//! `gadk_session::KEY_PREFIX_USER/APP/TEMP`.
//!
//! The regular expressions are Go's RE2 sources, built by the same functions as in Go
//! ([`go_regex_source`]) and translated for the `regex` crate by [`go_to_rust_regex`], which
//! keeps Go's ASCII-only `\s`, `\w` and `\b`. Go's `net/url` is `gadk_platform::go_url`; Go's
//! `encoding/json` decoding (the `Decoder.Token` grammar, `json.Number`, invalid surrogates as
//! U+FFFD, `SyntaxError.Offset`) is ported here ([`scan_json_string`], [`StrictDecoder`]), and
//! `json.Marshal` of a decoded value is [`marshal`] (sorted keys, Go string escaping through
//! `gadk_model::to_go_json`).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;

use gadk_platform::{go_query_escape, go_quote, go_to_lower, go_url};
use gadk_session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER};
use regex::Regex;
use serde_json::{Map, Number, Value};

use crate::config::{ContentFormatter, Logger};
use crate::error::Error;
use crate::formatter::{
    BINARY_DATA_PLACEHOLDER, CIRCULAR_REF_PLACEHOLDER, INLINE_DATA_TEXT, MAX_DEPTH_PLACEHOLDER,
    MAX_SANITIZE_DEPTH, NodeBudget, SANITIZE_BUDGET_PLACEHOLDER, UNSUPPORTED_KEY_PLACEHOLDER,
    UNSUPPORTED_OBJ_PLACEHOLDER,
};

/// Go `redactedValue`.
pub(crate) const REDACTED_VALUE: &str = "[REDACTED]";
/// Go `redactedURIPlaceholder`.
pub(crate) const REDACTED_URI_PLACEHOLDER: &str = "[REDACTED_SENSITIVE_URI]";
/// Go `redactedSensitiveTextPlaceholder`.
pub(crate) const REDACTED_SENSITIVE_TEXT_PLACEHOLDER: &str = "[REDACTED_SENSITIVE_TEXT]";
/// Go `formattingFailedPlaceholder`.
pub(crate) const FORMATTING_FAILED_PLACEHOLDER: &str = "[FORMATTER_FAILED]";
/// Go `unparseableJSONPlaceholder`.
pub(crate) const UNPARSEABLE_JSON_PLACEHOLDER: &str = "[UNPARSEABLE_JSON_BLOB]";

/// Go `maxInspectableJSONTextBytes` (adk-python `_MAX_JSON_INSPECT_CHARS`).
pub(crate) const MAX_INSPECTABLE_JSON_TEXT_BYTES: usize = 4_000_000;
/// Go `maxCanonicalUnescapePasses`.
pub(crate) const MAX_CANONICAL_UNESCAPE_PASSES: usize = 4;
/// Go `maxQuotedFragmentsPerInspect`.
pub(crate) const MAX_QUOTED_FRAGMENTS_PER_INSPECT: usize = 64;
/// Go `maxBasicCredentialBytes` (adk-python `_MAX_BASIC_AUTH_TOKEN_CHARS`).
const MAX_BASIC_CREDENTIAL_BYTES: usize = 16 * 1024;
/// Go `maxNestedKeyLen`: the longest value `nests_sensitive_key` may take for a key.
const MAX_NESTED_KEY_LEN: usize = 128;

/// Go `regexpSpace`: the bytes `\s` matches in a Go regular expression.
const REGEXP_SPACE: &str = "\t\n\x0c\r ";
/// Go `delimSpace`: the bytes `[\s[:cntrl:]]` matches around delimiters in
/// `textKeyValTokenRe`.
const DELIM_SPACE: &str = "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f \x7f";
/// Go `closingPunctuation`: what may follow the text of a bare value, as in the `%q` output
/// `"{APIKey:...}"`.
const CLOSING_PUNCTUATION: &str = "}])\"'\\:.";

/// Go `columnContent`: a payload column [`Config::payload_column_denylist`] may suppress.
///
/// [`Config::payload_column_denylist`]: crate::config::Config::payload_column_denylist
pub(crate) const COLUMN_CONTENT: &str = "content";
/// Go `columnContentParts`.
pub(crate) const COLUMN_CONTENT_PARTS: &str = "content_parts";
/// Go `columnAttributes`.
pub(crate) const COLUMN_ATTRIBUTES: &str = "attributes";
/// Go `columnLatencyMS`.
pub(crate) const COLUMN_LATENCY_MS: &str = "latency_ms";

/// Go `sensitiveKeys` (sorted): normalized (lowercase, `-` replaced by `_`) map keys whose
/// values are always redacted — the adk-python list plus the names ADK-Go adds.
const SENSITIVE_KEYS: [&str; 34] = [
    "access_token",
    "api_key",
    "api_key_secret",
    "api_key_string",
    "auth_code",
    "authorization",
    "aws_secret_access_key",
    "aws_session_token",
    "bearer",
    "client_secret",
    "code_verifier",
    "credential",
    "credentials",
    "google_access_id",
    "id_token",
    "passphrase",
    "password",
    "private_key",
    "proxy_authorization",
    "refresh_token",
    "secret",
    "secret_access_key",
    "session_token",
    "sig",
    "signature",
    "token",
    "x_amz_credential",
    "x_amz_signature",
    "x_api_key",
    "x_auth_token",
    "x_goog_api_key",
    "x_goog_credential",
    "x_goog_security_token",
    "x_goog_signature",
];

/// Go `sensitiveKeyNames`: [`SENSITIVE_KEYS`] with `_` removed, the form in which
/// [`is_sensitive_key`] compares keys.
static SENSITIVE_KEY_NAMES: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    SENSITIVE_KEYS
        .iter()
        .map(|k| remove_key_separators(k))
        .collect()
});

/// Go `patternWords`: words of which every match of the credential text patterns contains at
/// least one once the text is folded by [`fold_pattern_text`] — the sensitive key names
/// without separators, the `temp:` prefix and the `basic` scheme, minus every word that
/// contains another one.
static PATTERN_WORDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut words = vec!["basic".to_owned(), KEY_PREFIX_TEMP.to_owned()];
    words.extend(SENSITIVE_KEY_NAMES.iter().cloned());
    let mut minimal: Vec<String> = words
        .iter()
        .filter(|w| !words.iter().any(|o| o != *w && w.contains(o.as_str())))
        .cloned()
        .collect();
    minimal.sort();
    minimal
});

/// Go `regexp.QuoteMeta`.
fn regexp_quote_meta(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Go `fixedSensitiveKeyRegex()`: an alternation of [`SENSITIVE_KEYS`] in which each `_` may
/// also be written as `-` or left out.
fn fixed_sensitive_key_regex() -> String {
    SENSITIVE_KEYS
        .iter()
        .map(|k| regexp_quote_meta(k).replace('_', "[_-]?"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Go `sensitiveKeyRegex()`: key names read inside quotes, where a `temp:` key name may hold
/// `=` and `:`.
fn sensitive_key_regex() -> String {
    format!(r#"temp:[^"'\s\r\n,}}]+|{}"#, fixed_sensitive_key_regex())
}

/// Go `bareSensitiveKeyRegex()`: key names in unquoted `key=value` / `key: value` text; a
/// `temp:` key name ends before the first `=` or `:`.
fn bare_sensitive_key_regex() -> String {
    format!(r#"temp:[^=:"'\s\r\n,}}]+|{}"#, fixed_sensitive_key_regex())
}

/// Go `escapedQuotedRegex(q)`: a string delimited by `\q`, as produced by quoting JSON text
/// again.
fn escaped_quoted_regex(q: &str) -> String {
    format!(r"\\{q}(?:\\\\(?:\\\\|\\{q}|[^\\{q}])|\\[^\\{q}]|[^\\{q}])*\\{q}")
}

/// Go `quotedValueRegex()`: a double- or single-quoted string, or one quoted with escaped
/// quotes.
fn quoted_value_regex() -> String {
    format!(
        r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|{}|{}"#,
        escaped_quoted_regex("\""),
        escaped_quoted_regex("'")
    )
}

/// Go `sensitiveValueRegex(bareStop)`: a quoted value, or a run of bytes outside `bare_stop`.
fn sensitive_value_regex(bare_stop: &str) -> String {
    format!("{}|[^{bare_stop}]+", quoted_value_regex())
}

/// The Go (RE2) source of each package regular expression, by its Go variable name, built as
/// `redaction.go` builds it (`regexp.MustCompile(...)`); `""` for an unknown name.
fn go_regex_source(name: &str) -> String {
    match name {
        "uriTokenRe" => r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s"'<>]+"#.to_owned(),
        "authHeaderRe" => {
            r"(?im)\b((?:proxy[-_])?authorization|x[-_]api[-_]key|api[-_]key)[ \t]*:[ \t]*[^\r\n]*"
                .to_owned()
        }
        "bearerTokenRe" => {
            r#"(?i)\b(bearer)\s+("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\[[^\]\r\n\s]+\][^\s,;)"']*|[^\s,;)"'\]]+)"#
                .to_owned()
        }
        "basicTokenRe" => r"(?i)\b(basic)\s+([A-Za-z0-9+/]+={0,2})".to_owned(),
        "jsonKeyValTokenRe" => format!(
            r#"(?i)"[\s\p{{Z}}]*({})[\s\p{{Z}}]*"\s*:\s*({})"#,
            sensitive_key_regex(),
            sensitive_value_regex(r"\s,}")
        ),
        "textKeyValTokenRe" => format!(
            r#"(?i)\b({})((?:\\?["'])?)[\s[:cntrl:]]*([=:])[\s[:cntrl:]]*({})"#,
            bare_sensitive_key_regex(),
            sensitive_value_regex(r"\s,;&")
        ),
        "textKeyValTokenAtRe" => format!(
            r#"(?i)^({})((?:\\?["'])?)[\s[:cntrl:]]*([=:])[\s[:cntrl:]]*({})"#,
            bare_sensitive_key_regex(),
            sensitive_value_regex(r"\s,;&")
        ),
        "nestedKeyNameRe" => format!(
            r#"(?i)^(?:\\?["'])?(?:{}|{})?({})$"#,
            regexp_quote_meta(KEY_PREFIX_USER),
            regexp_quote_meta(KEY_PREFIX_APP),
            fixed_sensitive_key_regex()
        ),
        "compoundKeyNameRe" => format!(
            r#"(?i)^(?:\\?["'])?(?:[^\s"'\\]*[^\w\s"'\\])?({})$"#,
            bare_sensitive_key_regex()
        ),
        "quotedValueRe" => format!("^(?:{})", quoted_value_regex()),
        "quotedSensitiveKeyRe" => format!(
            r#"(?i)["'][\s\p{{Z}}]*({})[\s\p{{Z}}]*["'][\s\p{{Z}}]*[:=]"#,
            sensitive_key_regex()
        ),
        _ => String::new(),
    }
}

/// Translates a Go RE2 source into one the `regex` crate reads with Go's meaning: Go's `\s`
/// (`[\t\n\f\r ]`), `\w` (`[0-9A-Za-z_]`) and `\b` (an ASCII word boundary) are ASCII-only,
/// while the `regex` crate's are Unicode-aware, and a `[` inside a Go class is a literal, while
/// the `regex` crate reads it as a nested class. Everything else (`(?i)` simple case folding,
/// `\p{Z}`, `[:cntrl:]`, leftmost-first matching) means the same in both.
fn go_to_rust_regex(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    let mut in_class = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(n) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match (n, in_class) {
                    ('s', false) => out.push_str("[\\t\\n\\f\\r ]"),
                    ('s', true) => out.push_str("\\t\\n\\f\\r "),
                    ('w', false) => out.push_str("[0-9A-Za-z_]"),
                    ('w', true) => out.push_str("0-9A-Za-z_"),
                    ('b', false) => out.push_str("(?-u:\\b)"),
                    _ => {
                        out.push('\\');
                        out.push(n);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push('^');
                    chars.next();
                }
                if chars.peek() == Some(&']') {
                    out.push_str("\\]");
                    chars.next();
                }
            }
            '[' if chars.peek() == Some(&':') => {
                // A POSIX class such as [:cntrl:], copied up to its ":]".
                out.push('[');
                let mut prev = '[';
                for p in chars.by_ref() {
                    out.push(p);
                    if prev == ':' && p == ']' {
                        break;
                    }
                    prev = p;
                }
            }
            '[' => out.push_str("\\["),
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Go `regexp.MustCompile` of the named package regular expression.
#[allow(clippy::expect_used)] // a constant pattern (Go regexp.MustCompile)
fn must_compile(name: &str) -> Regex {
    regex::RegexBuilder::new(&go_to_rust_regex(&go_regex_source(name)))
        .size_limit(64 << 20)
        .build()
        .expect("valid redaction regex")
}

static URI_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("uriTokenRe"));
static AUTH_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("authHeaderRe"));
/// Go `bearerTokenRe`: a bearer token (RFC 6750) never holds quotes, `)` or `]`, so the bare
/// value stops there.
static BEARER_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("bearerTokenRe"));
/// Go `basicTokenRe`: a candidate basic credential, redacted only when it is valid base64.
static BASIC_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("basicTokenRe"));
static JSON_KEY_VAL_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("jsonKeyValTokenRe"));
/// Go `textKeyValTokenRe`: `key=value` and `key: value` pairs.
static TEXT_KEY_VAL_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("textKeyValTokenRe"));
static TEXT_KEY_VAL_TOKEN_AT_RE: LazyLock<Regex> =
    LazyLock::new(|| must_compile("textKeyValTokenAtRe"));
/// Go `nestedKeyNameRe`: a value that is itself a fixed sensitive key name.
static NESTED_KEY_NAME_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("nestedKeyNameRe"));
/// Go `compoundKeyNameRe`: a value that is a `temp:` key name or only ends in a sensitive key
/// name.
static COMPOUND_KEY_NAME_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("compoundKeyNameRe"));
/// Go `quotedValueRe`: a quoted value at the start of a text.
static QUOTED_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| must_compile("quotedValueRe"));
/// Go `quotedSensitiveKeyRe`: a quoted sensitive key followed by a key-value delimiter.
static QUOTED_SENSITIVE_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| must_compile("quotedSensitiveKeyRe"));
/// Go `r == '_' || unicode.IsLetter(r) || unicode.IsDigit(r)` for the first rune of a text.
#[allow(clippy::expect_used)] // a constant pattern
static WORD_RUNE_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[_\p{L}\p{Nd}]").expect("valid word-rune regex"));

/// A missing submatch in a [`submatch_index`] result (Go's `-1`).
const NO_MATCH: usize = usize::MAX;

/// Go `re.FindStringSubmatchIndex(s)`: the start and end of the match and of each group,
/// [`NO_MATCH`] for a group that did not take part.
fn submatch_index(re: &Regex, s: &str) -> Option<Vec<usize>> {
    let caps = re.captures(s)?;
    let mut loc = Vec::with_capacity(caps.len() * 2);
    for i in 0..caps.len() {
        match caps.get(i) {
            Some(m) => {
                loc.push(m.start());
                loc.push(m.end());
            }
            None => {
                loc.push(NO_MATCH);
                loc.push(NO_MATCH);
            }
        }
    }
    Some(loc)
}

/// Go `unicode.IsSpace(r)` (the Unicode `White_Space` property, as Rust's).
fn is_space(r: char) -> bool {
    r.is_whitespace()
}

/// Go `unicode.In(r, unicode.Z)`: the separator categories Zs, Zl and Zp.
fn is_unicode_z(r: char) -> bool {
    matches!(
        r,
        '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// Go `trimBOMAndSpace(s)`: `s` without leading and trailing white space and U+FEFF.
fn trim_bom_and_space(s: &str) -> &str {
    s.trim_matches(|r: char| is_space(r) || r == '\u{feff}')
}

/// The largest char boundary of `s` at or below `i`. Divergence (ledger 7b-1): Go slices bytes
/// (`s[:maxInspectableJSONTextBytes]`, `loc[0]+1`), and a cut inside a rune leaves invalid UTF-8
/// there, which a Rust `&str` cannot hold; the cut is rounded to a char boundary instead (no
/// probe corpus input reaches it).
fn floor_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The smallest char boundary of `s` at or above `i`.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Bytes Go would hold in a string, as Rust text. Divergence (ledger 7b-1): a percent escape
/// that decodes to invalid UTF-8 (`%FF`) is U+FFFD here, so a redacted component that holds
/// one is re-escaped as `%EF%BF%BD` where Go writes `%FF` back.
fn lossy(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Go `mayMatchTextPatterns(s)`: whether `s` holds one of [`PATTERN_WORDS`], ignoring case.
/// When it does not, none of the credential text patterns can match.
fn may_match_text_patterns(s: &str) -> bool {
    let folded = fold_pattern_text(s);
    PATTERN_WORDS.iter().any(|w| folded.contains(w.as_str()))
}

/// Go `foldPatternText(s)`: ASCII letters lowercased, `-` and `_` removed, U+017F and U+212A
/// (which `(?i)` matches against `s` and `k`) replaced by `s` and `k`.
fn fold_pattern_text(s: &str) -> String {
    let mut b = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' => b.push(c.to_ascii_lowercase()),
            '-' | '_' => {}
            '\u{17f}' => b.push('s'),
            '\u{212a}' => b.push('k'),
            _ => b.push(c),
        }
    }
    b
}

/// Go `hasQuotedSensitiveKey(s)`.
fn has_quoted_sensitive_key(s: &str) -> bool {
    may_match_text_patterns(s) && QUOTED_SENSITIVE_KEY_RE.is_match(s)
}

/// Go `normalizeKey(k)`: lowercased, trimmed, `-` replaced by `_`.
fn normalize_key(k: &str) -> String {
    go_to_lower(k.trim()).replace('-', "_")
}

/// Go `removeKeySeparators(k)`: `k` without `_` and `-`.
fn remove_key_separators(k: &str) -> String {
    k.chars().filter(|&r| r != '_' && r != '-').collect()
}

/// Go `isSensitiveKey(k)`: whether the values under map key `k` are redacted (case- and
/// separator-insensitive, `user:` / `app:` prefixes stripped, every `temp:` key).
pub(crate) fn is_sensitive_key(k: &str) -> bool {
    let n = go_to_lower(k.trim());
    if n.starts_with(KEY_PREFIX_TEMP) {
        return true;
    }
    let n = match n.strip_prefix(KEY_PREFIX_USER) {
        Some(rest) => rest,
        None => n.strip_prefix(KEY_PREFIX_APP).unwrap_or(&n),
    };
    SENSITIVE_KEY_NAMES.contains(&remove_key_separators(n.trim()))
}

/// Go `canonicalizeCredentialMarker(s)`: nested ASCII `\u`, `\x` and percent escapes, and
/// escaped quotes, decoded in one pass. The result is for detection only.
pub(crate) fn canonicalize_credential_marker(s: &str) -> String {
    if !s.contains(['\\', '%']) {
        // Every escape starts with "\" or "%".
        return s.to_owned();
    }
    let mut output: Vec<char> = Vec::with_capacity(s.len());
    for r in s.chars() {
        output.push(r);
        loop {
            let n = output.len();
            let mut escape: Option<(usize, u32)> = None;
            if n >= 6
                && output[n - 6] == '\\'
                && (output[n - 5] == 'u' || output[n - 5] == 'U')
                && output[n - 4..].iter().all(char::is_ascii_hexdigit)
            {
                escape = Some((n - 6, hex_value(&output[n - 4..])));
            } else if n >= 4
                && output[n - 4] == '\\'
                && (output[n - 3] == 'x' || output[n - 3] == 'X')
                && output[n - 2..].iter().all(char::is_ascii_hexdigit)
            {
                escape = Some((n - 4, hex_value(&output[n - 2..])));
            } else if n >= 3
                && output[n - 3] == '%'
                && output[n - 2..].iter().all(char::is_ascii_hexdigit)
            {
                escape = Some((n - 3, hex_value(&output[n - 2..])));
            } else if n >= 2
                && output[n - 2] == '\\'
                && (output[n - 1] == '"' || output[n - 1] == '\'')
            {
                let q = output[n - 1];
                output.truncate(n - 2);
                output.push(q);
                continue;
            }
            let Some((start, decoded)) = escape else {
                break;
            };
            if decoded > 0x7f {
                break;
            }
            output.truncate(start);
            output.push(char::from(decoded as u8));
        }
    }
    output.into_iter().collect()
}

/// The value of ASCII hex digits.
fn hex_value(digits: &[char]) -> u32 {
    digits
        .iter()
        .fold(0, |acc, d| acc * 16 + d.to_digit(16).unwrap_or(0))
}

/// Go `sanitizeURI(raw)`: `raw` with credentials removed from its userinfo, path, query and
/// fragment; `true` when it changed. A URI with user information, or one Go's `url.Parse`
/// rejects, becomes [`REDACTED_URI_PLACEHOLDER`].
pub(crate) fn sanitize_uri(raw: &str) -> (String, bool) {
    if raw.is_empty() {
        return (raw.to_owned(), false);
    }
    let mut u = match go_url::parse(raw) {
        Ok(u) if !u.has_user() => u,
        _ => return (REDACTED_URI_PLACEHOLDER.to_owned(), true),
    };
    let mut path_changed = false;
    let escaped_path = u.escaped_path();
    if !escaped_path.is_empty()
        && let Some((sanitized, unescaped)) = sanitize_path_segments(&escaped_path)
    {
        u.set_path_fields(unescaped, sanitized);
        path_changed = true;
    }
    let mut query_changed = false;
    if !u.raw_query().is_empty() {
        let (q, changed) = sanitize_query_pairs(u.raw_query(), false);
        u.set_raw_query(q);
        query_changed = changed;
    }
    let mut fragment_changed = false;
    if !u.fragment().is_empty() {
        let raw_fragment = u.escaped_fragment();
        let (sanitized_frag, ok) = sanitize_query_pairs(&raw_fragment, true);
        if ok {
            let fragment = go_url::path_unescape(&sanitized_frag)
                .unwrap_or_else(|_| sanitized_frag.as_bytes().to_vec());
            u.set_fragment_fields(fragment, sanitized_frag);
            fragment_changed = true;
        }
        let fragment = lossy(u.fragment().to_vec());
        let (safe_fragment, ok) = redact_uri_component(&fragment);
        if ok {
            u.set_fragment_fields(safe_fragment.into_bytes(), String::new());
            fragment_changed = true;
        }
    }
    if !path_changed && !query_changed && !fragment_changed {
        return (raw.to_owned(), false);
    }
    (u.to_go_string(), true)
}

/// Go `sanitizePathSegments(escapedPath)`: `Some((escaped, unescaped))` when a segment was
/// redacted.
fn sanitize_path_segments(escaped_path: &str) -> Option<(String, Vec<u8>)> {
    let mut segments: Vec<String> = escaped_path.split('/').map(str::to_owned).collect();
    let escaped_redacted = go_query_escape(REDACTED_VALUE);
    let mut changed = false;
    let mut redact_next = false;
    for seg_slot in &mut segments {
        let raw_seg = seg_slot.clone();
        if raw_seg.is_empty() {
            continue;
        }
        let seg = canonicalize_credential_marker(&raw_seg);
        if redact_next {
            *seg_slot = escaped_redacted.clone();
            changed = true;
            redact_next = false;
            continue;
        }
        if is_sensitive_key(&seg) {
            *seg_slot = escaped_redacted.clone();
            changed = true;
            redact_next = true;
            continue;
        }
        if let Some((raw_key, value)) = seg.split_once('=') {
            let canon_key = canonicalize_credential_marker(raw_key);
            if is_sensitive_key(&canon_key) || normalize_key(&canon_key) == "key" {
                if is_redacted_marker(value) {
                    // Already redacted, as when redacted text is scrubbed again.
                    continue;
                }
                *seg_slot = match raw_seg.split_once('=') {
                    Some((raw_left, _)) => format!("{raw_left}={escaped_redacted}"),
                    None => format!(
                        "{}={escaped_redacted}",
                        go_url::path_escape(canon_key.as_bytes())
                    ),
                };
                changed = true;
                continue;
            }
        }
        let decoded = match go_url::path_unescape(&raw_seg) {
            Ok(b) => lossy(b),
            Err(_) => raw_seg.clone(),
        };
        let (safe_seg, ok) = redact_uri_component(&decoded);
        if ok {
            *seg_slot = go_url::path_escape(safe_seg.as_bytes());
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    let escaped = segments.join("/");
    let unescaped = go_url::path_unescape(&escaped).unwrap_or_else(|_| escaped.as_bytes().to_vec());
    Some((escaped, unescaped))
}

/// Go `sanitizeQueryPairs(raw, requireEquals)`: the values of sensitive keys in `raw`, a query
/// or a fragment of `&`-separated pairs, redacted. For a fragment (`require_equals`), pairs
/// without `=` are skipped; for a query, the key and value of every other pair are scrubbed as
/// text.
fn sanitize_query_pairs(raw: &str, require_equals: bool) -> (String, bool) {
    let mut pairs: Vec<String> = raw.split('&').map(str::to_owned).collect();
    let mut changed = false;
    for pair in &mut pairs {
        let (raw_key, raw_value, has_eq) = match pair.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned(), true),
            None => (pair.clone(), String::new(), false),
        };
        if require_equals && !has_eq {
            continue;
        }
        let key = canonicalize_credential_marker(&raw_key);
        if is_sensitive_key(&key) || normalize_key(&key) == "key" {
            if has_eq && is_redacted_marker(&raw_value) {
                // Already redacted, as when redacted text is scrubbed again: re-encoding
                // "[REDACTED]" is not a change.
                continue;
            }
            *pair = format!("{raw_key}={}", go_query_escape(REDACTED_VALUE));
            changed = true;
            continue;
        }
        if require_equals {
            continue;
        }
        let (safe_key, key_changed) = redact_query_component(&raw_key);
        let (safe_value, value_changed) = redact_query_component(&raw_value);
        if key_changed || value_changed {
            *pair = safe_key;
            if has_eq {
                pair.push('=');
                pair.push_str(&safe_value);
            }
            changed = true;
        }
    }
    if !changed {
        return (raw.to_owned(), false);
    }
    (pairs.join("&"), true)
}

/// Go `redactQueryComponent(raw)`: one escaped query key or value scrubbed as text and escaped
/// again.
fn redact_query_component(raw: &str) -> (String, bool) {
    let decoded = match go_url::query_unescape(raw) {
        Ok(b) => lossy(b),
        Err(_) => raw.to_owned(),
    };
    let (safe, changed) = redact_uri_component(&decoded);
    if !changed {
        return (raw.to_owned(), false);
    }
    (go_query_escape(&safe), true)
}

/// Go `redactURIComponent(s)`: one decoded URI component scrubbed with the text patterns,
/// failing closed when decoding an escape in it exposes a credential pattern.
fn redact_uri_component(s: &str) -> (String, bool) {
    if s.is_empty() || is_known_sentinel(s) {
        return (s.to_owned(), false);
    }
    let scrubbed = scrub_text_patterns(s);
    if exposes_encoded_pattern(&scrubbed, scrub_text_patterns) {
        return (REDACTED_SENSITIVE_TEXT_PLACEHOLDER.to_owned(), true);
    }
    let changed = scrubbed != s;
    (scrubbed, changed)
}

/// Go `isKnownSentinel(s)`: `s` is one of the placeholders the formatter and redaction write.
fn is_known_sentinel(s: &str) -> bool {
    matches!(
        s,
        REDACTED_VALUE
            | REDACTED_URI_PLACEHOLDER
            | REDACTED_SENSITIVE_TEXT_PLACEHOLDER
            | FORMATTING_FAILED_PLACEHOLDER
            | UNPARSEABLE_JSON_PLACEHOLDER
            | INLINE_DATA_TEXT
            | BINARY_DATA_PLACEHOLDER
            | CIRCULAR_REF_PLACEHOLDER
            | MAX_DEPTH_PLACEHOLDER
            | SANITIZE_BUDGET_PLACEHOLDER
            | UNSUPPORTED_KEY_PLACEHOLDER
            | UNSUPPORTED_OBJ_PLACEHOLDER
    )
}

/// Go `sanitizeRawText(raw)`: free text with embedded JSON keys, URIs and credential patterns
/// redacted; `true` when content was lost. Positively harmless prose that only failed JSON
/// parsing is kept.
pub(crate) fn sanitize_raw_text(raw: &str) -> (String, bool) {
    let mut budget = NodeBudget::new();
    let (sanitized, changed, lost) = redact_json_text_with_budget(raw, 0, &mut budget);
    if !changed {
        let scrubbed = scrub_error_text(raw);
        if scrubbed != raw {
            return (scrubbed, true);
        }
        return (raw.to_owned(), false);
    }
    if sanitized != UNPARSEABLE_JSON_PLACEHOLDER {
        let scrubbed = scrub_error_text(&sanitized);
        if scrubbed != sanitized {
            return (scrubbed, true);
        }
        return (sanitized, lost);
    }
    let trimmed = trim_bom_and_space(raw);
    if is_positively_harmless_prose(raw, trimmed) {
        return (raw.to_owned(), false);
    }
    (sanitized, lost)
}

/// Go `hasCandidateQuotedSensitiveKey(s, depth)`: a quoted string in `s` (decoded, or
/// canonicalized) is a sensitive key followed by a key-value delimiter, at most 4 levels of
/// quoting deep.
fn has_candidate_quoted_sensitive_key(s: &str, depth: usize) -> bool {
    if depth > 4 || s.is_empty() {
        return false;
    }
    let s = &s[..floor_boundary(s, MAX_INSPECTABLE_JSON_TEXT_BYTES)];
    if has_quoted_sensitive_key(s) {
        return true;
    }
    let canon = canonicalize_credential_marker(s);
    if canon != s && has_quoted_sensitive_key(&canon) {
        return true;
    }

    let b = s.as_bytes();
    let mut in_quote = false;
    let mut quote_char = 0u8;
    let mut start = 0;
    let mut escaped = false;
    for (i, &c) in b.iter().enumerate() {
        if !in_quote {
            if c == b'"' || c == b'\'' {
                in_quote = true;
                quote_char = c;
                start = i;
                escaped = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if c == b'\\' {
            escaped = true;
            continue;
        }
        if c != quote_char {
            continue;
        }
        in_quote = false;
        let raw_candidate = &s[start + 1..i];
        let candidate = if quote_char == b'"' {
            match go_json_unmarshal_string(&s[start..=i]) {
                Some(unquoted) => unquoted,
                None => canonicalize_credential_marker(raw_candidate),
            }
        } else {
            canonicalize_credential_marker(raw_candidate)
        };
        if (is_sensitive_key(&candidate)
            || is_sensitive_key(&canonicalize_credential_marker(&candidate)))
            && followed_by_key_delimiter(&s[i + 1..])
        {
            return true;
        }
        if candidate.contains(['"', '\''])
            && has_candidate_quoted_sensitive_key(&candidate, depth + 1)
        {
            return true;
        }
    }
    false
}

/// Go `followedByKeyDelimiter(rest)`: `rest` starts with `:` or `=` after optional white space.
fn followed_by_key_delimiter(rest: &str) -> bool {
    for r in rest.chars() {
        if is_space(r) || is_unicode_z(r) {
            continue;
        }
        return r == ':' || r == '=';
    }
    false
}

/// Go `hasCredentialPattern(s)`.
fn has_credential_pattern(s: &str) -> bool {
    if scrub_error_text(s) != s {
        return true;
    }
    if has_candidate_quoted_sensitive_key(s, 0) {
        return true;
    }
    has_quoted_sensitive_key(&canonicalize_credential_marker(s))
}

/// Go `isPositivelyHarmlessProse(raw, trimmed)`: bracket-, brace- or quote-led text that failed
/// JSON parsing and carries no credential pattern.
fn is_positively_harmless_prose(raw: &str, trimmed: &str) -> bool {
    if raw.len() > MAX_INSPECTABLE_JSON_TEXT_BYTES {
        return false;
    }
    // An escaped quote can hide a quoted credential key from the pattern checks below; other
    // backslashes (paths, regexes) are harmless.
    if raw.contains("\\\"") {
        return false;
    }
    if has_credential_pattern(raw)
        || has_encoded_credential_marker(raw)
        || has_candidate_quoted_sensitive_key(raw, 0)
    {
        return false;
    }
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return true;
    }
    trimmed.starts_with('"') && !has_json_escaped_credential(raw)
}

/// Go `hasJSONEscapedCredential(s)`: the text between two consecutive quotes in `s`, read as a
/// JSON string, holds a credential pattern once decoded.
fn has_json_escaped_credential(s: &str) -> bool {
    if !s.contains('\\') {
        return false;
    }
    let mut open = s.find('"');
    while let Some(o) = open {
        let Some(n) = s[o + 1..].find('"') else {
            return false;
        };
        let closing = o + 1 + n;
        let quoted = &s[o..=closing];
        if quoted.contains('\\')
            && let Some(decoded) = go_json_unmarshal_string(quoted)
            && has_any_credential_marker(&decoded)
        {
            return true;
        }
        open = Some(closing);
    }
    false
}

/// Go `redactJSONTextWithBudget(s, depth, budget)`: redacts sensitive keys inside JSON-encoded
/// text (fail-closed: unparseable or over-budget JSON-like blobs become placeholders). Returns
/// `(sanitized, changed, content_lost)`.
pub(crate) fn redact_json_text_with_budget(
    s: &str,
    depth: usize,
    budget: &mut NodeBudget,
) -> (String, bool, bool) {
    let unparseable = || (UNPARSEABLE_JSON_PLACEHOLDER.to_owned(), true, true);
    let trimmed = trim_bom_and_space(s);
    if trimmed.is_empty() || is_known_sentinel(trimmed) {
        return (s.to_owned(), false, false);
    }
    if let Some(after_open_quote) = trimmed.strip_prefix('"') {
        if depth >= MAX_SANITIZE_DEPTH {
            return (MAX_DEPTH_PLACEHOLDER.to_owned(), true, true);
        }
        if !budget.take() {
            return (SANITIZE_BUDGET_PLACEHOLDER.to_owned(), true, true);
        }
        if trimmed.len() > MAX_INSPECTABLE_JSON_TEXT_BYTES {
            return unparseable();
        }
        let Ok((decoded, offset)) = scan_json_string(trimmed) else {
            let after_quote = trim_bom_and_space(after_open_quote);
            if after_quote.starts_with(['{', '[', '\\', '"']) || has_any_credential_marker(trimmed)
            {
                return unparseable();
            }
            return (s.to_owned(), false, false);
        };
        let suffix = &trimmed[offset..];
        let trimmed_suffix = trim_bom_and_space(suffix);
        if !trimmed_suffix.is_empty()
            && (suffix.contains(['{', '['])
                || trimmed_suffix.starts_with('\\')
                || has_any_credential_marker(suffix)
                || inspect_quoted_fragments(suffix, depth + 1, budget))
        {
            return unparseable();
        }
        let trimmed_inner = trim_bom_and_space(&decoded);
        let (inner_sanitized, inner_changed, inner_lost) =
            if trimmed_inner.starts_with(['{', '[', '"']) {
                redact_json_text_with_budget(&decoded, depth + 1, budget)
            } else if decoded.contains(['{', '['])
                || has_any_credential_marker(&decoded)
                || inspect_quoted_fragments(&decoded, depth + 1, budget)
            {
                return unparseable();
            } else {
                (String::new(), false, false)
            };
        if inner_changed
            && (inner_sanitized == UNPARSEABLE_JSON_PLACEHOLDER
                || inner_sanitized == MAX_DEPTH_PLACEHOLDER
                || inner_sanitized == SANITIZE_BUDGET_PLACEHOLDER)
        {
            return (inner_sanitized, true, true);
        }
        if !inner_changed {
            return (s.to_owned(), false, false);
        }
        let Some(b) = marshal(&JVal::Str(inner_sanitized)) else {
            return unparseable();
        };
        return (b + suffix, true, inner_lost);
    }

    if !trimmed.starts_with(['{', '[']) {
        return (s.to_owned(), false, false);
    }
    if depth >= MAX_SANITIZE_DEPTH {
        return (MAX_DEPTH_PLACEHOLDER.to_owned(), true, true);
    }
    if !budget.take() {
        return (SANITIZE_BUDGET_PLACEHOLDER.to_owned(), true, true);
    }
    if trimmed.len() > MAX_INSPECTABLE_JSON_TEXT_BYTES {
        return unparseable();
    }
    let (decoded, saw_duplicate_key) = match unmarshal_json_strict_with_budget(trimmed, budget) {
        Ok(v) => v,
        Err(DecodeFail::Budget) => return (SANITIZE_BUDGET_PLACEHOLDER.to_owned(), true, true),
        Err(DecodeFail::Syntax) => return unparseable(),
    };
    let (redacted, val_changed, val_lost) = redact_jval(&decoded, depth + 1, budget);
    if matches!(&redacted, JVal::Str(s) if s == SANITIZE_BUDGET_PLACEHOLDER) {
        return (SANITIZE_BUDGET_PLACEHOLDER.to_owned(), true, true);
    }
    if !val_changed && !saw_duplicate_key && !val_lost {
        return (s.to_owned(), false, false);
    }
    match marshal(&redacted) {
        Some(b) => (b, true, val_lost),
        None => unparseable(),
    }
}

/// Go `inspectQuotedFragments(s, depth, budget)`: a quoted fragment of `s` decodes to text that
/// needs redaction, or could not be decoded and starts like an encoded container.
fn inspect_quoted_fragments(s: &str, depth: usize, budget: &mut NodeBudget) -> bool {
    if depth >= MAX_SANITIZE_DEPTH {
        return true;
    }
    let b = s.as_bytes();
    let mut fragments = 0;
    // A quote before failed_until was read as an escaped quote by a decode that started at an
    // earlier quote and failed at failed_until; a decode starting at it would fail there too.
    let mut failed_until = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        fragments += 1;
        if fragments > MAX_QUOTED_FRAGMENTS_PER_INSPECT {
            return true;
        }
        if !budget.take() {
            return true;
        }
        let sub = &s[i..];
        if i < failed_until {
            if starts_like_encoded_container(sub) {
                return true;
            }
            i += 1;
            continue;
        }
        match scan_json_string(sub) {
            Err(err) => {
                match err {
                    JsonStrError::UnexpectedEof => failed_until = s.len(),
                    // Offset counts the byte that caused the error, which may itself be a
                    // quote that starts a decodable fragment.
                    JsonStrError::Syntax(offset) if offset > 1 => failed_until = i + offset - 1,
                    JsonStrError::Syntax(_) => {}
                }
                if starts_like_encoded_container(sub) {
                    return true;
                }
            }
            Ok((_, offset)) => {
                let (_, changed, lost) =
                    redact_json_text_with_budget(&sub[..offset], depth + 1, budget);
                if changed || lost {
                    return true;
                }
                if offset > 1 {
                    i += offset - 1;
                }
            }
        }
        i += 1;
    }
    false
}

/// Go `startsLikeEncodedContainer(s)`: `s`, a quoted fragment that could not be decoded,
/// starts like an encoded JSON container.
fn starts_like_encoded_container(s: &str) -> bool {
    let after_quote = s[1..].trim_start_matches(|r: char| is_space(r) || r == '\u{feff}');
    after_quote.starts_with(['{', '[', '\\', '"'])
}

/// Go `redactJSONValue(v, depth, budget)` over a decoded JSON value. Returns
/// `(out, changed, content_lost)`.
pub(crate) fn redact_json_value(
    v: &Value,
    depth: usize,
    budget: &mut NodeBudget,
) -> (Value, bool, bool) {
    let (out, changed, lost) = redact_jval(&JVal::from_value(v), depth, budget);
    (out.into_value(), changed, lost)
}

/// Go `redactJSONValue` over the decoder's values. Go walks a map in random order; this walks
/// it in key order (the outputs differ only when the node budget runs out inside a map).
fn redact_jval(v: &JVal, depth: usize, budget: &mut NodeBudget) -> (JVal, bool, bool) {
    let budget_placeholder = || JVal::Str(SANITIZE_BUDGET_PLACEHOLDER.to_owned());
    if depth >= MAX_SANITIZE_DEPTH {
        return (JVal::Str(MAX_DEPTH_PLACEHOLDER.to_owned()), true, true);
    }
    if !budget.take() {
        return (budget_placeholder(), true, true);
    }
    match v {
        JVal::Obj(m) => {
            let mut out = BTreeMap::new();
            let (mut changed, mut lost) = (false, false);
            for (k, val) in m {
                if budget.remaining <= 0 {
                    out.insert(SANITIZE_BUDGET_PLACEHOLDER.to_owned(), budget_placeholder());
                    return (JVal::Obj(out), true, true);
                }
                if is_sensitive_key(k) {
                    if !budget.take() {
                        out.insert(SANITIZE_BUDGET_PLACEHOLDER.to_owned(), budget_placeholder());
                        return (JVal::Obj(out), true, true);
                    }
                    if !matches!(val, JVal::Str(s) if s == REDACTED_VALUE) {
                        changed = true;
                    }
                    out.insert(k.clone(), JVal::Str(REDACTED_VALUE.to_owned()));
                    continue;
                }
                let (new_val, c, l) = redact_jval(val, depth + 1, budget);
                if matches!(&new_val, JVal::Str(s) if s == SANITIZE_BUDGET_PLACEHOLDER)
                    && budget.remaining <= 0
                {
                    out.insert(SANITIZE_BUDGET_PLACEHOLDER.to_owned(), budget_placeholder());
                    return (JVal::Obj(out), true, true);
                }
                out.insert(k.clone(), new_val);
                changed |= c;
                lost |= l;
            }
            (JVal::Obj(out), changed, lost)
        }
        JVal::Arr(xs) => {
            let mut out = Vec::with_capacity(xs.len().min(crate::formatter::MAX_CONTAINER_ALLOC));
            let (mut changed, mut lost) = (false, false);
            for val in xs {
                if budget.remaining <= 0 {
                    out.push(budget_placeholder());
                    return (JVal::Arr(out), true, true);
                }
                let (new_val, c, l) = redact_jval(val, depth + 1, budget);
                let exhausted = matches!(&new_val, JVal::Str(s) if s == SANITIZE_BUDGET_PLACEHOLDER)
                    && budget.remaining <= 0;
                out.push(new_val);
                changed |= c;
                lost |= l;
                if exhausted {
                    return (JVal::Arr(out), true, true);
                }
            }
            (JVal::Arr(out), changed, lost)
        }
        JVal::Str(x) => {
            let (s, c, l) = redact_json_text_with_budget(x, depth + 1, budget);
            (JVal::Str(s), c, l)
        }
        other => (other.clone(), false, false),
    }
}

/// Go `redactErrorMessage(msg)`: an error message with credentials scrubbed — full or embedded
/// JSON values, URIs with credentials, Authorization / API-key headers, bearer and basic
/// tokens, and sensitive `key=value` / `key: value` pairs.
pub(crate) fn redact_error_message(msg: &str) -> String {
    if msg.is_empty() || is_known_sentinel(msg) {
        return msg.to_owned();
    }
    let mut budget = NodeBudget::new();
    let trimmed = trim_bom_and_space(msg);
    if trimmed.starts_with(['{', '[']) {
        if let Ok((decoded, saw_duplicate_key)) =
            unmarshal_json_strict_with_budget(trimmed, &mut budget)
        {
            let (redacted, val_changed, val_lost) = redact_jval(&decoded, 1, &mut budget);
            let (scrubbed, str_changed) = scrub_error_json_strings(&redacted, 1);
            if (val_changed || saw_duplicate_key || val_lost || str_changed)
                && let Some(b) = marshal(&scrubbed)
            {
                return b;
            }
            return msg.to_owned();
        }
        // A malformed JSON document with a sensitive key pattern or a credential marker fails
        // closed rather than falling through to partial rewriting.
        if has_any_credential_marker(trimmed)
            || has_encoded_credential_marker(trimmed)
            || has_candidate_quoted_sensitive_key(trimmed, 0)
        {
            return UNPARSEABLE_JSON_PLACEHOLDER.to_owned();
        }
        return scrub_error_text(msg);
    }
    if trimmed.starts_with('"') {
        let (redacted, ok, _) = redact_json_text_with_budget(msg, 0, &mut budget);
        if ok {
            // As in sanitize_raw_text, quote-led prose that only failed JSON parsing is kept.
            if redacted == UNPARSEABLE_JSON_PLACEHOLDER
                && is_positively_harmless_prose(msg, trimmed)
            {
                return msg.to_owned();
            }
            return scrub_error_text(&redacted);
        }
    }
    scrub_error_text(msg)
}

/// Go `scrubErrorJSONStrings(v, depth)`: every string of a decoded value scrubbed with
/// [`scrub_error_text`].
fn scrub_error_json_strings(v: &JVal, depth: usize) -> (JVal, bool) {
    if depth >= MAX_SANITIZE_DEPTH {
        return (v.clone(), false);
    }
    match v {
        JVal::Obj(m) => {
            let mut changed = false;
            let mut out = BTreeMap::new();
            for (k, val) in m {
                let (new_val, c) = scrub_error_json_strings(val, depth + 1);
                out.insert(k.clone(), new_val);
                changed |= c;
            }
            (JVal::Obj(out), changed)
        }
        JVal::Arr(xs) => {
            let mut changed = false;
            let mut out = Vec::with_capacity(xs.len());
            for val in xs {
                let (new_val, c) = scrub_error_json_strings(val, depth + 1);
                out.push(new_val);
                changed |= c;
            }
            (JVal::Arr(out), changed)
        }
        JVal::Str(x) => {
            if is_known_sentinel(x) {
                return (v.clone(), false);
            }
            let scrubbed = scrub_error_text(x);
            let changed = scrubbed != *x;
            (JVal::Str(scrubbed), changed)
        }
        other => (other.clone(), false),
    }
}

/// Go `scrubErrorText(msg)`: URIs and credential text patterns redacted; the whole text
/// becomes [`REDACTED_SENSITIVE_TEXT_PLACEHOLDER`] when decoding escapes would expose one.
pub(crate) fn scrub_error_text(msg: &str) -> String {
    let scrubbed = scrub_error_tokens(msg);
    if has_encoded_credential_marker(&scrubbed) || has_backslash_escaped_credential(msg, &scrubbed)
    {
        return REDACTED_SENSITIVE_TEXT_PLACEHOLDER.to_owned();
    }
    scrubbed
}

/// Go `keptLiterals(scrubbed)`: the non-redacted pieces of `scrubbed` between redaction
/// placeholders, in order.
fn kept_literals(scrubbed: &str) -> Vec<String> {
    let mut parts = vec![scrubbed.to_owned()];
    let escaped_redacted = go_query_escape(REDACTED_VALUE);
    for sentinel in [
        REDACTED_URI_PLACEHOLDER,
        REDACTED_VALUE,
        escaped_redacted.as_str(),
    ] {
        parts = parts
            .iter()
            .flat_map(|part| part.split(sentinel))
            .filter(|piece| !piece.is_empty())
            .map(str::to_owned)
            .collect();
    }
    parts
}

/// Go `keptInOrder(parts, s)`: every part appears in `s`, in order.
fn kept_in_order(parts: &[String], s: &str) -> bool {
    let mut rest = s;
    for part in parts {
        let Some(i) = rest.find(part.as_str()) else {
            return false;
        };
        rest = &rest[i + part.len()..];
    }
    true
}

/// Go `unescapeCredentialStep(s)`.
fn unescape_credential_step(s: &str) -> String {
    let mut s = s.to_owned();
    if s.contains('\\') {
        s = unescape_backslash_controls(&s);
    }
    if s.contains(['%', 'x', 'X', 'u', 'U']) {
        s = canonicalize_credential_marker(&s);
    }
    s
}

/// Go `hasBackslashEscapedCredential(raw, scrubbed)`: unescaping backslash escapes, or
/// canonicalizing percent, `\x` or `\u` escapes, exposes a credential that
/// [`scrub_error_tokens`] or [`has_encoded_credential_marker`] catches; fails closed past
/// `2 * MAX_CANONICAL_UNESCAPE_PASSES` passes.
fn has_backslash_escaped_credential(raw: &str, scrubbed: &str) -> bool {
    let mut parts: Option<Vec<String>> = None;
    let mut cur_raw = raw.to_owned();
    let mut cur_scrubbed = scrubbed.to_owned();
    for _ in 0..2 * MAX_CANONICAL_UNESCAPE_PASSES {
        if !cur_raw.contains(['\\', '%']) {
            return false;
        }
        let next_raw = unescape_credential_step(&cur_raw);
        if next_raw == cur_raw {
            return false;
        }
        if cur_scrubbed == cur_raw {
            // Nothing was redacted, so the scrubbed text decodes to next_raw too.
            if scrub_error_tokens(&next_raw) != next_raw || has_encoded_credential_marker(&next_raw)
            {
                return true;
            }
            cur_scrubbed = next_raw.clone();
            cur_raw = next_raw;
            continue;
        }
        let kept = parts.get_or_insert_with(|| kept_literals(&cur_scrubbed));
        let next_scrubbed = unescape_credential_step(&cur_scrubbed);
        for part in kept.iter_mut() {
            *part = unescape_credential_step(part);
        }
        if scrub_error_tokens(&next_scrubbed) != next_scrubbed
            || has_encoded_credential_marker(&next_scrubbed)
        {
            return true;
        }
        if !kept_in_order(kept, &scrub_error_tokens(&next_raw)) {
            return true;
        }
        cur_raw = next_raw;
        cur_scrubbed = next_scrubbed;
    }
    unescape_credential_step(&cur_raw) != cur_raw
}

/// Go `unescapeBackslashControls(s)`: the standard backslash escapes decoded; a run of
/// backslashes is decoded in one step as the escape its last backslash starts.
fn unescape_backslash_controls(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j] == b'\\' {
            j += 1;
        }
        if j == b.len() {
            out.push(b'\\');
            break;
        }
        match b[j] {
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            c @ (b'/' | b'"' | b'\'') => out.push(c),
            c => {
                out.push(b'\\');
                out.push(c);
            }
        }
        i = j + 1;
    }
    lossy(out)
}

/// Go `scrubErrorTokens(msg)`: URIs sanitized with [`sanitize_uri`], then the text patterns.
fn scrub_error_tokens(msg: &str) -> String {
    let msg = if msg.contains("://") {
        URI_TOKEN_RE
            .replace_all(msg, |caps: &regex::Captures<'_>| {
                let token = caps.get(0).map_or("", |m| m.as_str());
                let trimmed = trim_trailing_punctuation(token);
                let suffix = &token[trimmed.len()..];
                let (sanitized, changed) = sanitize_uri(trimmed);
                if !changed {
                    return token.to_owned();
                }
                sanitized + suffix
            })
            .into_owned()
    } else {
        msg.to_owned()
    };
    scrub_text_patterns(&msg)
}

/// Go `trimTrailingPunctuation(s)`: `s` without the punctuation that may follow a URI or a
/// value in text; a `]` that closes a redaction placeholder is kept.
fn trim_trailing_punctuation(s: &str) -> &str {
    let trimmed = s.trim_end_matches(|c: char| ".,;:)]}".contains(c));
    let with_bracket = &s[..(trimmed.len() + 1).min(s.len())];
    if with_bracket.ends_with(']')
        && let Some(open) = trimmed.rfind('[')
        && is_known_sentinel(&with_bracket[open..])
    {
        return with_bracket;
    }
    trimmed
}

/// Go `scrubTextPatterns(s)`: Authorization and API-key headers, bearer and basic credentials,
/// and the values of sensitive key-value pairs redacted.
fn scrub_text_patterns(s: &str) -> String {
    if !may_match_text_patterns(s) {
        return s.to_owned();
    }
    let s = AUTH_HEADER_RE
        .replace_all(s, format!("${{1}}: {REDACTED_VALUE}").as_str())
        .into_owned();
    let s = replace_matches(&s, &BEARER_TOKEN_RE, redact_bearer_token);
    let s = replace_matches(&s, &BASIC_TOKEN_RE, redact_basic_credential);
    let s = replace_matches(&s, &JSON_KEY_VAL_TOKEN_RE, redact_json_key_value);
    let mut brackets = OpenBrackets::default();
    replace_matches_func(&s, find_key_val_token, |s, loc| {
        redact_text_key_value(s, loc, &mut brackets)
    })
}

/// Go `findKeyValToken(s)`.
fn find_key_val_token(s: &str) -> Option<Vec<usize>> {
    submatch_index(&TEXT_KEY_VAL_TOKEN_AT_RE, s)
        .or_else(|| submatch_index(&TEXT_KEY_VAL_TOKEN_RE, s))
}

/// Go `replaceMatches(s, re, repl)`.
fn replace_matches(
    s: &str,
    re: &Regex,
    repl: impl FnMut(&str, &[usize]) -> (String, usize),
) -> String {
    replace_matches_func(s, |t| submatch_index(re, t), repl)
}

/// Go `replaceMatchesFunc(s, find, repl)`: each match replaced with what `repl` returns, which
/// also says where the replaced text ends (possibly past the match); the search resumes there,
/// on the rest of `s` (so `^` and `\b` see the rest as a new text, as in Go).
fn replace_matches_func(
    s: &str,
    find: impl Fn(&str) -> Option<Vec<usize>>,
    mut repl: impl FnMut(&str, &[usize]) -> (String, usize),
) -> String {
    let mut b = String::new();
    let (mut last, mut pos) = (0, 0);
    while pos < s.len() {
        let Some(mut loc) = find(&s[pos..]) else {
            break;
        };
        for v in &mut loc {
            if *v != NO_MATCH {
                *v += pos;
            }
        }
        let (replacement, end) = repl(s, &loc);
        b.push_str(&s[last..loc[0]]);
        b.push_str(&replacement);
        last = end;
        pos = end.max(ceil_boundary(s, loc[0] + 1));
    }
    if last == 0 {
        return s.to_owned();
    }
    b.push_str(&s[last..]);
    b
}

/// Go `redactBearerToken(s, loc)`: a `bearerTokenRe` match replaced; "bearer of" followed by a
/// space is prose.
fn redact_bearer_token(s: &str, loc: &[usize]) -> (String, usize) {
    let end = loc[5];
    let value = &s[loc[4]..end];
    let rest = &s[loc[4]..];
    if rest.len() >= 2 && rest.as_bytes()[..2].eq_ignore_ascii_case(b"of") {
        let next = rest[2..].chars().next();
        if rest.len() == 2 || next.is_some_and(is_space) {
            return (s[loc[0]..end].to_owned(), end);
        }
    }
    if is_redacted_value(unquote_value(value).0) {
        return (s[loc[0]..end].to_owned(), end);
    }
    (format!("{} {REDACTED_VALUE}", &s[loc[2]..loc[3]]), end)
}

/// Go `redactBasicCredential(s, loc)`: a `basicTokenRe` match replaced when it holds a basic
/// credential (adk-python's test: valid base64 not running on into more base64 characters,
/// padded or decoding to `user:password`; an over-long token is always redacted).
fn redact_basic_credential(s: &str, loc: &[usize]) -> (String, usize) {
    let end = loc[5];
    let encoded = &s[loc[4]..end];
    if end < s.len() && is_base64_byte(s.as_bytes()[end]) {
        return (s[loc[0]..end].to_owned(), end);
    }
    if encoded.len() <= MAX_BASIC_CREDENTIAL_BYTES {
        match decode_std_base64(encoded) {
            Some(decoded) if encoded.contains('=') || decoded.contains(&b':') => {}
            _ => return (s[loc[0]..end].to_owned(), end),
        }
    }
    (format!("{} {REDACTED_VALUE}", &s[loc[2]..loc[3]]), end)
}

/// Go `isBase64Byte(c)`.
fn is_base64_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'='
}

/// Go `base64.StdEncoding.DecodeString(s)` for the text `basicTokenRe` matches (base64
/// characters, then at most two `=`): `None` when Go reports an error (a length that is not a
/// multiple of 4, or a character outside the alphabet, such as the U+017F or U+212A that the
/// pattern's `(?i)` admits). Non-zero trailing bits are accepted, as Go's non-strict decoder
/// does.
pub(crate) fn decode_std_base64(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return None;
    }
    let data = s.trim_end_matches('=');
    if b.len() - data.len() > 2 || data.contains('=') {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in data.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Go `redactJSONKeyValue(s, loc)`: a `jsonKeyValTokenRe` match replaced with
/// `"key":"[REDACTED]"`.
fn redact_json_key_value(s: &str, loc: &[usize]) -> (String, usize) {
    let end = sensitive_value_end(s, loc[4], loc[5]);
    if is_redacted_value(unquote_value(&s[loc[4]..end]).0) {
        return (s[loc[0]..end].to_owned(), end);
    }
    (
        format!("\"{}\":\"{REDACTED_VALUE}\"", &s[loc[2]..loc[3]]),
        end,
    )
}

/// Go `redactTextKeyValue(s, loc, brackets)`: a `textKeyValTokenRe` match replaced, keeping the
/// key, its quote, the delimiter and the quotes around the value.
fn redact_text_key_value(s: &str, loc: &[usize], brackets: &mut OpenBrackets) -> (String, usize) {
    let open = brackets.before(s, loc[0]);
    let (start, mut end) = (loc[8], loc[9]);
    let keep = |quote: &str| {
        format!(
            "{}{}{}{quote}{REDACTED_VALUE}{quote}",
            &s[loc[2]..loc[3]],
            &s[loc[4]..loc[5]],
            &s[loc[6]..loc[7]]
        )
    };
    match nests_sensitive_key(s, start, end, starts_line(s, loc[7], start)) {
        (NestedKeyKind::NestedKey, _) => return (s[loc[0]..start].to_owned(), start),
        (NestedKeyKind::ValueEndingInKeyName, pair) => {
            let quote = unquote_value(&s[start..end]).1;
            end = nested_pair_end(s, pair, open);
            brackets.skip(end);
            return (keep(quote), end);
        }
        (NestedKeyKind::NotNestedKey, _) => {}
    }
    end = text_value_end(s, start, end, open);
    brackets.skip(end);
    if end == start {
        // Only closing brackets follow the delimiter, as in {Password:}.
        return (s[loc[0]..end].to_owned(), end);
    }
    let (inner, quote) = unquote_value(&s[start..end]);
    if is_redacted_value(inner) {
        return (s[loc[0]..end].to_owned(), end);
    }
    (keep(quote), end)
}

/// Go `textValueEnd(s, start, end, open)`: where the value matched at `s[start..end]` ends.
fn text_value_end(s: &str, start: usize, end: usize, open: usize) -> usize {
    if !unquote_value(&s[start..end]).1.is_empty() {
        return end;
    }
    // rest is where the text after a JSON object or array value starts.
    let rest = sensitive_value_end(s, start, start);
    rest + trim_enclosing_closers(&s[rest..end.max(rest)], open).len()
}

/// Go `startsLine(s, delim, start)`: the white space between the delimiter (ending at `delim`)
/// and the value at `start` holds a line break.
fn starts_line(s: &str, delim: usize, start: usize) -> bool {
    s[delim..start].contains(['\r', '\n'])
}

/// Go `nestedKeyKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedKeyKind {
    /// A value, which is redacted.
    NotNestedKey,
    /// A sensitive key, left in place so the pair it starts is matched on its own.
    NestedKey,
    /// A value that only ends in a sensitive key name, redacted together with the value of the
    /// pair it starts.
    ValueEndingInKeyName,
}

/// Go `nestsSensitiveKey(s, start, end, lineStart)`: whether the value matched at
/// `s[start..end]` is a sensitive key rather than a value, and where the key name in it
/// starts.
fn nests_sensitive_key(
    s: &str,
    start: usize,
    end: usize,
    line_start: bool,
) -> (NestedKeyKind, usize) {
    let not = (NestedKeyKind::NotNestedKey, 0);
    if end - start > MAX_NESTED_KEY_LEN
        || s.as_bytes()[start] == b'{'
        || s.as_bytes()[start] == b'['
    {
        return not;
    }
    let v = &s[start..end];
    let (inner, quote) = unquote_value(v);
    if !quote.is_empty() {
        if !value_follows_delimiter(s, end) {
            return not;
        }
        return nested_key_name(inner, start + quote.len(), line_start);
    }
    if v.ends_with([':', '=']) {
        if !value_starts_at(s, end) {
            return not;
        }
        return nested_key_name(&v[..v.len() - 1], start, line_start);
    }
    if followed_by_delimiter(s, end) {
        if !value_follows_delimiter(s, end) {
            return not;
        }
        return nested_key_name(v, start, line_start);
    }
    // A name and a delimiter followed by a quoted value that holds white space, ",", ";" or
    // "&", as in credentials: password="a b", where the value matched ends inside it.
    if let Some(q) = v.find(['"', '\'', '\\'])
        && q > 0
        && (v.as_bytes()[q - 1] == b':' || v.as_bytes()[q - 1] == b'=')
        && QUOTED_VALUE_RE
            .find(&s[start + q..])
            .is_some_and(|m| m.len() > end - start - q)
        && value_starts_at(s, start + q)
    {
        let found = nested_key_name(&v[..q - 1], start, line_start);
        if found.0 != NestedKeyKind::NotNestedKey {
            return found;
        }
    }
    // A pair that an earlier pass redacted, as in "token:[REDACTED]", or token:"[REDACTED]"} in
    // the %+v output of a struct.
    let Some(i) = v.rfind([':', '=']).filter(|&i| i > 0) else {
        return not;
    };
    if !is_redacted_value(unquote_value(trim_trailing_punctuation(&v[i + 1..])).0) {
        return not;
    }
    nested_key_name(&v[..i], start, line_start)
}

/// Go `nestedKeyName(name, at, lineStart)`.
fn nested_key_name(name: &str, at: usize, line_start: bool) -> (NestedKeyKind, usize) {
    if SENSITIVE_KEYS.contains(&name) {
        return (NestedKeyKind::NestedKey, at);
    }
    if let Some(m) = submatch_index(&NESTED_KEY_NAME_RE, name) {
        return (NestedKeyKind::NestedKey, at + m[2]);
    }
    match submatch_index(&COMPOUND_KEY_NAME_RE, name) {
        None => (NestedKeyKind::NotNestedKey, 0),
        Some(m) if line_start => (NestedKeyKind::NestedKey, at + m[2]),
        Some(m) => (NestedKeyKind::ValueEndingInKeyName, at + m[2]),
    }
}

/// Go `nestedPairEnd(s, pair, open)`: where the text redacted with a value that only ends in a
/// key name ends — at the end of the value of the pair that the key name starts, following
/// chains of such pairs.
pub(crate) fn nested_pair_end(s: &str, mut pair: usize, open: usize) -> usize {
    loop {
        let Some(loc) = submatch_index(&TEXT_KEY_VAL_TOKEN_AT_RE, &s[pair..]) else {
            // Not reached: nests_sensitive_key has read a delimiter and a value after the name.
            return pair;
        };
        let (start, end) = (pair + loc[8], pair + loc[9]);
        let (kind, next) = nests_sensitive_key(s, start, end, starts_line(s, pair + loc[7], start));
        if kind == NestedKeyKind::NotNestedKey {
            return text_value_end(s, start, end, open);
        }
        pair = next;
    }
}

/// Go `followedByDelimiter(s, i)`: `:` or `=` follows `s[..i]`, possibly after white space.
fn followed_by_delimiter(s: &str, i: usize) -> bool {
    let rest = s[i..].trim_start_matches(|c: char| DELIM_SPACE.contains(c));
    rest.starts_with([':', '='])
}

/// Go `valueFollowsDelimiter(s, i)`: a delimiter follows `s[..i]` and a value starts after it.
fn value_follows_delimiter(s: &str, i: usize) -> bool {
    let rest = s[i..].trim_start_matches(|c: char| DELIM_SPACE.contains(c));
    rest.starts_with([':', '=']) && value_starts_at(rest, 1)
}

/// Go `valueStartsAt(s, i)`: a value that can be redacted starts in `s` at `i`, possibly after
/// white space.
fn value_starts_at(s: &str, i: usize) -> bool {
    let trim_set = |c: char| DELIM_SPACE.contains(c) || CLOSING_PUNCTUATION.contains(c);
    let mut rest = s[i..].trim_start_matches(|c: char| DELIM_SPACE.contains(c));
    if rest.starts_with(['"', '\'', '\\'])
        && let Some(q) = QUOTED_VALUE_RE.find(rest)
        && !q.as_str().is_empty()
        && quoted_value_ends(rest, q.end())
    {
        return !unquote_value(q.as_str())
            .0
            .trim_matches(trim_set)
            .is_empty();
    }
    if let Some(n) = rest.find(|c: char| REGEXP_SPACE.contains(c) || ",;&".contains(c)) {
        rest = &rest[..n];
    }
    !rest.trim_matches(trim_set).is_empty()
}

/// Go `quotedValueEnds(s, n)`: the quoted value `s[..n]` ends there — `s` ends, or goes on with
/// a rune that is not a letter, a digit, `_` or the value's quote.
fn quoted_value_ends(s: &str, n: usize) -> bool {
    let after = &s[n..];
    if after.is_empty() {
        return true;
    }
    if WORD_RUNE_START_RE.is_match(after) {
        return false;
    }
    let quote = unquote_value(&s[..n]).1;
    !after.starts_with(quote)
}

/// Go `openBrackets`: the brackets (`{` or `[`) open in a text read from its start.
#[derive(Debug, Default)]
struct OpenBrackets {
    /// `s[..pos]` has been read or skipped.
    pos: usize,
    depth: usize,
}

impl OpenBrackets {
    /// Go `before(s, i)`: reads `s` up to `i` and returns the brackets open there.
    fn before(&mut self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        while self.pos < i {
            match b[self.pos] {
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            self.pos += 1;
        }
        self.depth
    }

    /// Go `skip(i)`: moves past `s[..i]` without reading it.
    fn skip(&mut self, i: usize) {
        self.pos = self.pos.max(i);
    }
}

/// Go `trimEnclosingClosers(v, open)`: `v` without the `}` and `]` at its end that close
/// brackets opened before it (at most `open`), and the punctuation after them.
fn trim_enclosing_closers(v: &str, open: usize) -> &str {
    let tail = v
        .trim_end_matches(|c: char| CLOSING_PUNCTUATION.contains(c))
        .len();
    let depth = OpenBrackets::default().before(v, tail);
    let closers: Vec<usize> = v.as_bytes()[tail..]
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == b'}' || **c == b']')
        .map(|(i, _)| tail + i)
        .collect();
    // The first depth closers close brackets opened in v; at most open of the others close
    // brackets opened before it.
    let j = depth.max(closers.len().saturating_sub(open));
    match closers.get(j) {
        Some(&at) => &v[..at],
        None => v,
    }
}

/// Go `sensitiveValueEnd(s, start, end)`: a value that opens a JSON object or array is
/// extended to its closing bracket (or the end of `s`); brackets inside strings, including
/// strings delimited by escaped quotes, do not count.
fn sensitive_value_end(s: &str, start: usize, end: usize) -> usize {
    let b = s.as_bytes();
    if b[start] != b'{' && b[start] != b'[' {
        return end;
    }
    let mut depth = 0i64;
    // quote is the quote of the string being skipped; escaped reports that it was opened with
    // an escaped quote, which only the same escaped quote closes.
    let mut quote = 0u8;
    let mut escaped = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if escaped {
            if c == b'\\' && i + 1 != b.len() {
                match b[i + 1] {
                    n if n == quote => {
                        quote = 0;
                        escaped = false;
                        i += 1;
                    }
                    b'\\' => {
                        // An escape of the inner text; the escaped byte may itself be escaped.
                        i += 2;
                        if i < b.len() && b[i] == b'\\' {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
        } else if c == b'\\' {
            if quote == 0 && i + 1 < b.len() && (b[i + 1] == b'"' || b[i + 1] == b'\'') {
                quote = b[i + 1];
                escaped = true;
            }
            i += 1;
        } else if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'{' || c == b'[' {
            depth += 1;
        } else if c == b'}' || c == b']' {
            depth -= 1;
            if depth == 0 {
                return end.max(i + 1);
            }
        }
        i += 1;
    }
    b.len()
}

/// Go `unquoteValue(v)`: one pair of quotes (`\"`, `\'`, `"` or `'`) stripped from a value;
/// returns the content and the quote (`""` when unquoted).
fn unquote_value(v: &str) -> (&str, &'static str) {
    for q in ["\\\"", "\\'", "\"", "'"] {
        if v.len() >= 2 * q.len() && v.starts_with(q) && v.ends_with(q) {
            return (&v[q.len()..v.len() - q.len()], q);
        }
    }
    (v, "")
}

/// Go `isRedactedMarker(v)`: `v`, possibly percent-encoded, is a placeholder.
fn is_redacted_marker(v: &str) -> bool {
    if is_known_sentinel(v) {
        return true;
    }
    match go_url::path_unescape(v) {
        Ok(decoded) if decoded != v.as_bytes() => {
            std::str::from_utf8(&decoded).is_ok_and(is_known_sentinel)
        }
        _ => false,
    }
}

/// Go `isRedactedValue(v)`: a placeholder, possibly followed by sentence punctuation.
fn is_redacted_value(v: &str) -> bool {
    is_redacted_marker(v) || is_redacted_marker(trim_trailing_punctuation(v))
}

/// Go `hasEncodedCredentialMarker(msg)`.
fn has_encoded_credential_marker(msg: &str) -> bool {
    exposes_encoded_pattern(msg, scrub_error_tokens)
}

/// Go `exposesEncodedPattern(msg, scrub)`: decoding the escapes in `msg` exposes a pattern that
/// `scrub` would redact.
fn exposes_encoded_pattern(msg: &str, scrub: fn(&str) -> String) -> bool {
    if msg.is_empty() || is_known_sentinel(msg) {
        return false;
    }
    let cleaned = msg
        .replace("%5BREDACTED%5D", REDACTED_VALUE)
        .replace("%5bredacted%5d", REDACTED_VALUE);
    let canon = canonicalize_credential_marker(&cleaned);
    if canon == cleaned {
        return false;
    }
    scrub(&canon) != canon
}

/// Go `hasAnyCredentialMarker(s)`.
fn has_any_credential_marker(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if scrub_error_text(s) != s {
        return true;
    }
    if has_candidate_quoted_sensitive_key(s, 0) {
        return true;
    }
    let canon = canonicalize_credential_marker(s);
    if has_quoted_sensitive_key(&canon) {
        return true;
    }
    canon != s && scrub_error_tokens(&canon) != canon
}

/// The panic value's Go `%T` as far as Rust can tell: a `&str` or `String` payload (Go
/// `panic("...")`) is `string`.
fn panic_type_name(payload: &(dyn std::any::Any + Send)) -> &'static str {
    if payload.is::<&str>() || payload.is::<String>() {
        "string"
    } else {
        "unknown"
    }
}

/// The `%T` written for a formatter error. Divergence (ledger 7b-1): Rust has no runtime type
/// name for a `Box<dyn Error>`, so every error is `error` (Go writes its dynamic type, such as
/// `*errors.errorString`).
const FORMATTER_ERROR_TYPE: &str = "error";

/// Go `applyContentFormatter(formatter, logger, payload, eventType, debugErrors)`: runs the
/// formatter fail-closed. Returns `(result, err_note, failed)`; on an error or a panic the
/// result is [`FORMATTING_FAILED_PLACEHOLDER`] (as a JSON string) and `failed` is true. Raw
/// error strings and panic values are never logged.
pub(crate) fn apply_content_formatter(
    formatter: Option<&ContentFormatter>,
    logger: Option<&dyn Logger>,
    payload: &Value,
    event_type: &str,
    debug_errors: bool,
) -> (Value, String, bool) {
    let Some(formatter) = formatter else {
        return (payload.clone(), String::new(), false);
    };
    let failed = || Value::String(FORMATTING_FAILED_PLACEHOLDER.to_owned());
    // Divergence (ledger 7b-1): Go's recover is silent; Rust's panic hook still reports the
    // panic (by default on stderr) before catch_unwind returns.
    match catch_unwind(AssertUnwindSafe(|| formatter(payload, event_type))) {
        Err(panic) => {
            if let Some(logger) = logger {
                logger.printf(format_args!(
                    "agentanalytics: ContentFormatter panicked for event {event_type} (type={}); content replaced",
                    panic_type_name(panic.as_ref())
                ));
            }
            (failed(), "content_formatter panicked".to_owned(), true)
        }
        Ok(Err(err)) => {
            if let Some(logger) = logger {
                if debug_errors {
                    logger.printf(format_args!(
                        "agentanalytics: ContentFormatter failed for event {event_type} (type={FORMATTER_ERROR_TYPE}, err_len={}); content replaced",
                        err.to_string().len()
                    ));
                } else {
                    logger.printf(format_args!(
                        "agentanalytics: ContentFormatter failed for event {event_type} ({FORMATTER_ERROR_TYPE}); content replaced"
                    ));
                }
            }
            (
                failed(),
                format!("content_formatter raised {FORMATTER_ERROR_TYPE}"),
                true,
            )
        }
        Ok(Ok(formatted)) => (formatted, String::new(), false),
    }
}

/// Go `parsePayloadColumnDenylist(cols)`: validates `cols` and returns them as a set.
pub(crate) fn parse_payload_column_denylist(cols: &[String]) -> Result<BTreeSet<String>, Error> {
    let mut denied = BTreeSet::new();
    for c in cols {
        match c.as_str() {
            COLUMN_CONTENT | COLUMN_CONTENT_PARTS | COLUMN_ATTRIBUTES | COLUMN_LATENCY_MS => {
                denied.insert(c.clone());
            }
            _ => {
                return Err(Error::msg(format!(
                    "unsupported PayloadColumnDenylist entry {}: allowed values are {}, {}, {} and {}",
                    go_quote(c),
                    go_quote(COLUMN_CONTENT),
                    go_quote(COLUMN_CONTENT_PARTS),
                    go_quote(COLUMN_ATTRIBUTES),
                    go_quote(COLUMN_LATENCY_MS)
                )));
            }
        }
    }
    Ok(denied)
}

/// Go `customMetadataFilter`: selects custom-metadata keys by exact name or by prefix (entries
/// ending in `*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CustomMetadataFilter {
    pub(crate) exact: BTreeSet<String>,
    pub(crate) prefixes: Vec<String>,
}

impl CustomMetadataFilter {
    /// Go `newCustomMetadataFilter(allowlist)`.
    pub(crate) fn new(allowlist: &[String]) -> CustomMetadataFilter {
        let mut f = CustomMetadataFilter::default();
        for entry in allowlist {
            match entry.strip_suffix('*') {
                Some(prefix) => f.prefixes.push(prefix.to_owned()),
                None => {
                    f.exact.insert(entry.clone());
                }
            }
        }
        f
    }

    /// Go `allows(key)`.
    pub(crate) fn allows(&self, key: &str) -> bool {
        self.exact.contains(key) || self.prefixes.iter().any(|p| key.starts_with(p.as_str()))
    }

    /// Go `apply(md)`: the allowed entries, or `None` when there are none.
    pub(crate) fn apply(&self, md: &Map<String, Value>) -> Option<Map<String, Value>> {
        let out: Map<String, Value> = md
            .iter()
            .filter(|(k, _)| self.allows(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        (!out.is_empty()).then_some(out)
    }
}

// ---------------------------------------------------------------------------------------------
// Go encoding/json, as redaction.go uses it.
// ---------------------------------------------------------------------------------------------

/// A value Go's `decodeJSONValue` builds (`map[string]any`, `[]any`, `string`, `json.Number`,
/// `bool`, `nil`), or one `redactJSONValue` receives from a caller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JVal {
    Null,
    Bool(bool),
    Num(JNum),
    Str(String),
    Arr(Vec<JVal>),
    Obj(BTreeMap<String, JVal>),
}

/// A JSON number: Go's `json.Number` (the literal text, written back as is) or a caller's
/// `serde_json::Number`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JNum {
    Text(String),
    Value(Number),
}

impl JVal {
    fn from_value(v: &Value) -> JVal {
        match v {
            Value::Null => JVal::Null,
            Value::Bool(b) => JVal::Bool(*b),
            Value::Number(n) => JVal::Num(JNum::Value(n.clone())),
            Value::String(s) => JVal::Str(s.clone()),
            Value::Array(xs) => JVal::Arr(xs.iter().map(JVal::from_value).collect()),
            Value::Object(m) => JVal::Obj(
                m.iter()
                    .map(|(k, v)| (k.clone(), JVal::from_value(v)))
                    .collect(),
            ),
        }
    }

    fn into_value(self) -> Value {
        match self {
            JVal::Null => Value::Null,
            JVal::Bool(b) => Value::Bool(b),
            JVal::Num(JNum::Value(n)) => Value::Number(n),
            JVal::Num(JNum::Text(t)) => t.parse::<Number>().map_or(Value::String(t), Value::Number),
            JVal::Str(s) => Value::String(s),
            JVal::Arr(xs) => Value::Array(xs.into_iter().map(JVal::into_value).collect()),
            JVal::Obj(m) => {
                Value::Object(m.into_iter().map(|(k, v)| (k, v.into_value())).collect())
            }
        }
    }
}

/// Go `json.Marshal(v)` of a [`JVal`]: compact, map keys sorted, strings escaped as Go does;
/// `None` where Go reports an error.
fn marshal(v: &JVal) -> Option<String> {
    let mut out = String::new();
    write_json(v, &mut out)?;
    Some(out)
}

fn write_json(v: &JVal, out: &mut String) -> Option<()> {
    match v {
        JVal::Null => out.push_str("null"),
        JVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JVal::Num(JNum::Text(t)) => out.push_str(t),
        JVal::Num(JNum::Value(n)) => {
            out.push_str(&String::from_utf8(gadk_model::to_go_json(n).ok()?).ok()?);
        }
        JVal::Str(s) => write_json_string(s, out)?,
        JVal::Arr(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(x, out)?;
            }
            out.push(']');
        }
        JVal::Obj(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out)?;
                out.push(':');
                write_json(x, out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

/// Go's encoding of a JSON string (`<`, `>`, `&`, U+2028 and U+2029 escaped).
fn write_json_string(s: &str, out: &mut String) -> Option<()> {
    out.push_str(&String::from_utf8(gadk_model::to_go_json(s).ok()?).ok()?);
    Some(())
}

/// Why a JSON string could not be decoded (Go `io.ErrUnexpectedEOF` or a `*json.SyntaxError`
/// with its `Offset`: the index of the offending byte plus one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonStrError {
    UnexpectedEof,
    Syntax(usize),
}

/// Go `json.NewDecoder(strings.NewReader(s)).Decode(&str)` for an `s` that starts with `"`:
/// the decoded string and `dec.InputOffset()` (the index after the closing quote). Invalid
/// `\u` surrogates decode to U+FFFD, as Go's `unquote` does.
fn scan_json_string(s: &str) -> Result<(String, usize), JsonStrError> {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 1;
    loop {
        let Some(&c) = b.get(i) else {
            return Err(JsonStrError::UnexpectedEof);
        };
        match c {
            b'"' => return Ok((out, i + 1)),
            b'\\' => {
                let Some(&e) = b.get(i + 1) else {
                    return Err(JsonStrError::UnexpectedEof);
                };
                match e {
                    b'"' | b'\\' | b'/' => out.push(char::from(e)),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let r = read_hex4(b, i + 2)?;
                        i += 6;
                        if (0xd800..0xe000).contains(&r) {
                            // Go: a valid pair is consumed, else U+FFFD (the next escape stays).
                            let pair = (0xd800..0xdc00)
                                .contains(&r)
                                .then(|| get_u4(b, i))
                                .flatten()
                                .filter(|r2| (0xdc00..0xe000).contains(r2));
                            match pair {
                                Some(r2) => {
                                    let cp = 0x10000 + ((r - 0xd800) << 10) + (r2 - 0xdc00);
                                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                    i += 6;
                                }
                                None => out.push('\u{fffd}'),
                            }
                        } else {
                            out.push(char::from_u32(r).unwrap_or('\u{fffd}'));
                        }
                        continue;
                    }
                    _ => return Err(JsonStrError::Syntax(i + 2)),
                }
                i += 2;
            }
            c if c < 0x20 => return Err(JsonStrError::Syntax(i + 1)),
            _ => {
                let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
}

/// The 4 hex digits of a `\u` escape at `b[at..at + 4]`, scanned as Go's scanner does.
fn read_hex4(b: &[u8], at: usize) -> Result<u32, JsonStrError> {
    let mut r = 0u32;
    for k in 0..4 {
        let Some(&h) = b.get(at + k) else {
            return Err(JsonStrError::UnexpectedEof);
        };
        let Some(d) = char::from(h).to_digit(16) else {
            return Err(JsonStrError::Syntax(at + k + 1));
        };
        r = r * 16 + d;
    }
    Ok(r)
}

/// Go `getu4(s[i:])`: the value of a `\uXXXX` escape at `b[i..]`, if there is one.
fn get_u4(b: &[u8], i: usize) -> Option<u32> {
    if b.get(i) != Some(&b'\\') || b.get(i + 1) != Some(&b'u') {
        return None;
    }
    read_hex4(b, i + 2).ok()
}

/// Go `json.Unmarshal([]byte(s), &str)`: `s` must be exactly one JSON string (plus JSON white
/// space).
fn go_json_unmarshal_string(s: &str) -> Option<String> {
    if !s.starts_with('"') {
        return None;
    }
    let (decoded, end) = scan_json_string(s).ok()?;
    s.as_bytes()[end..]
        .iter()
        .all(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        .then_some(decoded)
}

/// How [`unmarshal_json_strict_with_budget`] failed: Go's `budgetExceeded`, or another error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeFail {
    Budget,
    Syntax,
}

/// Go `unmarshalJSONStrictWithBudget(b, budget)`: the decoded value and whether an object held
/// a duplicate key (the last value wins).
pub(crate) fn unmarshal_json_strict_with_budget(
    s: &str,
    budget: &mut NodeBudget,
) -> Result<(JVal, bool), DecodeFail> {
    let mut dec = StrictDecoder {
        b: s.as_bytes(),
        s,
        pos: 0,
    };
    let v = dec.value(0, Delim::None, budget)?;
    dec.skip_space();
    if dec.pos != dec.b.len() {
        // Go: "unexpected data after JSON value".
        return Err(DecodeFail::Syntax);
    }
    Ok(v)
}

/// The delimiter Go's `Decoder.Token` consumes before a value: `,` before an array element
/// after the first, `:` before an object member's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    None,
    Comma,
    Colon,
}

/// Go's `decodeJSONValue` over a `json.Decoder` with `UseNumber`: the `Decoder.Token` grammar
/// (strict JSON; white space is space, tab, CR and LF), in Go's order of the depth check, the
/// budget take and the delimiter.
struct StrictDecoder<'a> {
    b: &'a [u8],
    s: &'a str,
    pos: usize,
}

impl StrictDecoder<'_> {
    fn skip_space(&mut self) {
        while self
            .b
            .get(self.pos)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.b.get(self.pos).copied()
    }

    /// Go `dec.More()`.
    fn more(&mut self) -> bool {
        self.peek().is_some_and(|c| c != b']' && c != b'}')
    }

    /// Go `decodeJSONValue(dec, depth, budget)`; `delim` is what the value's `Token` call
    /// consumes first.
    fn value(
        &mut self,
        depth: usize,
        delim: Delim,
        budget: &mut NodeBudget,
    ) -> Result<(JVal, bool), DecodeFail> {
        if depth > MAX_SANITIZE_DEPTH * 2 {
            return Err(DecodeFail::Syntax);
        }
        if !budget.take() {
            return Err(DecodeFail::Budget);
        }
        let want = match delim {
            Delim::None => None,
            Delim::Comma => Some(b','),
            Delim::Colon => Some(b':'),
        };
        if let Some(d) = want {
            if self.peek() != Some(d) {
                return Err(DecodeFail::Syntax);
            }
            self.pos += 1;
        }
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                let mut m = BTreeMap::new();
                let mut saw_dup = false;
                let mut first = true;
                while self.more() {
                    if !first {
                        if self.peek() != Some(b',') {
                            return Err(DecodeFail::Syntax);
                        }
                        self.pos += 1;
                    }
                    first = false;
                    if self.peek() != Some(b'"') {
                        return Err(DecodeFail::Syntax);
                    }
                    let key = self.string()?;
                    let (val, child_dup) = self.value(depth + 1, Delim::Colon, budget)?;
                    saw_dup |= m.contains_key(&key) || child_dup;
                    m.insert(key, val);
                }
                if self.peek() != Some(b'}') {
                    return Err(DecodeFail::Syntax);
                }
                self.pos += 1;
                Ok((JVal::Obj(m), saw_dup))
            }
            Some(b'[') => {
                self.pos += 1;
                let mut xs = Vec::new();
                let mut saw_dup = false;
                while self.more() {
                    let delim = if xs.is_empty() {
                        Delim::None
                    } else {
                        Delim::Comma
                    };
                    let (val, child_dup) = self.value(depth + 1, delim, budget)?;
                    saw_dup |= child_dup;
                    xs.push(val);
                }
                if self.peek() != Some(b']') {
                    return Err(DecodeFail::Syntax);
                }
                self.pos += 1;
                Ok((JVal::Arr(xs), saw_dup))
            }
            Some(b'"') => Ok((JVal::Str(self.string()?), false)),
            Some(b'-' | b'0'..=b'9') => Ok((JVal::Num(JNum::Text(self.number()?)), false)),
            Some(b't') => self.literal("true", JVal::Bool(true)),
            Some(b'f') => self.literal("false", JVal::Bool(false)),
            Some(b'n') => self.literal("null", JVal::Null),
            _ => Err(DecodeFail::Syntax),
        }
    }

    fn string(&mut self) -> Result<String, DecodeFail> {
        let (decoded, n) = scan_json_string(&self.s[self.pos..]).map_err(|_| DecodeFail::Syntax)?;
        self.pos += n;
        Ok(decoded)
    }

    fn literal(&mut self, word: &str, v: JVal) -> Result<(JVal, bool), DecodeFail> {
        if !self.b[self.pos..].starts_with(word.as_bytes()) {
            return Err(DecodeFail::Syntax);
        }
        self.pos += word.len();
        Ok((v, false))
    }

    /// A JSON number (`-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`), as its text.
    fn number(&mut self) -> Result<String, DecodeFail> {
        let start = self.pos;
        let digit = |b: &[u8], i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
        let mut i = self.pos;
        if self.b.get(i) == Some(&b'-') {
            i += 1;
        }
        match self.b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                while digit(self.b, i) {
                    i += 1;
                }
            }
            _ => return Err(DecodeFail::Syntax),
        }
        if self.b.get(i) == Some(&b'.') {
            i += 1;
            if !digit(self.b, i) {
                return Err(DecodeFail::Syntax);
            }
            while digit(self.b, i) {
                i += 1;
            }
        }
        if matches!(self.b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(self.b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if !digit(self.b, i) {
                return Err(DecodeFail::Syntax);
            }
            while digit(self.b, i) {
                i += 1;
            }
        }
        self.pos = i;
        Ok(self.s[start..i].to_owned())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_payload_column_denylist_accepts_the_four_columns() {
        let cols: Vec<String> = [
            "content",
            "content_parts",
            "attributes",
            "latency_ms",
            "content",
        ]
        .map(String::from)
        .to_vec();
        let set = parse_payload_column_denylist(&cols).unwrap();
        assert_eq!(set.len(), 4);
        assert!(parse_payload_column_denylist(&[]).unwrap().is_empty());
    }

    #[test]
    fn parse_payload_column_denylist_rejects_other_columns_with_gos_text() {
        let err = parse_payload_column_denylist(&["content".into(), "status".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"unsupported PayloadColumnDenylist entry "status": allowed values are "content", "content_parts", "attributes" and "latency_ms""#
        );
    }

    #[test]
    fn go_to_rust_regex_keeps_gos_ascii_classes() {
        assert_eq!(
            go_to_rust_regex(r#"(?i)\b(x)\s+[^\s,\]"]*[\s[:cntrl:]]\w[^\w\s]\p{Z}[a[b]"#),
            r#"(?i)(?-u:\b)(x)[\t\n\f\r ]+[^\t\n\f\r ,\]"]*[\t\n\f\r [:cntrl:]][0-9A-Za-z_][^0-9A-Za-z_\t\n\f\r ]\p{Z}[a\[b]"#
        );
        // Go's \s, \w and \b are ASCII-only; the regex crate's are Unicode-aware.
        let re = Regex::new(&go_to_rust_regex(r"\btoken\s")).unwrap();
        assert!(re.is_match("é token "));
        assert!(!re.is_match("token\u{a0}"));
        assert!(!re.is_match("xtoken "));
        assert!(re.is_match("étoken "));
    }

    #[test]
    fn regexes_compile() {
        for re in [
            &URI_TOKEN_RE,
            &AUTH_HEADER_RE,
            &BEARER_TOKEN_RE,
            &BASIC_TOKEN_RE,
            &JSON_KEY_VAL_TOKEN_RE,
            &TEXT_KEY_VAL_TOKEN_RE,
            &TEXT_KEY_VAL_TOKEN_AT_RE,
            &NESTED_KEY_NAME_RE,
            &COMPOUND_KEY_NAME_RE,
            &QUOTED_VALUE_RE,
            &QUOTED_SENSITIVE_KEY_RE,
            &WORD_RUNE_START_RE,
        ] {
            LazyLock::force(re);
        }
        // (?i) folds U+017F into s and U+212A into k, as Go's does; U+017F is not an ASCII
        // word byte, so \b does not see a word start before it.
        assert!(TEXT_KEY_VAL_TOKEN_AT_RE.is_match("\u{17f}ecret=x"));
        assert!(!TEXT_KEY_VAL_TOKEN_RE.is_match(" \u{17f}ecret=x"));
        assert!(TEXT_KEY_VAL_TOKEN_RE.is_match("api\u{212a}ey=x"));
    }

    #[test]
    fn scan_json_string_reports_gos_offsets() {
        assert_eq!(scan_json_string(r#""a\"b" x"#), Ok(("a\"b".into(), 6)));
        assert_eq!(scan_json_string(r#""a\x""#), Err(JsonStrError::Syntax(4)));
        assert_eq!(scan_json_string("\"a\nb\""), Err(JsonStrError::Syntax(3)));
        assert_eq!(
            scan_json_string(r#""\u12g4""#),
            Err(JsonStrError::Syntax(6))
        );
        assert_eq!(
            scan_json_string(r#""abc"#),
            Err(JsonStrError::UnexpectedEof)
        );
        assert_eq!(
            scan_json_string(r#""\u12"#),
            Err(JsonStrError::UnexpectedEof)
        );
        // Invalid surrogates are U+FFFD; a valid pair is one rune.
        assert_eq!(
            scan_json_string(r#""\ud800\u0041\udc00\ud83d\ude00""#)
                .unwrap()
                .0,
            "\u{fffd}A\u{fffd}\u{1f600}"
        );
        assert_eq!(go_json_unmarshal_string(r#""a" "#), Some("a".into()));
        assert_eq!(go_json_unmarshal_string(r#""a"b"#), None);
    }

    #[test]
    fn strict_decoder_follows_gos_token_grammar() {
        let ok = |s: &str| unmarshal_json_strict_with_budget(s, &mut NodeBudget::new());
        for good in [
            "{}",
            "[]",
            r#"{"a":[1,-0.5e+3,true,false,null,"x"]}"#,
            " [ 1 , 2 ] \n",
        ] {
            assert!(ok(good).is_ok(), "{good}");
        }
        for bad in [
            "[1,]",
            r#"{"a":1,}"#,
            "[1 2]",
            r#"{"a" 1}"#,
            "{1:2}",
            "[01]",
            "[1.]",
            "[-]",
            "[tru]",
            "[truex]",
            "{} {}",
            "[",
            r#"{"a":1 "b":2}"#,
            "[,1]",
            "[1}",
        ] {
            assert_eq!(ok(bad).unwrap_err(), DecodeFail::Syntax, "{bad}");
        }
        let (v, dup) = ok(r#"{"a":1,"a":2.50}"#).unwrap();
        assert!(dup);
        assert_eq!(marshal(&v).unwrap(), r#"{"a":2.50}"#);
        // Depth: the 102nd nested array is at depth 101 > maxSanitizeDepth*2.
        let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(ok(&nested(101)).is_ok());
        assert_eq!(ok(&nested(102)).unwrap_err(), DecodeFail::Syntax);
        let mut small = NodeBudget { remaining: 2 };
        assert_eq!(
            unmarshal_json_strict_with_budget("[1,2]", &mut small).unwrap_err(),
            DecodeFail::Budget
        );
    }

    #[test]
    fn marshal_escapes_strings_as_go_does() {
        let v = JVal::Obj(BTreeMap::from([
            ("b<".to_owned(), JVal::Str("x&y\u{2028}".into())),
            (
                "a".to_owned(),
                JVal::Num(JNum::Value(Number::from_f64(1e21).unwrap())),
            ),
        ]));
        assert_eq!(
            marshal(&v).unwrap(),
            r#"{"a":1e+21,"b\u003c":"x\u0026y\u2028"}"#
        );
    }

    #[test]
    fn decode_std_base64_matches_go() {
        assert_eq!(
            decode_std_base64("dXNlcjpwYXNz"),
            Some(b"user:pass".to_vec())
        );
        assert_eq!(decode_std_base64("YQ=="), Some(b"a".to_vec()));
        assert_eq!(decode_std_base64("YWI="), Some(b"ab".to_vec()));
        assert_eq!(decode_std_base64("YQ"), None);
        assert_eq!(decode_std_base64("YWJ"), None);
        assert_eq!(decode_std_base64("Y\u{17f}=="), None);
        // Non-zero trailing bits are accepted (not StdEncoding.Strict()).
        assert_eq!(decode_std_base64("YR=="), Some(b"a".to_vec()));
    }

    #[test]
    fn redact_json_value_redacts_sensitive_keys_in_values() {
        let v = json!({"api_key": "k", "n": 1.5, "nested": [{"token": "[REDACTED]"}, "{\"password\":\"p\"}"]});
        let (out, changed, lost) = redact_json_value(&v, 0, &mut NodeBudget::new());
        assert_eq!(
            out,
            json!({"api_key": "[REDACTED]", "n": 1.5, "nested": [{"token": "[REDACTED]"}, "{\"password\":\"[REDACTED]\"}"]})
        );
        assert!(changed);
        assert!(!lost);
        let (out, changed, lost) =
            redact_json_value(&json!({"a": 1}), MAX_SANITIZE_DEPTH, &mut NodeBudget::new());
        assert_eq!(out, json!(MAX_DEPTH_PLACEHOLDER));
        assert!(changed && lost);
        let (out, _, lost) =
            redact_json_value(&json!(["a", "b", "c"]), 0, &mut NodeBudget { remaining: 2 });
        assert_eq!(out, json!(["a", SANITIZE_BUDGET_PLACEHOLDER]));
        assert!(lost);
    }

    #[test]
    fn custom_metadata_filter_matches_exact_names_and_prefixes() {
        let f = CustomMetadataFilter::new(&["team".into(), "a2a:*".into(), "*x".into()]);
        assert_eq!(
            f.exact,
            BTreeSet::from(["team".to_owned(), "*x".to_owned()])
        );
        assert_eq!(f.prefixes, vec!["a2a:".to_owned()]);
        assert!(f.allows("team") && f.allows("a2a:task") && f.allows("*x"));
        assert!(!f.allows("teams") && !f.allows("x"));
        let md = json!({"team": 1, "a2a:id": "x", "other": true});
        assert_eq!(
            f.apply(md.as_object().unwrap()),
            Some(
                json!({"team": 1, "a2a:id": "x"})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(f.apply(json!({"other": 1}).as_object().unwrap()), None);
        assert_eq!(CustomMetadataFilter::new(&["*".into()]).allows(""), true);
    }

    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl Logger for Lines {
        fn printf(&self, args: std::fmt::Arguments<'_>) {
            self.0.lock().unwrap().push(args.to_string());
        }
        fn println(&self, args: std::fmt::Arguments<'_>) {
            self.0.lock().unwrap().push(args.to_string());
        }
    }

    #[test]
    fn apply_content_formatter_fails_closed() {
        let payload = json!({"text": "hi"});
        assert_eq!(
            apply_content_formatter(None, None, &payload, "E", false),
            (payload.clone(), String::new(), false)
        );
        let upper: ContentFormatter = Arc::new(|v, t| Ok(json!({"v": v, "t": t})));
        assert_eq!(
            apply_content_formatter(Some(&upper), None, &payload, "E", false),
            (json!({"v": {"text": "hi"}, "t": "E"}), String::new(), false)
        );

        let failing: ContentFormatter = Arc::new(|_, _| Err("secret=abc".into()));
        let logs = Lines::default();
        let got = apply_content_formatter(Some(&failing), Some(&logs), &payload, "E", false);
        assert_eq!(
            got,
            (
                json!("[FORMATTER_FAILED]"),
                "content_formatter raised error".to_owned(),
                true
            )
        );
        apply_content_formatter(Some(&failing), Some(&logs), &payload, "E", true);

        let panicking: ContentFormatter = Arc::new(|_, _| panic!("boom secret"));
        let got = apply_content_formatter(Some(&panicking), Some(&logs), &payload, "E", false);
        assert_eq!(
            got,
            (
                json!("[FORMATTER_FAILED]"),
                "content_formatter panicked".to_owned(),
                true
            )
        );
        assert_eq!(
            *logs.0.lock().unwrap(),
            vec![
                "agentanalytics: ContentFormatter failed for event E (error); content replaced",
                "agentanalytics: ContentFormatter failed for event E (type=error, err_len=10); content replaced",
                "agentanalytics: ContentFormatter panicked for event E (type=string); content replaced",
            ]
        );
    }

    // -----------------------------------------------------------------------------------------
    // Golden: the probe's redaction fixtures (scripts/gen-go-agentanalytics, T2).
    // -----------------------------------------------------------------------------------------

    fn fixture(rel: &str) -> Value {
        let path = format!("{}/tests/fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn golden_constants_match_go() {
        let c = fixture("config/constants.json");
        let strs = |k: &str| -> Vec<String> {
            c[k].as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            strs("sensitiveKeys"),
            SENSITIVE_KEYS.map(String::from).to_vec()
        );
        assert_eq!(strs("patternWords"), *PATTERN_WORDS);
        assert_eq!(
            c["fixedSensitiveKeyRegex"],
            json!(fixed_sensitive_key_regex())
        );
        assert_eq!(c["sensitiveKeyRegex"], json!(sensitive_key_regex()));
        assert_eq!(
            c["bareSensitiveKeyRegex"],
            json!(bare_sensitive_key_regex())
        );
        assert_eq!(c["quotedValueRegex"], json!(quoted_value_regex()));
        let regexes = c["regexes"].as_object().unwrap();
        assert_eq!(regexes.len(), 11);
        for (name, src) in regexes {
            assert_eq!(json!(go_regex_source(name)), *src, "{name}");
        }
        for (k, want) in [
            ("redactedValue", REDACTED_VALUE),
            ("redactedURIPlaceholder", REDACTED_URI_PLACEHOLDER),
            (
                "redactedSensitiveTextPlaceholder",
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            ("formattingFailedPlaceholder", FORMATTING_FAILED_PLACEHOLDER),
            ("unparseableJSONPlaceholder", UNPARSEABLE_JSON_PLACEHOLDER),
        ] {
            assert_eq!(c[k], json!(want), "{k}");
        }
        for (k, want) in [
            (
                "maxInspectableJSONTextBytes",
                MAX_INSPECTABLE_JSON_TEXT_BYTES,
            ),
            ("maxCanonicalUnescapePasses", MAX_CANONICAL_UNESCAPE_PASSES),
            (
                "maxQuotedFragmentsPerInspect",
                MAX_QUOTED_FRAGMENTS_PER_INSPECT,
            ),
            ("maxBasicCredentialBytes", MAX_BASIC_CREDENTIAL_BYTES),
            ("maxNestedKeyLen", MAX_NESTED_KEY_LEN),
        ] {
            assert_eq!(c[k], json!(want), "{k}");
        }
    }

    #[test]
    fn golden_keys_match_go() {
        let cases = fixture("redaction/keys.json");
        let cases = cases["cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let key = case["key"].as_str().unwrap();
            assert_eq!(
                json!(is_sensitive_key(key)),
                case["isSensitiveKey"],
                "isSensitiveKey({key:?})"
            );
            assert_eq!(
                json!(normalize_key(key)),
                case["normalizeKey"],
                "normalizeKey({key:?})"
            );
            assert_eq!(
                json!(remove_key_separators(key)),
                case["removeKeySeparators"],
                "removeKeySeparators({key:?})"
            );
        }
    }

    /// Every entry point over every corpus input (`SmartTruncate100` is checked by
    /// `tests/golden_formatter.rs`).
    #[test]
    fn golden_text_corpus_matches_go() {
        let corpus = fixture("redaction/text_corpus.json");
        let cases = corpus["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 1030);
        let mut failures = Vec::new();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let mut check = |name: &str, got: Value| {
                if got != case[name] {
                    failures.push(format!(
                        "{name}({input:?}):\n   got {got}\n  want {}",
                        case[name]
                    ));
                }
            };
            check("isSensitiveKey", json!(is_sensitive_key(input)));
            let (u, c) = sanitize_uri(input);
            check("sanitizeURI", json!([u, c]));
            let (t, c) = sanitize_raw_text(input);
            check("sanitizeRawText", json!([t, c]));
            check("redactErrorMessage", json!(redact_error_message(input)));
            check("scrubErrorText", json!(scrub_error_text(input)));
            let (r, c, l) = redact_json_text_with_budget(input, 0, &mut NodeBudget::new());
            check("redactJSONTextWithBudget", json!([r, c, l]));
            check(
                "canonicalizeCredentialMarker",
                json!(canonicalize_credential_marker(input)),
            );
            check("hasCredentialPattern", json!(has_credential_pattern(input)));
            check(
                "mayMatchTextPatterns",
                json!(may_match_text_patterns(input)),
            );
        }
        assert!(
            failures.is_empty(),
            "{} mismatches:\n{}",
            failures.len(),
            failures
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
