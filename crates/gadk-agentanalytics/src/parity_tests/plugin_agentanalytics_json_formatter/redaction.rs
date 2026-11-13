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

//! The redaction tests of ADK-Go `plugin/agentanalytics/json_formatter_test.go`:
//! `TestSanitizeURI`, `TestRedactErrorMessage` and `TestSanitizeRawText`.

use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;

use crate::redaction::{
    REDACTED_SENSITIVE_TEXT_PLACEHOLDER, REDACTED_URI_PLACEHOLDER, REDACTED_VALUE,
    UNPARSEABLE_JSON_PLACEHOLDER, nested_pair_end, redact_error_message, sanitize_raw_text,
    sanitize_uri, scrub_error_text,
};

/// Go's limit for each timed call in the linear-time subtests.
const TIME_LIMIT: Duration = Duration::from_secs(3);

/// One `TestRedactErrorMessage` table row.
struct Case {
    name: &'static str,
    msg: String,
    want: String,
}

fn case(name: &'static str, msg: impl Into<String>, want: impl Into<String>) -> Case {
    Case {
        name,
        msg: msg.into(),
        want: want.into(),
    }
}

/// Go `base64.StdEncoding.EncodeToString` (padded standard alphabet).
fn base64_std(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Go `string(json.Marshal(s))` for a string.
fn go_json_string(s: &str) -> String {
    String::from_utf8(gadk_model::to_go_json(s).expect("json.Marshal")).expect("UTF-8 JSON")
}

/// Runs `f` and returns its result with the time it took.
fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let got = f();
    (got, start.elapsed())
}

#[test]
fn test_sanitize_uri() {
    // (name, uri, want, want_changed)
    let tests: [(&str, &str, &str, bool); 22] = [
        ("empty", "", "", false),
        (
            "no query",
            "gs://bucket/file.mp4",
            "gs://bucket/file.mp4",
            false,
        ),
        (
            "harmless query",
            "https://example.com/a?alt=media",
            "https://example.com/a?alt=media",
            false,
        ),
        (
            "harmless fragment",
            "https://example.com/a#section-1",
            "https://example.com/a#section-1",
            false,
        ),
        (
            "API key",
            "https://example.com/a?key=abc&alt=media",
            "https://example.com/a?key=%5BREDACTED%5D&alt=media",
            true,
        ),
        (
            "signed URL",
            "https://storage.googleapis.com/b/o?X-Goog-Signature=abc&X-Goog-Credential=def",
            "https://storage.googleapis.com/b/o?X-Goog-Signature=%5BREDACTED%5D&X-Goog-Credential=%5BREDACTED%5D",
            true,
        ),
        (
            "access token",
            "https://example.com/a?access_token=abc",
            "https://example.com/a?access_token=%5BREDACTED%5D",
            true,
        ),
        (
            "fragment access token",
            "https://example.com/callback#access_token=secret&state=ok",
            "https://example.com/callback#access_token=%5BREDACTED%5D&state=ok",
            true,
        ),
        (
            "path segment api_key pair",
            "https://example.com/v1/api_key/secret/items",
            "https://example.com/v1/%5BREDACTED%5D/%5BREDACTED%5D/items",
            true,
        ),
        (
            "path segment token pair",
            "https://example.com/token/abc",
            "https://example.com/%5BREDACTED%5D/%5BREDACTED%5D",
            true,
        ),
        (
            "path segment key=value",
            "https://example.com/v1/api_key=secret/items",
            "https://example.com/v1/api_key=%5BREDACTED%5D/items",
            true,
        ),
        (
            "double-escaped path segment access_token pair",
            "https://example.test/access%255Ftoken/SECRET",
            "https://example.test/%5BREDACTED%5D/%5BREDACTED%5D",
            true,
        ),
        (
            "double-escaped path segment api_key=value",
            "https://example.test/api%255Fkey=SECRET",
            "https://example.test/api%255Fkey=%5BREDACTED%5D",
            true,
        ),
        (
            "access token in a fragment route",
            "https://example.com/app#/cb?access_token=SECRET123",
            "https://example.com/app#/cb?access_token=%5BREDACTED%5D",
            true,
        ),
        (
            "access token in a URL nested in a query value",
            "https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123",
            "https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D",
            true,
        ),
        (
            "path segment token:value",
            "https://example.com/token:SECRET123/file.png",
            "https://example.com/token:%5BREDACTED%5D/file.png",
            true,
        ),
        (
            "access token after a semicolon in a query value",
            "https://example.com/a?a=1;access_token=SECRET123",
            "https://example.com/a?a=1%3Baccess_token%3D%5BREDACTED%5D",
            true,
        ),
        (
            "camelCase and unseparated credential parameters",
            "https://example.com/a?apiKey=abc&accessToken=def&apikey=ghi&tokenCount=5",
            "https://example.com/a?apiKey=%5BREDACTED%5D&accessToken=%5BREDACTED%5D&apikey=%5BREDACTED%5D&tokenCount=5",
            true,
        ),
        (
            "query value mentioning basic",
            "https://example.com/search?q=basic%20recipes",
            "https://example.com/search?q=basic%20recipes",
            false,
        ),
        (
            "query value mentioning bearer of",
            "https://example.com/search?q=bearer+of+bad+news#the-bearer-of-bad-news",
            "https://example.com/search?q=bearer+of+bad+news#the-bearer-of-bad-news",
            false,
        ),
        (
            "user information",
            "https://user:secret@example.com/a",
            REDACTED_URI_PLACEHOLDER,
            true,
        ),
        (
            "unparsable",
            "://missing-scheme",
            REDACTED_URI_PLACEHOLDER,
            true,
        ),
    ];
    for (name, uri, want, want_changed) in tests {
        let (got, changed) = sanitize_uri(uri);
        assert_eq!(
            (got.as_str(), changed),
            (want, want_changed),
            "{name}: sanitize_uri({uri:?})"
        );
    }
}

#[test]
fn test_redact_error_message() {
    let tests = vec![
        case(
            "JSON error with sensitive key and Bearer token in message field",
            r#"{"api_key":"k","message":"Bearer secret"}"#,
            r#"{"api_key":"[REDACTED]","message":"Bearer [REDACTED]"}"#,
        ),
        case(
            "JSON error with Bearer token in non-sensitive field and sensitive key",
            r#"{"error":"Bearer S","token":"x"}"#,
            r#"{"error":"Bearer [REDACTED]","token":"[REDACTED]"}"#,
        ),
        case(
            "JSON error with sensitive key and signed URL in detail field",
            r#"{"api_key":"k","detail":"https://example.com/v1?key=url-secret"}"#,
            r#"{"api_key":"[REDACTED]","detail":"https://example.com/v1?key=%5BREDACTED%5D"}"#,
        ),
        case(
            "Bearer token separated by tab",
            "auth failed: Bearer\tsecret",
            "auth failed: Bearer [REDACTED]",
        ),
        case(
            "Bearer token separated by newline",
            "auth failed: Bearer\nsecret",
            "auth failed: Bearer [REDACTED]",
        ),
        case(
            "Basic token separated by tab",
            "auth failed: Basic\tdXNlcjpwYXNz",
            "auth failed: Basic [REDACTED]",
        ),
        case(
            "Authorization token header",
            "request failed: Authorization: token ghp_S",
            "request failed: Authorization: [REDACTED]",
        ),
        case(
            "Authorization Digest header",
            r#"Authorization: Digest username="Mufasa", realm="test", response="6629fae49393a05397450978507c4ef1""#,
            "Authorization: [REDACTED]",
        ),
        case(
            "Proxy-Authorization header",
            "Proxy-Authorization: Basic dXNlcjpwYXNz",
            "Proxy-Authorization: [REDACTED]",
        ),
        case(
            "X-API-Key header",
            "X-API-Key: sk-secret-header",
            "X-API-Key: [REDACTED]",
        ),
        case(
            "key=value followed by suffix text is idempotent",
            "api_key=S failed",
            "api_key=[REDACTED] failed",
        ),
        case(
            "percent-encoded sensitive key fails closed",
            "request failed: api%5fkey=SECRET",
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "unicode-escaped sensitive key fails closed",
            r#"request failed: api\u005fkey=SECRET"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "unicode-escaped space after Bearer fails closed",
            r#"request failed: Bearer\u0020SECRET"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "truncated JSON error body without credentials is preserved",
            r#"{"error": {"code": 429, "message": "quota exceeded..."#,
            r#"{"error": {"code": 429, "message": "quota exceeded..."#,
        ),
        case(
            "brace-prefixed diagnostic text without credentials is preserved",
            "{bad request} retry later",
            "{bad request} retry later",
        ),
        case(
            "truncated JSON error body with access_token fails closed",
            r#"{"error": {"code": 401, "access_token": "secret", "message": "unauth"#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "malformed JSON error body with password fails closed",
            r#"{"password":"correct SECRET"#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "key=value with sentinel suffix preserves sentinel and strips secret",
            "access_token=[REDACTED]SECRET",
            "access_token=[REDACTED]",
        ),
        case(
            "Bearer with sentinel suffix preserves sentinel and strips secret",
            "Bearer [REDACTED]SECRET",
            "Bearer [REDACTED]",
        ),
        case(
            "already-redacted key=value sentinel alone is preserved",
            "access_token=[REDACTED]",
            "access_token=[REDACTED]",
        ),
        case(
            "already-redacted Bearer sentinel alone is preserved",
            "Bearer [REDACTED]",
            "Bearer [REDACTED]",
        ),
        case(
            "percent-encoded sensitive key with unrelated percent prose fails closed",
            "request failed at 100%: api%5fkey=SECRET",
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "nested 5-layer percent-encoded sensitive key fails closed",
            "request failed: api%252525255fkey=SECRET",
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "URI query parameter after secret is preserved",
            "API error: GET https://api.exchangerate.host/convert?api_key=SECRET_12345&to=USD",
            "API error: GET https://api.exchangerate.host/convert?api_key=%5BREDACTED%5D&to=USD",
        ),
        case(
            "plain token key-value followed by ampersand parameter is preserved",
            "error: token=SECRET_VAL&format=json",
            "error: token=[REDACTED]&format=json",
        ),
        case(
            "quoted JSON error with password and Bearer token in message",
            r#"{"password":"FIRST_SECRET","message":"Bearer SYNTHETIC_SECRET"}"#,
            r#"{"message":"Bearer [REDACTED]","password":"[REDACTED]"}"#,
        ),
        case(
            "error message with percent-encoded path and sensitive word is preserved",
            "GET https://example.com/v1/projects%2Fp/items failed: token expired",
            "GET https://example.com/v1/projects%2Fp/items failed: token expired",
        ),
        case(
            "bracket-led error message with token expired is preserved",
            "[ERROR] token expired, please sign in again",
            "[ERROR] token expired, please sign in again",
        ),
        case(
            "malformed JSON document with padded sensitive key fails closed",
            r#"{" api_key ":"SYNTHETIC_SECRET""#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "malformed JSON document with truncated temp key fails closed",
            r#"{"temp:cache/value":"SYNTHETIC_SECRET""#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "malformed JSON document with Unicode-padded key fails closed",
            "{\"\u{a0}api_key\u{a0}\":\"SYNTHETIC_SECRET\"",
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "malformed JSON document with escaped Unicode-padded key fails closed",
            r#"{"\u00a0api_key\u00a0":"SYNTHETIC_SECRET""#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "malformed JSON document with Unicode-padded temp key fails closed",
            "{\" \u{a0}temp:path/to/key\u{a0} \":\"secret\"",
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "quoted diagnostic with escaped password returns rejection sentinel",
            r#""note" then "{\"password\":\"SYNTHETIC_SECRET\"}""#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "bare password with hash suffix consumes punctuation",
            "failed: password=prefix#SYNTHETIC_SECRET",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare password with question mark suffix consumes punctuation",
            "failed: password=prefix?SYNTHETIC_SECRET",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare temp key with padded value redacts the whole value",
            "failed: temp:cache=SYNTHETIC_SECRET==",
            "failed: temp:cache=[REDACTED]",
        ),
        case(
            "bare temp key with colon delimiter and padded value redacts the whole value",
            "failed: temp:cache:SYNTHETIC_SECRET==",
            "failed: temp:cache:[REDACTED]",
        ),
        case(
            "bare temp key with spaced colon delimiter and padded value redacts the whole value",
            "failed: temp:cache: SYNTHETIC_SECRET==",
            "failed: temp:cache:[REDACTED]",
        ),
        case(
            "bare temp key with colon-bearing value redacts the whole value",
            "failed: temp:cache=urn:uuid:12345",
            "failed: temp:cache=[REDACTED]",
        ),
        case(
            "bare temp key with colon delimiter and colon-bearing value redacts the whole value",
            "failed: temp:cache:urn:uuid:12345",
            "failed: temp:cache:[REDACTED]",
        ),
        case(
            "bare temp key with spaced colon delimiter and colon-bearing value redacts the whole value",
            "failed: temp:cache: urn:uuid:12345",
            "failed: temp:cache:[REDACTED]",
        ),
        case(
            "bare temp key with slash and padded value redacts the whole value",
            "failed: temp:cache/val=SYNTHETIC_SECRET==",
            "failed: temp:cache/val=[REDACTED]",
        ),
        case(
            "JSON document with equals sign in temp key redacts the value",
            r#"{"temp:cache=foo": "bar"}"#,
            r#"{"temp:cache=foo":"[REDACTED]"}"#,
        ),
        case(
            "JSON document with colon in temp key redacts the value",
            r#"{"temp:cache:foo": "bar"}"#,
            r#"{"temp:cache:foo":"[REDACTED]"}"#,
        ),
        // As in adk-python, the key=value pass reads "temp:cache" as the
        // key and redacts the rest of the text up to "}" as its value.
        case(
            "embedded JSON with equals sign in quoted temp key redacts the value",
            r#"failed: {"temp:cache=foo": "SYNTHETIC_SECRET"}"#,
            r#"failed: {"temp:cache=[REDACTED]}"#,
        ),
        case(
            "bare password with escaped double quote in quoted value redacts the whole value",
            r#"failed: password="prefix\"suffix""#,
            r#"failed: password="[REDACTED]""#,
        ),
        case(
            "bare password with escaped single quote in quoted value redacts the whole value",
            r#"failed: password='prefix\'suffix'"#,
            "failed: password='[REDACTED]'",
        ),
        case(
            "bare temp key with escaped double quote in quoted value redacts the whole value",
            r#"failed: temp:cache="prefix\"suffix""#,
            r#"failed: temp:cache="[REDACTED]""#,
        ),
        case(
            "bare temp key with colon and escaped double quote in quoted value redacts the whole value",
            r#"failed: temp:cache: "prefix\"suffix""#,
            r#"failed: temp:cache:"[REDACTED]""#,
        ),
        case(
            "Bearer token with escaped double quote in quoted value redacts the whole value",
            r#"failed: Bearer "prefix\"suffix""#,
            "failed: Bearer [REDACTED]",
        ),
        case(
            "JSON quoted with %q redacts the escaped value",
            r#"token exchange failed: "{\"access_token\": \"SECRET123\"}""#,
            r#"token exchange failed: "{\"access_token\":\"[REDACTED]\"}""#,
        ),
        case(
            "value quoted with escaped double quotes redacts the whole value",
            r#"failed: password=\"SECRET123\""#,
            r#"failed: password=\"[REDACTED]\""#,
        ),
        case(
            "value quoted with escaped single quotes redacts the whole value",
            r#"failed: password=\'SECRET123\'"#,
            r#"failed: password=\'[REDACTED]\'"#,
        ),
        case(
            "single-quoted key redacts the value",
            "bad config: {'api_key': 'SECRET123'}",
            "bad config: {'api_key':'[REDACTED]'}",
        ),
        case(
            "credentials envelope is redacted whole",
            r#"auth failed: {"credentials": {"access_token": "SECRET123", "expires_in": 3600}}"#,
            r#"auth failed: {"credentials":"[REDACTED]"}"#,
        ),
        case(
            "AWS STS credentials envelope is redacted whole",
            r#"assume role failed: {"Credentials": {"AccessKeyId": "EXAMPLEKEYID", "SecretAccessKey": "SECRET123", "SessionToken": "SECRET123", "Expiration": "2026-10-14T00:00:00Z"}}"#,
            r#"assume role failed: {"Credentials":"[REDACTED]"}"#,
        ),
        case(
            "JSON document with a credentials envelope is redacted whole",
            r#"{"Credentials":{"AccessKeyId":"EXAMPLEKEYID","SecretAccessKey":"SECRET123","SessionToken":"SECRET123"},"Message":"ok"}"#,
            r#"{"Credentials":"[REDACTED]","Message":"ok"}"#,
        ),
        case(
            "AWS secret access key and session token in key=value text are redacted",
            "failed: SecretAccessKey=SECRET123 SessionToken=SECRET456 secret_access_key=SECRET789 session-token=SECRET000",
            "failed: SecretAccessKey=[REDACTED] SessionToken=[REDACTED] secret_access_key=[REDACTED] session-token=[REDACTED]",
        ),
        case(
            "AWS environment variables for a secret access key and a session token are redacted",
            "AWS_SECRET_ACCESS_KEY=SECRET123 AWS_SESSION_TOKEN=SECRET123",
            "AWS_SECRET_ACCESS_KEY=[REDACTED] AWS_SESSION_TOKEN=[REDACTED]",
        ),
        case(
            "AWS shared credentials file entries are redacted",
            "bad profile: [default]\naws_access_key_id = EXAMPLEKEYID\naws_secret_access_key = SECRET123\naws_session_token = SECRET456",
            "bad profile: [default]\naws_access_key_id = EXAMPLEKEYID\naws_secret_access_key=[REDACTED]\naws_session_token=[REDACTED]",
        ),
        case(
            "credentials block in YAML keeps its nested keys and redacts their values",
            "config:\ncredentials:\n  token: SECRET123\n  region: us",
            "config:\ncredentials:\n  token:[REDACTED]\n  region: us",
        ),
        case(
            "credentials block in YAML with a nested password",
            "config:\ncredentials:\n  password: SECRET123",
            "config:\ncredentials:\n  password:[REDACTED]",
        ),
        case(
            "indented credentials block in YAML with a nested client secret",
            "config:\n  credentials:\n    client_secret: SECRET123",
            "config:\n  credentials:\n    client_secret:[REDACTED]",
        ),
        case(
            "credentials block in YAML with a nested hyphenated header",
            "credentials:\n  X-Auth-Token: SECRET123",
            "credentials:\n  X-Auth-Token:[REDACTED]",
        ),
        case(
            "credentials block in YAML with a quoted nested key and value",
            "credentials:\n  'token': 'SECRET123'",
            "credentials:\n  'token':'[REDACTED]'",
        ),
        case(
            "credentials block in YAML with a nested camelCase key",
            "Credentials:\n  SessionToken: SECRET123",
            "Credentials:\n  SessionToken:[REDACTED]",
        ),
        case(
            "credentials block in YAML with a nested dotted key",
            "credentials:\n  spring.datasource.password: SECRET123",
            "credentials:\n  spring.datasource.password:[REDACTED]",
        ),
        case(
            "credentials block in YAML whose nested value is on the next line",
            "credentials:\n  token:\n    SECRET123",
            "credentials:\n  token:[REDACTED]",
        ),
        case(
            "credentials followed by a key: value pair in text",
            "loading credentials: token: SECRET123",
            "loading credentials: token:[REDACTED]",
        ),
        case(
            "credentials followed by a key = value pair in text",
            "credentials: token = SECRET123",
            "credentials: token=[REDACTED]",
        ),
        case(
            "credentials followed by a key: value pair after an unterminated quote",
            r#"bad config: credentials: "token: SECRET123"#,
            r#"bad config: credentials: "token:[REDACTED]"#,
        ),
        case(
            "credentials followed by a key: value pair after an unterminated escaped quote",
            r#"bad config: credentials: \"token: SECRET123"#,
            r#"bad config: credentials: \"token:[REDACTED]"#,
        ),
        case(
            "quoted value that starts with a key: value pair is redacted whole",
            "password: 'token: SECRETA SECRETB'",
            "password:'[REDACTED]'",
        ),
        case(
            "quoted value that starts with a key=value pair is redacted whole",
            r#"token: "password=SECRETA SECRETB""#,
            r#"token:"[REDACTED]""#,
        ),
        case(
            "quoted value that starts with a key:value pair is redacted whole",
            r#"password: "token:SECRETA SECRETB""#,
            r#"password:"[REDACTED]""#,
        ),
        case(
            "Python dict value that starts with a key=value pair is redacted whole",
            "bad config: {'api_key': 'sig=SECRETA SECRETB'}",
            "bad config: {'api_key':'[REDACTED]'}",
        ),
        case(
            "bare value that ends with a key:value pair is redacted whole",
            "password=SECRETP-token:SECRETB",
            "password=[REDACTED]",
        ),
        case(
            "bare value that ends with a dotted key=value pair is redacted whole",
            "password=SECRETP.secret=SECRETB",
            "password=[REDACTED]",
        ),
        case(
            "bare value that ends with a dotted sig=value pair is redacted whole",
            "api_key=SECRETP.sig=SECRETB",
            "api_key=[REDACTED]",
        ),
        case(
            "credentials in a Python dict whose value starts with a key=value pair is redacted whole",
            "bad config: {'credentials': 'secret=correct horse SECRETB staple'}",
            "bad config: {'credentials':'[REDACTED]'}",
        ),
        case(
            "credential in a Python dict whose value starts with a key: value pair is redacted whole",
            "bad config: {'credential': 'token: abc SECRETB'}",
            "bad config: {'credential':'[REDACTED]'}",
        ),
        case(
            "quoted credentials value that starts with a key=value pair is redacted whole",
            r#"credentials="token=abc SECRETB""#,
            r#"credentials="[REDACTED]""#,
        ),
        case(
            "upper-case quoted credentials value that starts with a key=value pair is redacted whole",
            r#"CREDENTIALS="secret=correct horse SECRETB""#,
            r#"CREDENTIALS="[REDACTED]""#,
        ),
        case(
            "credentials bare value that ends with a key:value pair is redacted whole",
            "credentials: SECRETP-token:SECRETB",
            "credentials:[REDACTED]",
        ),
        case(
            "single-quoted credentials value that starts with a key=value pair is redacted whole",
            "credentials: 'secret=correct horse SECRETB'",
            "credentials:'[REDACTED]'",
        ),
        case(
            "quoted value that is a key name with no delimiter after it is redacted",
            r#"failed: password: "password""#,
            r#"failed: password:"[REDACTED]""#,
        ),
        case(
            "quoted value followed by a delimiter is redacted when it is not a key name",
            r#"failed: password: "SECRET123": invalid"#,
            r#"failed: password:"[REDACTED]": invalid"#,
        ),
        case(
            "nested key that ends with = is kept and its value redacted",
            "credentials: token= SECRET123",
            "credentials: token=[REDACTED]",
        ),
        case(
            "value followed by a spaced delimiter is redacted when it is not a key name",
            "failed: password: SECRET123 : retry",
            "failed: password:[REDACTED] : retry",
        ),
        case(
            "value that ends with a redacted value is redacted when its head is not a key name",
            "failed: password=SECRET123:[REDACTED]",
            "failed: password=[REDACTED]",
        ),
        case(
            "nested key with a colon in its name is redacted idempotently",
            "credentials:\n  app:client_secret: SECRET123",
            "credentials:\n  app:client_secret:[REDACTED]",
        ),
        case(
            "value that ends with a key name joined to other text is redacted",
            "failed: password: SECRET123token: retry",
            "failed: password:[REDACTED] retry",
        ),
        case(
            "value that only ends in a key name before a wrapped error is redacted with the pair it starts",
            "connect user=bob password=dev-SECRET123-password: dial tcp 10.0.0.1:5432: connection refused",
            "connect user=bob password=[REDACTED] tcp 10.0.0.1:5432: connection refused",
        ),
        case(
            "quoted value that only ends in a key name is redacted with the pair it starts",
            r#"login failed: password="SECRET123-secret": invalid"#,
            r#"login failed: password="[REDACTED]""#,
        ),
        case(
            "single-quoted value that only ends in a key name is redacted with the pair it starts",
            "login failed: password='SECRET123-secret': invalid",
            "login failed: password='[REDACTED]'",
        ),
        case(
            "value quoted with escaped quotes that only ends in a key name is redacted with the pair it starts",
            r#"login failed: password=\"SECRET123-secret\": invalid"#,
            r#"login failed: password=\"[REDACTED]\""#,
        ),
        case(
            "quoted value that only ends in a key name before a spaced delimiter is redacted with the pair it starts",
            r#"login failed: password="SECRET123-secret" : invalid"#,
            r#"login failed: password="[REDACTED]""#,
        ),
        case(
            "value that only ends in a hyphenated key name is redacted with the pair it starts",
            "api_key=prodSECRET123-api-key: permission denied",
            "api_key=[REDACTED]",
        ),
        case(
            "token that only ends in a key name in prose is redacted with the pair it starts",
            "my login is token=abc-SECRET123-token: please check",
            "my login is token=[REDACTED] check",
        ),
        case(
            "value that only ends in a dotted key name is redacted with the pair it starts",
            "password=svc.SECRET123.password: dial tcp",
            "password=[REDACTED] tcp",
        ),
        case(
            "upper-case value that only ends in a key name is redacted with the pair it starts",
            "PASSWORD=DEV-SECRET123-PASSWORD: DIAL TCP",
            "PASSWORD=[REDACTED] TCP",
        ),
        case(
            "credentials value that only ends in a key name is redacted with the pair it starts",
            "credentials=dev-SECRET123-password: dial tcp",
            "credentials=[REDACTED] tcp",
        ),
        case(
            "value that only ends in a key name before a spaced delimiter is redacted with the pair it starts",
            "failed: password: SECRET123-token = SECRET456 retry",
            "failed: password:[REDACTED] retry",
        ),
        case(
            "compound key name on the line of credentials is redacted with its value",
            "credentials: X-Amz-Security-Token: SECRET123",
            "credentials:[REDACTED]",
        ),
        case(
            "temp: value ending in a key name before a wrapped error is redacted with the pair it starts",
            "password=temp:SECRET123-token: dial tcp",
            "password=[REDACTED] tcp",
        ),
        case(
            "temp: value before a wrapped error is redacted with the pair it starts",
            "password=temp:SECRET123: dial tcp",
            "password=[REDACTED] tcp",
        ),
        case(
            "user:temp: value before a wrapped error is redacted with the pair it starts",
            "password=user:temp:SECRET123: dial tcp",
            "password=[REDACTED] tcp",
        ),
        case(
            "app:temp: value before a wrapped error is redacted with the pair it starts",
            "password=app:temp:SECRET123: dial tcp",
            "password=[REDACTED] tcp",
        ),
        case(
            "quoted temp: value before a wrapped error is redacted with the pair it starts",
            r#"password="temp:SECRET123": dial tcp"#,
            r#"password="[REDACTED]" tcp"#,
        ),
        case(
            "temp: key on the line of credentials is redacted with its value",
            "credentials: temp:foo: SECRET123",
            "credentials:[REDACTED]",
        ),
        case(
            "temp: key on an indented line under credentials is kept and its value redacted",
            "credentials:\n  temp:foo: SECRET123",
            "credentials:\n  temp:foo:[REDACTED]",
        ),
        case(
            "user:temp: key on an indented line under credentials is kept and its value redacted",
            "credentials:\n  user:temp:foo: SECRET123",
            "credentials:\n  user:temp:foo:[REDACTED]",
        ),
        case(
            "prefixed JSON object with a credential after an escaped line break fails closed",
            r#"POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefix with bracket tag before JSON object with a credential after an escaped line break fails closed",
            r#"POST /v1/login [400]: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "bracket-led log line before JSON object with only an escaped-newline credential fails closed",
            r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123"}"#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "bracket-prefixed JSON object with a credential after an escaped line break and a raw marker fails closed",
            r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "JSON object followed by trailing status text with a credential after an escaped line break fails closed",
            r#"POST /v1/login: {"msg":"retry\npassword=SECRET123"} (status 400)"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed JSON array with a credential after an escaped line break fails closed",
            r#"POST /v1/login: 400: ["retry\npassword=dev-SECRET123-password"]"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object with a credential after an escaped line break and a bare pair in the last field redacts each pair",
            r#"{"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\"","code":"token=abc"}"#,
            r#"{"code":"token=[REDACTED]","msg":"retry\npassword=[REDACTED] vault: secret:[REDACTED]"}"#,
        ),
        case(
            "JSON object whose string ends with a value ending in a key name before a word redacts the pair",
            r#"{"msg":"retry\npassword=dev-SECRET123-password: dial"}"#,
            r#"{"msg":"retry\npassword=[REDACTED]"}"#,
        ),
        case(
            "JSON object with a password after an escaped line break and a final value ending in a key name redacts both",
            r#"{"msg":"retry\npassword=hunter2val token=abc-SECRET123-token: dial"}"#,
            r#"{"msg":"retry\npassword=[REDACTED] token=[REDACTED]"}"#,
        ),
        case(
            "JSON array whose string ends with a value ending in a key name before a word redacts the pair",
            r#"["retry\npassword=dev-SECRET123-password: dial"]"#,
            r#"["retry\npassword=[REDACTED]"]"#,
        ),
        case(
            "JSON object whose string ends with a value ending in a key name redacts the pair",
            r#"{"msg":"retry\npassword=dev-SECRET123-password"}"#,
            r#"{"msg":"retry\npassword=[REDACTED]"}"#,
        ),
        case(
            "JSON array whose string ends with a value ending in a key name redacts the pair",
            r#"["retry\npassword=dev-SECRET123-password"]"#,
            r#"["retry\npassword=[REDACTED]"]"#,
        ),
        case(
            "stray quote before JSON object with an escaped-newline credential fails closed",
            r#"bad token " in body: {"msg":"retry\npassword=SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped quote before JSON object with an escaped-newline credential fails closed",
            r#"flag \"foo: {"msg":"retry\npassword=SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "non-JSON backslash before escaped quote and escaped-newline credential fails closed",
            r#"err: "dir C:\work\"retry\npassword=SECRET123""#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "truncated unclosed JSON error body with an escaped-newline credential fails closed",
            r#"upstream error: {"error":"login failed\npassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "single-quoted Python dict with an escaped-newline credential fails closed",
            r#"{'error': 'retry\npassword=SECRET123'}"#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "prefixed single-quoted Python dict with an escaped-newline credential fails closed",
            r#"bad config: {'error': 'retry\npassword=SECRET123'}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "unquoted escaped log line with an escaped-newline credential fails closed",
            r#"err=retry\npassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped backspace before a credential fails closed",
            r#"retry\bpassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped form feed before a credential fails closed",
            r#"retry\fpassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped carriage return before a credential fails closed",
            r#"retry\rpassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped tab before a credential fails closed",
            r#"retry\tpassword=SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "escaped slash in a URL with userinfo credentials fails closed",
            r#"https:\/\/user:SECRET123@example.com"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "deeply backslash-escaped double quote before a key word with no value is kept",
            [r#"\"#.repeat(256).as_str(), r#""token"#].concat(),
            [r#"\"#.repeat(256).as_str(), r#""token"#].concat(),
        ),
        case(
            "deeply backslash-escaped single quote before a key word with no value is kept",
            [r#"\"#.repeat(256).as_str(), "'token"].concat(),
            [r#"\"#.repeat(256).as_str(), "'token"].concat(),
        ),
        case(
            "escaped backslash before a unicode-escaped space in a bearer token fails closed",
            r#"Bearer\\u0020SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "Windows path without credentials is preserved",
            r#"C:\new\file.txt"#,
            r#"C:\new\file.txt"#,
        ),
        case(
            "value that only ends in a key name glued to a quoted value is redacted with it",
            r#"failed: password: dev-SECRET123-token="a SECRET123 b" retry"#,
            "failed: password:[REDACTED] retry",
        ),
        case(
            "value that only ends in a key name before a nested key is redacted with the pair that key starts",
            "password=a-SECRET123-token: token: SECRET456 retry",
            "password=[REDACTED] retry",
        ),
        case(
            "values that only end in key names one after another are redacted together",
            "password=a-SECRET123-token: b-SECRET123-token: c-SECRET123-token: SECRET456 retry",
            "password=[REDACTED] retry",
        ),
        case(
            "Go struct field whose value only ends in a key name is redacted with the pair it starts",
            "failed: {Password:a-SECRET123-token: SECRET456} retry",
            "failed: {Password:[REDACTED]} retry",
        ),
        case(
            "Go map entry whose value only ends in a key name is redacted with the pair it starts",
            "failed: map[password:a-SECRET123-token: SECRET456 user:bob]",
            "failed: map[password:[REDACTED] user:bob]",
        ),
        case(
            "value that only ends in a key name before a redacted value is redacted",
            "failed: token=abc-SECRET123-token:[REDACTED] retry",
            "failed: token=[REDACTED] retry",
        ),
        case(
            "credentials block in YAML with a nested hyphenated compound key",
            "credentials:\n  X-Amz-Security-Token: SECRET123",
            "credentials:\n  X-Amz-Security-Token:[REDACTED]",
        ),
        case(
            "hyphenated header name after credentials is kept and its value redacted",
            "credentials: X-Auth-Token: SECRET123",
            "credentials: X-Auth-Token:[REDACTED]",
        ),
        case(
            "key-shaped bare value with no value after its colon is redacted whole",
            "password=SECRETP-token:",
            "password=[REDACTED]",
        ),
        case(
            "key-shaped bare value with no value after its equals sign is redacted whole",
            "password=SECRETP/token=",
            "password=[REDACTED]",
        ),
        case(
            "base64 value that ends with a key name and one pad is redacted whole",
            "api_key=SECRETAbC+xYz/Sig=",
            "api_key=[REDACTED]",
        ),
        case(
            "base64 value that ends with a key name, one pad and a newline is redacted whole",
            "api_key=SECRETAbC+xYz/Sig=\n",
            "api_key=[REDACTED]\n",
        ),
        case(
            "base64 value that ends with a key name, one pad and a space is redacted whole",
            "api_key=SECRETAbC+xYz/Sig= ",
            "api_key=[REDACTED] ",
        ),
        case(
            "key-shaped bare value followed by a comma is redacted whole",
            "password=SECRETP-token:,next",
            "password=[REDACTED],next",
        ),
        case(
            "key-shaped bare value followed by a semicolon is redacted whole",
            "password=SECRETP-token:;next",
            "password=[REDACTED];next",
        ),
        case(
            "key-shaped bare value followed by a space and a comma is redacted whole",
            "password=SECRETP-token: ,next",
            "password=[REDACTED] ,next",
        ),
        case(
            "key-shaped bare value followed by an ampersand is redacted whole",
            "password=SECRETP-token:&next=1",
            "password=[REDACTED]&next=1",
        ),
        case(
            "key-shaped bare value followed by a tab and a semicolon is redacted whole",
            "password=SECRETP-token:\t;x",
            "password=[REDACTED]\t;x",
        ),
        case(
            "key-shaped bare value at the end of a CRLF line is redacted whole",
            "password=SECRETP-token:\r\n",
            "password=[REDACTED]\r\n",
        ),
        case(
            "key-shaped bare value followed by a form feed is redacted whole",
            "password=SECRETP-token:\u{c}",
            "password=[REDACTED]\u{c}",
        ),
        case(
            "key-shaped bare value followed by a closing brace is redacted whole",
            "failed: password=SECRETP-token: }",
            "failed: password=[REDACTED] }",
        ),
        case(
            "key-shaped value of a Go struct field followed by the closing brace is redacted whole",
            "failed: {Password:abc-token: }",
            "failed: {Password:[REDACTED] }",
        ),
        case(
            "base64 value that ends with a key name, one pad and a closing brace is redacted whole",
            "api_key=SECRETAbC+xYz/Sig= }",
            "api_key=[REDACTED] }",
        ),
        case(
            "key-shaped bare value followed by a closing bracket is redacted whole",
            "password=SECRETP-token: ]",
            "password=[REDACTED] ]",
        ),
        case(
            "key-shaped value of a Go struct field followed by the closing brace and another struct is redacted whole",
            "failed: [{Password:abc-token: } {User:bob}]",
            "failed: [{Password:[REDACTED] } {User:bob}]",
        ),
        case(
            "key-shaped bare value followed by only punctuation is redacted whole",
            "password=abc-token: ...",
            "password=[REDACTED] ...",
        ),
        case(
            "nested key whose value starts with a closing bracket is kept and its value redacted",
            "credentials: token = ]SECRETZ",
            "credentials: token=[REDACTED]",
        ),
        case(
            "nested key in YAML whose value starts with a closing brace is kept and its value redacted",
            "credentials:\n  password: }SECRETZ",
            "credentials:\n  password:[REDACTED]",
        ),
        case(
            "nested key in a Go struct whose value starts with a closing bracket is kept and its value redacted",
            "failed: {Credentials:token: ]SECRETZ}",
            "failed: {Credentials:token:[REDACTED]}",
        ),
        case(
            "nested key whose value is quoted with leading whitespace is kept and its value redacted",
            r#"credentials: token: " SECRETX""#,
            r#"credentials: token:"[REDACTED]""#,
        ),
        case(
            "nested key whose single-quoted value has leading whitespace is kept and its value redacted",
            "credentials: token: ' SECRETX'",
            "credentials: token:'[REDACTED]'",
        ),
        case(
            "nested key whose value is quoted with escaped quotes and has leading whitespace is kept and its value redacted",
            r#"credentials: token: \" SECRETX\""#,
            r#"credentials: token:\"[REDACTED]\""#,
        ),
        case(
            "nested key in YAML whose quoted value has leading whitespace is kept and its value redacted",
            "credentials:\n  token: \" SECRETX\"\n  region: us",
            "credentials:\n  token:\"[REDACTED]\"\n  region: us",
        ),
        case(
            "key-shaped bare value followed by the quote that closes the string around it is redacted whole",
            r#"failed: "password=SECRETP-token: ""#,
            r#"failed: "password=[REDACTED] ""#,
        ),
        case(
            "key-shaped bare value followed by the closing single quote of the string around it is redacted whole",
            "failed: 'password=SECRETP-token: '",
            "failed: 'password=[REDACTED] '",
        ),
        case(
            "key-shaped bare value followed by the closing escaped quote of the string around it is redacted whole",
            r#"failed: \"password=SECRETP-token: \""#,
            r#"failed: \"password=[REDACTED] \""#,
        ),
        case(
            "key-shaped bare value in a quoted string followed by a colon is redacted whole",
            r#"parsing "password=SECRETP-token: ": invalid syntax"#,
            r#"parsing "password=[REDACTED] ": invalid syntax"#,
        ),
        case(
            "key-shaped bare value in a quoted string followed by another quoted string is redacted whole",
            r#"failed: "password=SECRETP-token: " for user "bob""#,
            r#"failed: "password=[REDACTED] " for user "bob""#,
        ),
        case(
            "key-shaped bare value in a quoted string followed by a quoted string that starts with a non-ASCII letter is redacted whole",
            r#"failed: "password=SECRETP-token: " for user "Émile""#,
            r#"failed: "password=[REDACTED] " for user "Émile""#,
        ),
        case(
            "key-shaped bare value in a quoted string followed by an empty quoted string is redacted whole",
            r#"got "password=SECRETP-token: ", want """#,
            r#"got "password=[REDACTED] ", want """#,
        ),
        case(
            "key-shaped bare value in an escaped quoted string followed by an empty escaped quoted string is redacted whole",
            r#"got \"password=SECRETP-token: \", want \"\""#,
            r#"got \"password=[REDACTED] \", want \"\""#,
        ),
        case(
            "nested key whose escaped quoted value is followed by an escaped newline fails closed",
            r#"credentials: token: \" SECRET123\"\nretry"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "nested key glued to a quoted value with spaces is kept and its value redacted",
            r#"credentials: password="correct SECRET123 battery""#,
            r#"credentials: password="[REDACTED]""#,
        ),
        case(
            "nested key glued to a quoted value with leading whitespace is kept and its value redacted",
            r#"credentials: token=" SECRET123""#,
            r#"credentials: token="[REDACTED]""#,
        ),
        case(
            "nested key glued by a colon to a quoted bearer value is kept and its value redacted",
            r#"credentials: token:"Bearer SECRET123""#,
            r#"credentials: token:"[REDACTED]""#,
        ),
        case(
            "nested key glued to a single-quoted value with a comma is kept and its value redacted",
            "credentials: token='a,SECRET123'",
            "credentials: token='[REDACTED]'",
        ),
        case(
            "nested key glued to a quoted value with a semicolon is kept and its value redacted",
            r#"credentials: token="a;SECRET123" more"#,
            r#"credentials: token="[REDACTED]" more"#,
        ),
        case(
            "nested key glued to an escaped quoted value with spaces is kept and its value redacted",
            r#"credentials: token=\"a SECRET123\""#,
            r#"credentials: token=\"[REDACTED]\""#,
        ),
        case(
            "nested key under password glued to a quoted value with spaces is kept and its value redacted",
            r#"password: token:"a SECRET123""#,
            r#"password: token:"[REDACTED]""#,
        ),
        case(
            "nested key in a Go struct glued to a quoted value with leading whitespace is kept and its value redacted",
            r#"failed: {Credentials:token=" SECRET123"}"#,
            r#"failed: {Credentials:token="[REDACTED]"}"#,
        ),
        case(
            "nested key in a Go map glued to a quoted value with spaces is kept and its value redacted",
            r#"failed: map[credentials:token="a SECRET123"]"#,
            r#"failed: map[credentials:token="[REDACTED]"]"#,
        ),
        case(
            "nested key in YAML glued to a quoted value with spaces is kept and its value redacted",
            "credentials:\n  token=\"a SECRET123\"\n  region: us",
            "credentials:\n  token=\"[REDACTED]\"\n  region: us",
        ),
        case(
            "nested pair glued to a bare value is redacted whole",
            "credentials: token=SECRET123",
            "credentials:[REDACTED]",
        ),
        case(
            "nested pair glued to a quoted value without spaces is redacted whole",
            r#"credentials: token="SECRET123""#,
            "credentials:[REDACTED]",
        ),
        case(
            "key-shaped bare value glued to the closing quote of a string followed by another quoted string is redacted whole",
            r#"got "password=SECRETP-token=", want "abc""#,
            r#"got "password=[REDACTED], want "abc""#,
        ),
        case(
            "name that is not a key name glued to a quoted value with spaces is redacted",
            r#"credentials: user="SECRET123 token=SECRET456""#,
            "credentials:[REDACTED] token=[REDACTED]",
        ),
        case(
            "key name glued to a quoted value with spaces by a character other than a delimiter is redacted",
            r#"credentials: token("SECRET123 token=SECRET456")"#,
            "credentials:[REDACTED] token=[REDACTED]",
        ),
        case(
            "nested password in YAML whose quoted value has leading whitespace is kept and its value redacted",
            "credentials:\n  password: \" SECRET123\"\n  region: us",
            "credentials:\n  password:\"[REDACTED]\"\n  region: us",
        ),
        case(
            "quoted nested key whose single-quoted value has leading whitespace is kept and its value redacted",
            "credentials: 'token': ' SECRET123'",
            "credentials: 'token':'[REDACTED]'",
        ),
        case(
            "nested key whose quoted value starts with punctuation is kept and its value redacted",
            r#"credentials: token = ". SECRET123""#,
            r#"credentials: token="[REDACTED]""#,
        ),
        case(
            "nested key whose quoted value starts with a closing brace is kept and its value redacted",
            r#"credentials: token: "} SECRET123""#,
            r#"credentials: token:"[REDACTED]""#,
        ),
        case(
            "nested key in a Go struct whose quoted value has leading whitespace is kept and its value redacted",
            r#"failed: {Credentials:token: " SECRET123"}"#,
            r#"failed: {Credentials:token:"[REDACTED]"}"#,
        ),
        case(
            "nested key in a slice of Go structs whose quoted value has leading whitespace is kept and its value redacted",
            r#"failed: [{Credentials:token: " SECRET123"}]"#,
            r#"failed: [{Credentials:token:"[REDACTED]"}]"#,
        ),
        case(
            "key-shaped value of a Go struct field followed by an unclosed quote is redacted whole",
            r#"failed: {Password:SECRET123-token: "}"#,
            r#"failed: {Password:[REDACTED] "}"#,
        ),
        case(
            "key-shaped value of a Go struct field followed by an unclosed single quote is redacted whole",
            "failed: {Password:SECRET123-token: '}",
            "failed: {Password:[REDACTED] '}",
        ),
        case(
            "key-shaped value of a Go struct field followed by empty quotes is redacted whole",
            r#"failed: {Password:SECRET123-token: ""}"#,
            r#"failed: {Password:[REDACTED] ""}"#,
        ),
        case(
            "key-shaped value of a Go map entry followed by empty quotes is redacted whole",
            r#"failed: map[Password:SECRET123-token: ""]"#,
            r#"failed: map[Password:[REDACTED] ""]"#,
        ),
        case(
            "key-shaped bare value followed by a lone backslash is redacted whole",
            r#"password=SECRET123-token: \"#,
            r#"password=[REDACTED] \"#,
        ),
        case(
            "quoted key-shaped value followed by a delimiter with no value is redacted whole",
            r#"password="SECRETQ-token" :"#,
            r#"password="[REDACTED]" :"#,
        ),
        case(
            "key-shaped bare value followed by a spaced delimiter with no value is redacted whole",
            "password=SECRETQ-token :",
            "password=[REDACTED] :",
        ),
        case(
            "key-shaped bare value followed by a tab and a delimiter with no value is redacted whole",
            "password=SECRETP-token\t:",
            "password=[REDACTED]\t:",
        ),
        case(
            "key-shaped value after a spaced delimiter with no value after it is redacted whole",
            "password = SECRETP-token:",
            "password=[REDACTED]",
        ),
        case(
            "temp: key-shaped value with no value after it is redacted whole",
            "password=temp:SECRETT:",
            "password=[REDACTED]",
        ),
        case(
            "value that is only a key name and a colon is redacted",
            "password=secret:",
            "password=[REDACTED]",
        ),
        case(
            "value that is only a capitalized key name and a colon is redacted",
            "password=Secret:",
            "password=[REDACTED]",
        ),
        case(
            "dotted key-shaped value with no value after it is redacted whole",
            "secret: SECRETR.password=",
            "secret:[REDACTED]",
        ),
        case(
            "nested key followed by a newline and a delimiter is kept and its value redacted",
            "credentials: token\n= SECRETA",
            "credentials: token=[REDACTED]",
        ),
        case(
            "nested key followed by a tab and a delimiter is kept and its value redacted",
            "credentials: token\t= SECRETA",
            "credentials: token=[REDACTED]",
        ),
        case(
            "nested key followed by a spaced colon is kept and its value redacted",
            "credentials: token : SECRETA",
            "credentials: token:[REDACTED]",
        ),
        case(
            "credentials object in text is redacted whole",
            "credentials: {token: SECRET123, refresh: SECRET456}",
            "credentials:[REDACTED]",
        ),
        case(
            "credentials list in text is redacted whole",
            "credentials: [token: SECRET123, refresh: SECRET456]",
            "credentials:[REDACTED]",
        ),
        case(
            "credentials envelope printed with %+v is redacted whole",
            "assume role failed: {Credentials:{AccessKeyID:EXAMPLEKEYID SecretAccessKey:SECRET123 SessionToken:SECRET456}}",
            "assume role failed: {Credentials:[REDACTED]}",
        ),
        case(
            "object under a sensitive key is redacted whole",
            r#"auth failed: {"token": {"value": "SECRET123"}}"#,
            r#"auth failed: {"token":"[REDACTED]"}"#,
        ),
        case(
            "token object under a sensitive key is redacted whole",
            r#"auth failed: {"token": {"access_token": "SECRET123", "expires_in": 3600}} retrying"#,
            r#"auth failed: {"token":"[REDACTED]"} retrying"#,
        ),
        case(
            "bare value with a parenthesis is redacted whole",
            "failed: password=abc)SECRET123",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value starting with a parenthesis is redacted whole",
            "failed: password=)SECRET123",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value starting with slashes is redacted whole",
            "failed: password: //SECRET123",
            "failed: password:[REDACTED]",
        ),
        case(
            "bare value with a bracket is redacted whole",
            "failed: password=abc]SECRET123",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value with a quote is redacted whole",
            r#"failed: password=abc"SECRET123"#,
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value with a closing brace is redacted whole",
            "failed: password=ab}SECRET123",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value ending with a closing brace is redacted whole",
            "failed: password=SECRET123}",
            "failed: password=[REDACTED]",
        ),
        case(
            "bare value starting with a closing brace is redacted whole",
            "failed: password=}SECRET123",
            "failed: password=[REDACTED]",
        ),
        case(
            "basic credential that decodes to user:password is redacted",
            "auth failed: basic dXNlcjpwYXNz",
            "auth failed: basic [REDACTED]",
        ),
        case(
            "padded basic credential is redacted",
            "auth failed: Basic dXNlcg==",
            "auth failed: Basic [REDACTED]",
        ),
        case(
            "unpadded basic value without a colon is kept",
            "auth failed: basic YWJj",
            "auth failed: basic YWJj",
        ),
        case(
            "basic followed by a word is kept",
            "basic auth not configured",
            "basic auth not configured",
        ),
        case(
            "basic in prose is kept",
            "Write a basic HTTP server in Go",
            "Write a basic HTTP server in Go",
        ),
        case(
            "bearer of in prose is kept",
            "I'm the bearer of bad news",
            "I'm the bearer of bad news",
        ),
        case(
            "bearer token starting with of is redacted",
            "auth failed: Bearer of-SECRET123",
            "auth failed: Bearer [REDACTED]",
        ),
        case(
            "markdown link mentioning basic is kept",
            "[Basic usage](https://example.com/docs)",
            "[Basic usage](https://example.com/docs)",
        ),
        case(
            "quoted sensitive word in prose is kept",
            r#"[Q] What does "token" mean?"#,
            r#"[Q] What does "token" mean?"#,
        ),
        case(
            "URL with basic in a query value is kept",
            "GET https://example.com/search?q=basic%20recipes failed",
            "GET https://example.com/search?q=basic%20recipes failed",
        ),
        case(
            "key with a Kelvin sign, which case-insensitive matching folds to k, is redacted",
            "failed: TOKEN=SECRET123",
            "failed: TOKEN=[REDACTED]",
        ),
        case(
            "key with a long s, which case-insensitive matching folds to s, is redacted",
            "failed: paſſword=SECRET123",
            "failed: paſſword=[REDACTED]",
        ),
        case(
            "camelCase keys in key=value and key: value text are redacted",
            "auth failed: accessToken=SECRET123 apiKey: SECRET456",
            "auth failed: accessToken=[REDACTED] apiKey:[REDACTED]",
        ),
        case(
            "x_goog_api_key in key=value text is redacted",
            "request failed: x_goog_api_key=SECRET123",
            "request failed: x_goog_api_key=[REDACTED]",
        ),
        case(
            "x-goog-api-key header is redacted",
            "request failed: x-goog-api-key: SECRET123",
            "request failed: x-goog-api-key: [REDACTED]",
        ),
        case(
            "Go struct printed with %+v has credential fields redacted",
            "bad config: {APIKey:SECRET123 ClientSecret:SECRET456 Region:us}",
            "bad config: {APIKey:[REDACTED] ClientSecret:[REDACTED] Region:us}",
        ),
        case(
            "Go struct printed with %+v ending with credential fields keeps its closing brace",
            "bad config: {APIKey:SECRET123 ClientSecret:SECRET456}",
            "bad config: {APIKey:[REDACTED] ClientSecret:[REDACTED]}",
        ),
        case(
            "Go struct printed with %+v whose credential field holds a closing brace",
            "bad config: {Password:ab}SECRET123}",
            "bad config: {Password:[REDACTED]}",
        ),
        case(
            "Go struct printed with %+v whose credential field holds a brace pair",
            "bad config: {Password:ab{SECRET123} Region:us}",
            "bad config: {Password:[REDACTED] Region:us}",
        ),
        case(
            "nested Go structs printed with %+v keep their closing braces",
            "bad config: {Auth:{Token:SECRET123}} and {Auth:{Token:SECRET456} Region:us}",
            "bad config: {Auth:{Token:[REDACTED]}} and {Auth:{Token:[REDACTED]} Region:us}",
        ),
        case(
            "slice of Go structs and Go map printed with %+v keep their closing brackets",
            "bad config: [{APIKey:SECRET123}] map[password:SECRET456]",
            "bad config: [{APIKey:[REDACTED]}] map[password:[REDACTED]]",
        ),
        case(
            "Go struct printed with %+v and quoted with %q keeps its closing brace and quote",
            r#"load failed: "{Region:us APIKey:SECRET123}": invalid"#,
            r#"load failed: "{Region:us APIKey:[REDACTED]}": invalid"#,
        ),
        case(
            "quoted credentials ending with a closing bracket keep their quotes",
            r#"bad config: {'password': 'SECRET123}'} [{'token': 'SECRET456]'}] config.Auth{Password:"SECRET789}"}"#,
            r#"bad config: {'password':'[REDACTED]'} [{'token':'[REDACTED]'}] config.Auth{Password:"[REDACTED]"}"#,
        ),
        case(
            "JSON quoted with %q whose credential ends with a closing brace keeps its quotes",
            r#"load failed: "{\"password\":\"SECRET123}\"}""#,
            r#"load failed: "{\"password\":\"[REDACTED]\"}""#,
        ),
        case(
            "Go struct printed with %+v whose credential field holds a closing brace before another field",
            "bad config: {Password:ab}cd Token:SECRET123}",
            "bad config: {Password:[REDACTED] Token:[REDACTED]}",
        ),
        case(
            "Go struct printed with %+v in parentheses keeps its closing brace and parenthesis",
            "({APIKey:SECRET123})",
            "({APIKey:[REDACTED]})",
        ),
        case(
            "Go struct printed with %+v in single quotes ending a sentence keeps its closing brace, quote and period",
            "'{APIKey:SECRET123}'.",
            "'{APIKey:[REDACTED]}'.",
        ),
        case(
            "Go struct printed with %+v in JSON quoted with %q keeps its closing brace and escaped quote",
            r#"bad config: "{\"msg\":\"{APIKey:SECRET123}\"}""#,
            r#"bad config: "{\"msg\":\"{APIKey:[REDACTED]}\"}""#,
        ),
        case(
            "closing brace that closes no bracket is redacted with the value",
            "] password=SECRET123}",
            "] password=[REDACTED]",
        ),
        case(
            "closing bracket that closes no bracket does not hide a later open brace",
            "unexpected ] in {Token:SECRET123}",
            "unexpected ] in {Token:[REDACTED]}",
        ),
        case(
            "AWS credentials printed with %+v are redacted",
            "assume role failed: {AccessKeyID:EXAMPLEKEYID SecretAccessKey:SECRET123 SessionToken:SECRET456}",
            "assume role failed: {AccessKeyID:EXAMPLEKEYID SecretAccessKey:[REDACTED] SessionToken:[REDACTED]}",
        ),
        case(
            "empty credential field in a Go struct printed with %+v is kept",
            "bad config: {Password:}",
            "bad config: {Password:}",
        ),
        case(
            "JSON error with a camelCase sensitive key",
            r#"{"accessToken":"SECRET123","message":"denied"}"#,
            r#"{"accessToken":"[REDACTED]","message":"denied"}"#,
        ),
        case(
            "embedded JSON with a camelCase sensitive key",
            r#"token exchange failed: {"accessToken": "SECRET123", "expiresIn": 3600}"#,
            r#"token exchange failed: {"accessToken":"[REDACTED]", "expiresIn": 3600}"#,
        ),
        case(
            "session user and app keys in key=value text are redacted",
            "failed: user:api_key=SECRET123 app:clientSecret=SECRET456",
            "failed: user:api_key=[REDACTED] app:clientSecret=[REDACTED]",
        ),
        case(
            "keys that only contain a sensitive key name are kept",
            "failed: tokenCount=5 api_key_id=abc secretName=projects/p/secrets/s",
            "failed: tokenCount=5 api_key_id=abc secretName=projects/p/secrets/s",
        ),
        case(
            "hyphenated sensitive keys are redacted",
            "failed: x-api-key=SECRET123 refresh-token=SECRET456",
            "failed: x-api-key=[REDACTED] refresh-token=[REDACTED]",
        ),
        case(
            "command-line flags are redacted",
            "mysql -u root --password=SECRET123 failed",
            "mysql -u root --password=[REDACTED] failed",
        ),
        case(
            "command-line flag with a hyphenated key is redacted",
            "--api-key=SECRET123",
            "--api-key=[REDACTED]",
        ),
        case(
            "command-line flags with hyphenated token keys are redacted",
            "exec failed: tool --token=SECRET123 --access-token=SECRET456 github-token=SECRET789",
            "exec failed: tool --token=[REDACTED] --access-token=[REDACTED] github-token=[REDACTED]",
        ),
        case(
            "hyphenated header ending in token is redacted",
            "X-Auth-Token: SECRET123",
            "X-Auth-Token:[REDACTED]",
        ),
        case(
            "upper-case hyphenated header ending in token is redacted",
            "PRIVATE-TOKEN: SECRET123",
            "PRIVATE-TOKEN:[REDACTED]",
        ),
        case(
            "credential in a nested URL in text is redacted in place",
            "see https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123 for details",
            "see https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D for details",
        ),
        case(
            "credential in a nested URL that ends a sentence is redacted in place",
            "redirected to https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123.",
            "redirected to https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D.",
        ),
        case(
            "credential in a nested URL in parentheses is redacted in place",
            "see (https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123) for details",
            "see (https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D) for details",
        ),
        case(
            "URL with a credential query value and another escape is redacted in place",
            "GET https://example.com/cb?access_token=SECRET123&q=a%20b failed",
            "GET https://example.com/cb?access_token=%5BREDACTED%5D&q=a%20b failed",
        ),
        case(
            "URL with a credential path segment and another escape is redacted in place",
            "GET https://example.com/v1/access_token=SECRET123 failed at 100%25",
            "GET https://example.com/v1/access_token=%5BREDACTED%5D failed at 100%25",
        ),
        case(
            "object quoted with %q whose string field holds a closing brace is redacted whole",
            r#"auth failed: "{\"token\": {\"label\": \"a},\", \"val\": \"SECRET123\"}}""#,
            r#"auth failed: "{\"token\":[REDACTED]}""#,
        ),
        case(
            "array quoted with %q whose string field holds a closing bracket is redacted whole",
            r#"auth failed: "{\"token\": [\"a],\", \"SECRET123\"], \"expires_in\": 3600}""#,
            r#"auth failed: "{\"token\":[REDACTED], \"expires_in\": 3600}""#,
        ),
        case(
            "quote-led prose with brackets is kept",
            r#""Hello," she said [laughing]. "Bye.""#,
            r#""Hello," she said [laughing]. "Bye.""#,
        ),
        case(
            "quoted JSON string whose escape hides a bearer token fails closed",
            r#""Bearer\tSECRET123""#,
            UNPARSEABLE_JSON_PLACEHOLDER,
        ),
        case(
            "JSON object where earlier token value ends with escaped newline before value ending in key name",
            r#"{"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#,
            r#"{"msg":"token=[REDACTED]\npassword:[REDACTED]"}"#,
        ),
        case(
            "prefixed JSON object where earlier token swallows escaped newline before value ending in key name",
            r#"POST /v1/login: 400: {"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where earlier compound key value ends with escaped newline before password",
            r#"{"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
            r#"{"msg":"secret:[REDACTED]\npassword:[REDACTED]"}"#,
        ),
        case(
            "prefixed JSON object where earlier compound key value swallows escaped newline before password",
            r#"POST /v1/login: 400: {"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where password colon is followed by escaped newline and space",
            r#"{"msg":"password:\n SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
        ),
        case(
            "prefixed JSON object where password colon is followed by escaped newline and space",
            r#"POST /v1/login: 400: {"msg":"password:\n SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed single-quoted Python dict where earlier token swallows escaped newline before password",
            r#"bad config: {'error': 'token=abc\npassword: SECRET123'}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "truncated JSON error body where earlier token swallows escaped newline before password",
            r#"upstream error: {"error":"token=abc\npassword: SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "bare token value ending with escaped quote before password key",
            r#"token=abc\"password\": SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed quoted string where earlier token swallows escaped newline before password",
            r#"upstream failed: "token=abc\npassword: SECRET123""#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed JSON object where earlier token swallows percent-encoded newline before password",
            r#"POST /v1/login: 400: {"msg":"token=abc%0Apassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed JSON object where earlier token swallows double-escaped unicode newline before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\\u000apassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed JSON object where earlier token swallows escaped newline and unicode colon before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\npassword\u003a SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where earlier token swallows unicode-escaped backslash and n before password",
            r#"{"msg":"token=abc\u005cnpassword: SECRET123"}"#,
            r#"{"msg":"[REDACTED_SENSITIVE_TEXT]"}"#,
        ),
        case(
            "prefixed JSON object where earlier token swallows unicode-escaped backslash and n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\u005cnpassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where earlier token swallows percent-escaped backslash and n before password",
            r#"{"msg":"token=abc%5Cnpassword: SECRET123"}"#,
            r#"{"msg":"[REDACTED_SENSITIVE_TEXT]"}"#,
        ),
        case(
            "prefixed JSON object where earlier token swallows hex-escaped backslash and n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\x5cnpassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "prefixed JSON object where earlier token swallows unicode-escaped backslash and unicode n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\u005c\u006epassword: SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where password colon is followed by escaped backspace and space",
            r#"{"msg":"password:\b SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
        ),
        case(
            "prefixed JSON object where password colon is followed by escaped backspace and space",
            r#"POST /v1/login: 400: {"msg":"password:\b SECRET123"}"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "plain text where password colon is followed by escaped backspace and space",
            r#"password:\b SECRET123"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "JSON object where password colon is followed by unicode backspace and space",
            r#"{"msg":"password:\u0008 SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
        ),
        case(
            "JSON object where password colon is followed by unicode vertical tab and space",
            r#"{"msg":"password:\u000b SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
        ),
        case(
            "JSON object where password colon is followed by unicode NUL and space",
            r#"{"msg":"password:\u0000 SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
        ),
        case(
            "Go struct field ending in key name followed by space, closing brace, backspace byte and secret",
            "failed: {Password:abc-token: }\u{8}SECRET123",
            "failed: {Password:[REDACTED]",
        ),
        case(
            "Go struct field ending in key name followed by space, backspace byte and closing brace",
            "failed: {Password:abc-token: \u{8}}",
            "failed: {Password:[REDACTED] \u{8}}",
        ),
        case(
            "caller-supplied redaction placeholders splitting secret after escaped newline fail closed",
            r#"password:\n [REDACTED]a[REDACTED]u[REDACTED]t[REDACTED]h[REDACTED] authorization=1 authorization=2"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
        case(
            "caller-supplied redaction placeholders with nonce prefix splitting secret after escaped newline fail closed",
            r#"password:\n [REDACTED]zzRedactedLiteral[REDACTED]a[REDACTED]u[REDACTED]t[REDACTED]h[REDACTED] authorization=1 authorization=2"#,
            REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
        ),
    ];
    for tt in &tests {
        let got = redact_error_message(&tt.msg);
        assert_eq!(
            got, tt.want,
            "{}: redact_error_message({:?})",
            tt.name, tt.msg
        );
        let twice = redact_error_message(&got);
        assert_eq!(
            twice, got,
            "{}: redact_error_message is not idempotent: second pass({got:?})",
            tt.name
        );
    }
}

#[test]
fn test_sanitize_raw_text() {
    // deeply quoted credential text remains unparseable blob
    {
        let mut payload = String::from(r#"{"\u0061pi_key": "secret_credential_value""#);
        for _ in 0..8 {
            payload = go_json_string(&payload);
        }
        let (got, lost) = sanitize_raw_text(&payload);
        assert_eq!(
            got, UNPARSEABLE_JSON_PLACEHOLDER,
            "sanitize_raw_text with 8 quoted layers"
        );
        assert!(
            lost,
            "sanitize_raw_text with 8 quoted layers reported lost = false, want true"
        );
    }

    // raw content with truncated temp key and Unicode-padded sensitive keys fails closed
    {
        let samples = [
            r#"{"temp:cache/value":"SYNTHETIC_SECRET""#,
            "{\"\u{a0}api_key\u{a0}\":\"SYNTHETIC_SECRET\"",
            r#"{"\u00a0api_key\u00a0":"SYNTHETIC_SECRET""#,
            "{\" \u{a0}temp:path/to/key\u{a0} \":\"secret\"",
        ];
        for s in samples {
            let (got, lost) = sanitize_raw_text(s);
            assert_eq!(
                (got.as_str(), lost),
                (UNPARSEABLE_JSON_PLACEHOLDER, true),
                "sanitize_raw_text({s:?})"
            );
        }
    }

    // bare temp assignment with padded or colon-bearing value redacts the whole value
    {
        let tests = [
            (
                "failed: temp:cache=SYNTHETIC_SECRET==",
                "failed: temp:cache=[REDACTED]",
            ),
            (
                "failed: temp:cache:SYNTHETIC_SECRET==",
                "failed: temp:cache:[REDACTED]",
            ),
            (
                "failed: temp:cache=urn:uuid:12345",
                "failed: temp:cache=[REDACTED]",
            ),
            (
                "failed: temp:cache:urn:uuid:12345",
                "failed: temp:cache:[REDACTED]",
            ),
            (
                "failed: temp:cache/val=SYNTHETIC_SECRET==",
                "failed: temp:cache/val=[REDACTED]",
            ),
        ];
        for (raw, want) in tests {
            let (got, lost) = sanitize_raw_text(raw);
            assert_eq!(
                (got.as_str(), lost),
                (want, true),
                "sanitize_raw_text({raw:?})"
            );
        }
    }

    // harmless bracket-led prose without quotes or escapes is preserved
    {
        let prose = "[docs](https://cloud.google.com/bigquery)";
        let (got, lost) = sanitize_raw_text(prose);
        assert_eq!(
            (got.as_str(), lost),
            (prose, false),
            "sanitize_raw_text({prose:?})"
        );
    }

    // prose mentioning sensitive words without credential patterns is preserved
    {
        let prose_samples = [
            "[Help] I forgot my password",
            "[INFO] refreshed the access token",
            "[Docs] see the credentials page",
            "[Note] your password was reset",
        ];
        for prose in prose_samples {
            let (got, lost) = sanitize_raw_text(prose);
            assert_eq!(
                (got.as_str(), lost),
                (prose, false),
                "sanitize_raw_text({prose:?})"
            );
        }
    }

    // bracket-led prose with quotes and Windows paths is preserved
    {
        let prose_samples = [
            r#"[INFO] user clicked "Submit""#,
            r#"[1] Smith, "On Analytics", 2020"#,
            r#"{name} said "hello" to the team"#,
            r#"[path] C:\Users\me\report.txt was saved"#,
            r#"[regex] use \d+ to match digits"#,
            "My reset link https://example.com/reset?email=bob%40example.com says the token expired",
            "I typed 100%25 of my password correctly",
            "Fetched https://example.com/items?q=red%20shoes; the token expired after 3 retries",
        ];
        for prose in prose_samples {
            let (got, lost) = sanitize_raw_text(prose);
            assert_eq!(
                (got.as_str(), lost),
                (prose, false),
                "sanitize_raw_text({prose:?})"
            );
        }
    }

    // large JSON documents over 64 KiB are inspected and preserved
    {
        let mut large_obj = std::collections::BTreeMap::new();
        large_obj.insert("document", "A".repeat(100 * 1024));
        large_obj.insert("api_key", "secret_key_val".to_owned());
        let (got, truncated) = crate::formatter::smart_truncate(&large_obj, 500 * 1024).unwrap();
        let got = String::from_utf8(got).unwrap();
        assert!(
            !truncated,
            "expected truncated=false for large JSON under max limit"
        );
        assert!(
            got.contains(REDACTED_VALUE),
            "expected api_key to be redacted in large JSON"
        );
        assert!(
            got.contains("AAAA"),
            "expected document content to be preserved"
        );
    }

    // text without credentials is unchanged
    {
        let prose_samples = [
            "Write a basic HTTP server in Go",
            "I'm the bearer of bad news",
            "basic auth not configured",
            "[Basic usage](https://example.com/docs)",
            r#"[Q] What does "token" mean?"#,
            "See https://example.com/search?q=basic%20recipes for ideas",
            r#""Hello," she said [laughing]. "Bye.""#,
            r#""Hello," she said {quietly}. "Bye.""#,
            r#""What does [this] mean?""#,
        ];
        for prose in prose_samples {
            let (got, lost) = sanitize_raw_text(prose);
            assert_eq!(
                (got.as_str(), lost),
                (prose, false),
                "sanitize_raw_text({prose:?})"
            );
        }
    }

    // quote-led text with a JSON string that decodes to a credential fails closed
    {
        // The checks on the raw text do not decode \t, so these are only
        // caught by reading the quoted strings as JSON.
        let samples = [
            r#""Bearer\tSECRET123""#,
            r#""Bearer\tSECRET123" was rejected"#,
            r#""Hi" then "Bearer\tSECRET123""#,
        ];
        for s in samples {
            let (got, lost) = sanitize_raw_text(s);
            assert_eq!(
                (got.as_str(), lost),
                (UNPARSEABLE_JSON_PLACEHOLDER, true),
                "sanitize_raw_text({s:?})"
            );
        }
    }

    // credentials in quoted or nested values are redacted
    {
        let tests: Vec<(String, String)> = vec![
            (
                String::from(r#"token exchange failed: "{\"access_token\": \"SECRET123\"}""#),
                String::from(r#"token exchange failed: "{\"access_token\":\"[REDACTED]\"}""#),
            ),
            (
                String::from(r#"failed: password=\"SECRET123\""#),
                String::from(r#"failed: password=\"[REDACTED]\""#),
            ),
            (
                String::from("bad config: {'api_key': 'SECRET123'}"),
                String::from("bad config: {'api_key':'[REDACTED]'}"),
            ),
            (
                String::from(
                    r#"auth failed: {"credentials": {"access_token": "SECRET123", "expires_in": 3600}}"#,
                ),
                String::from(r#"auth failed: {"credentials":"[REDACTED]"}"#),
            ),
            (
                String::from(
                    r#"assume role failed: {"Credentials": {"AccessKeyId": "EXAMPLEKEYID", "SecretAccessKey": "SECRET123", "SessionToken": "SECRET123"}}"#,
                ),
                String::from(r#"assume role failed: {"Credentials":"[REDACTED]"}"#),
            ),
            (
                String::from(r#"auth failed: {"token": {"value": "SECRET123"}}"#),
                String::from(r#"auth failed: {"token":"[REDACTED]"}"#),
            ),
            (
                String::from("failed: password=abc)SECRET123"),
                String::from("failed: password=[REDACTED]"),
            ),
            (
                String::from("failed: password=ab}SECRET123"),
                String::from("failed: password=[REDACTED]"),
            ),
            (
                String::from("bad config: {'password': 'SECRET123}'}"),
                String::from("bad config: {'password':'[REDACTED]'}"),
            ),
            (
                String::from("bad config: {APIKey:SECRET123 ClientSecret:SECRET123}"),
                String::from("bad config: {APIKey:[REDACTED] ClientSecret:[REDACTED]}"),
            ),
            (
                String::from("failed: password: //SECRET123"),
                String::from("failed: password:[REDACTED]"),
            ),
            (
                String::from(
                    r#"auth failed: "{\"token\": {\"label\": \"a},\", \"val\": \"SECRET123\"}}""#,
                ),
                String::from(r#"auth failed: "{\"token\":[REDACTED]}""#),
            ),
            (
                String::from(
                    "see https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123 for details",
                ),
                String::from(
                    "see https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D for details",
                ),
            ),
            (
                String::from("GET https://example.com/cb?access_token=SECRET123&q=a%20b failed"),
                String::from(
                    "GET https://example.com/cb?access_token=%5BREDACTED%5D&q=a%20b failed",
                ),
            ),
            (
                String::from("config:\ncredentials:\n  token: SECRET123\n  region: us"),
                String::from("config:\ncredentials:\n  token:[REDACTED]\n  region: us"),
            ),
            (
                String::from("config:\ncredentials:\n  password: SECRET123"),
                String::from("config:\ncredentials:\n  password:[REDACTED]"),
            ),
            (
                String::from("config:\n  credentials:\n    client_secret: SECRET123"),
                String::from("config:\n  credentials:\n    client_secret:[REDACTED]"),
            ),
            (
                String::from("credentials:\n  X-Auth-Token: SECRET123"),
                String::from("credentials:\n  X-Auth-Token:[REDACTED]"),
            ),
            (
                String::from("credentials:\n  'token': 'SECRET123'"),
                String::from("credentials:\n  'token':'[REDACTED]'"),
            ),
            (
                String::from("loading credentials: token: SECRET123"),
                String::from("loading credentials: token:[REDACTED]"),
            ),
            (
                String::from("credentials: token = SECRET123"),
                String::from("credentials: token=[REDACTED]"),
            ),
            (
                String::from("Credentials:\n  SessionToken: SECRET123"),
                String::from("Credentials:\n  SessionToken:[REDACTED]"),
            ),
            (
                [
                    "credentials:\n  token:\n",
                    " ".repeat(100).as_str(),
                    "SECRET123",
                ]
                .concat(),
                String::from("credentials:\n  token:[REDACTED]"),
            ),
            (
                String::from("credentials:\n  db/password: SECRET123"),
                String::from("credentials:\n  db/password:[REDACTED]"),
            ),
            (
                String::from("credentials: db/password = SECRET123"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from(
                    "connect user=bob password=dev-SECRET123-password: dial tcp 10.0.0.1:5432: connection refused",
                ),
                String::from(
                    "connect user=bob password=[REDACTED] tcp 10.0.0.1:5432: connection refused",
                ),
            ),
            (
                String::from(r#"login failed: password="SECRET123-secret": invalid"#),
                String::from(r#"login failed: password="[REDACTED]""#),
            ),
            (
                String::from("api_key=prodSECRET123-api-key: permission denied"),
                String::from("api_key=[REDACTED]"),
            ),
            (
                String::from("my login is token=abc-SECRET123-token: please check"),
                String::from("my login is token=[REDACTED] check"),
            ),
            (
                String::from("credentials: X-Amz-Security-Token: SECRET123"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from("password=a-SECRET123-token: token: SECRET456 retry"),
                String::from("password=[REDACTED] retry"),
            ),
            (
                String::from("credentials: {token: SECRET123, refresh: SECRET456}"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from("password: 'token: SECRETA SECRETB'"),
                String::from("password:'[REDACTED]'"),
            ),
            (
                String::from(r#"token: "password=SECRETA SECRETB""#),
                String::from(r#"token:"[REDACTED]""#),
            ),
            (
                String::from(r#"password: "token:SECRETA SECRETB""#),
                String::from(r#"password:"[REDACTED]""#),
            ),
            (
                String::from("bad config: {'api_key': 'sig=SECRETA SECRETB'}"),
                String::from("bad config: {'api_key':'[REDACTED]'}"),
            ),
            (
                String::from("password=SECRETP-token:SECRETB"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=SECRETP.secret=SECRETB"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("api_key=SECRETP.sig=SECRETB"),
                String::from("api_key=[REDACTED]"),
            ),
            (
                String::from("bad config: {'credentials': 'secret=correct horse SECRETB staple'}"),
                String::from("bad config: {'credentials':'[REDACTED]'}"),
            ),
            (
                String::from("bad config: {'credential': 'token: abc SECRETB'}"),
                String::from("bad config: {'credential':'[REDACTED]'}"),
            ),
            (
                String::from(r#"credentials="token=abc SECRETB""#),
                String::from(r#"credentials="[REDACTED]""#),
            ),
            (
                String::from(r#"CREDENTIALS="secret=correct horse SECRETB""#),
                String::from(r#"CREDENTIALS="[REDACTED]""#),
            ),
            (
                String::from("credentials: SECRETP-token:SECRETB"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from("credentials: 'secret=correct horse SECRETB'"),
                String::from("credentials:'[REDACTED]'"),
            ),
            (
                String::from(r#"failed: password: "password""#),
                String::from(r#"failed: password:"[REDACTED]""#),
            ),
            (
                String::from(r#"failed: password: "SECRET123": invalid"#),
                String::from(r#"failed: password:"[REDACTED]": invalid"#),
            ),
            (
                String::from("credentials: token= SECRET123"),
                String::from("credentials: token=[REDACTED]"),
            ),
            (
                String::from("failed: password: SECRET123 : retry"),
                String::from("failed: password:[REDACTED] : retry"),
            ),
            (
                String::from("failed: password=SECRET123:[REDACTED]"),
                String::from("failed: password=[REDACTED]"),
            ),
            (
                String::from("credentials:\n  app:client_secret: SECRET123"),
                String::from("credentials:\n  app:client_secret:[REDACTED]"),
            ),
            (
                String::from("failed: password: SECRET123token: retry"),
                String::from("failed: password:[REDACTED] retry"),
            ),
            (
                String::from("password=SECRETP-token:"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=SECRETP/token="),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("api_key=SECRETAbC+xYz/Sig="),
                String::from("api_key=[REDACTED]"),
            ),
            (
                String::from("api_key=SECRETAbC+xYz/Sig=\n"),
                String::from("api_key=[REDACTED]\n"),
            ),
            (
                String::from("api_key=SECRETAbC+xYz/Sig= "),
                String::from("api_key=[REDACTED] "),
            ),
            (
                String::from("password=SECRETP-token:,next"),
                String::from("password=[REDACTED],next"),
            ),
            (
                String::from("password=SECRETP-token:;next"),
                String::from("password=[REDACTED];next"),
            ),
            (
                String::from("password=SECRETP-token: ,next"),
                String::from("password=[REDACTED] ,next"),
            ),
            (
                String::from("password=SECRETP-token:&next=1"),
                String::from("password=[REDACTED]&next=1"),
            ),
            (
                String::from("password=SECRETP-token:\t;x"),
                String::from("password=[REDACTED]\t;x"),
            ),
            (
                String::from("password=SECRETP-token:\r\n"),
                String::from("password=[REDACTED]\r\n"),
            ),
            (
                String::from("password=SECRETP-token:\u{c}"),
                String::from("password=[REDACTED]\u{c}"),
            ),
            (
                String::from("failed: password=SECRETP-token: }"),
                String::from("failed: password=[REDACTED] }"),
            ),
            (
                String::from("failed: {Password:abc-token: }"),
                String::from("failed: {Password:[REDACTED] }"),
            ),
            (
                String::from("api_key=SECRETAbC+xYz/Sig= }"),
                String::from("api_key=[REDACTED] }"),
            ),
            (
                String::from("password=SECRETP-token: ]"),
                String::from("password=[REDACTED] ]"),
            ),
            (
                String::from("failed: [{Password:abc-token: } {User:bob}]"),
                String::from("failed: [{Password:[REDACTED] } {User:bob}]"),
            ),
            (
                String::from("password=abc-token: ..."),
                String::from("password=[REDACTED] ..."),
            ),
            (
                String::from("credentials: token = ]SECRETZ"),
                String::from("credentials: token=[REDACTED]"),
            ),
            (
                String::from("credentials:\n  password: }SECRETZ"),
                String::from("credentials:\n  password:[REDACTED]"),
            ),
            (
                String::from("failed: {Credentials:token: ]SECRETZ}"),
                String::from("failed: {Credentials:token:[REDACTED]}"),
            ),
            (
                String::from(r#"credentials: token: " SECRETX""#),
                String::from(r#"credentials: token:"[REDACTED]""#),
            ),
            (
                String::from("credentials: token: ' SECRETX'"),
                String::from("credentials: token:'[REDACTED]'"),
            ),
            (
                String::from(r#"credentials: token: \" SECRETX\""#),
                String::from(r#"credentials: token:\"[REDACTED]\""#),
            ),
            (
                String::from("credentials:\n  token: \" SECRETX\"\n  region: us"),
                String::from("credentials:\n  token:\"[REDACTED]\"\n  region: us"),
            ),
            (
                String::from(r#"failed: "password=SECRETP-token: ""#),
                String::from(r#"failed: "password=[REDACTED] ""#),
            ),
            (
                String::from("failed: 'password=SECRETP-token: '"),
                String::from("failed: 'password=[REDACTED] '"),
            ),
            (
                String::from(r#"failed: \"password=SECRETP-token: \""#),
                String::from(r#"failed: \"password=[REDACTED] \""#),
            ),
            (
                String::from(r#"parsing "password=SECRETP-token: ": invalid syntax"#),
                String::from(r#"parsing "password=[REDACTED] ": invalid syntax"#),
            ),
            (
                String::from(r#"failed: "password=SECRETP-token: " for user "bob""#),
                String::from(r#"failed: "password=[REDACTED] " for user "bob""#),
            ),
            (
                String::from(r#"failed: "password=SECRETP-token: " for user "Émile""#),
                String::from(r#"failed: "password=[REDACTED] " for user "Émile""#),
            ),
            (
                String::from(r#"got "password=SECRETP-token: ", want """#),
                String::from(r#"got "password=[REDACTED] ", want """#),
            ),
            (
                String::from(r#"got "password=SECRETP-token: ", want "2""#),
                String::from(r#"got "password=[REDACTED] ", want "2""#),
            ),
            (
                String::from(r#"got "password=SECRETP-token: ", want "_id""#),
                String::from(r#"got "password=[REDACTED] ", want "_id""#),
            ),
            (
                String::from(r#"got \"password=SECRETP-token: \", want \"\""#),
                String::from(r#"got \"password=[REDACTED] \", want \"\""#),
            ),
            (
                String::from("credentials:\n  password: \" SECRET123\"\n  region: us"),
                String::from("credentials:\n  password:\"[REDACTED]\"\n  region: us"),
            ),
            (
                String::from("credentials: 'token': ' SECRET123'"),
                String::from("credentials: 'token':'[REDACTED]'"),
            ),
            (
                String::from(r#"credentials: token = ". SECRET123""#),
                String::from(r#"credentials: token="[REDACTED]""#),
            ),
            (
                String::from(r#"credentials: token: "} SECRET123""#),
                String::from(r#"credentials: token:"[REDACTED]""#),
            ),
            (
                String::from(r#"failed: {Credentials:token: " SECRET123"}"#),
                String::from(r#"failed: {Credentials:token:"[REDACTED]"}"#),
            ),
            (
                String::from(r#"failed: [{Credentials:token: " SECRET123"}]"#),
                String::from(r#"failed: [{Credentials:token:"[REDACTED]"}]"#),
            ),
            (
                String::from(r#"failed: {Password:SECRET123-token: "}"#),
                String::from(r#"failed: {Password:[REDACTED] "}"#),
            ),
            (
                String::from("failed: {Password:SECRET123-token: '}"),
                String::from("failed: {Password:[REDACTED] '}"),
            ),
            (
                String::from(r#"failed: {Password:SECRET123-token: ""}"#),
                String::from(r#"failed: {Password:[REDACTED] ""}"#),
            ),
            (
                String::from(r#"failed: map[Password:SECRET123-token: ""]"#),
                String::from(r#"failed: map[Password:[REDACTED] ""]"#),
            ),
            (
                String::from(r#"password=SECRET123-token: \"#),
                String::from(r#"password=[REDACTED] \"#),
            ),
            (
                String::from(r#"credentials: password="correct SECRET123 battery""#),
                String::from(r#"credentials: password="[REDACTED]""#),
            ),
            (
                String::from(r#"credentials: token=" SECRET123""#),
                String::from(r#"credentials: token="[REDACTED]""#),
            ),
            (
                String::from(r#"credentials: token:"Bearer SECRET123""#),
                String::from(r#"credentials: token:"[REDACTED]""#),
            ),
            (
                String::from("credentials: token='a,SECRET123'"),
                String::from("credentials: token='[REDACTED]'"),
            ),
            (
                String::from(r#"credentials: token="a;SECRET123" more"#),
                String::from(r#"credentials: token="[REDACTED]" more"#),
            ),
            (
                String::from(r#"credentials: token=\"a SECRET123\""#),
                String::from(r#"credentials: token=\"[REDACTED]\""#),
            ),
            (
                String::from(r#"password: token:"a SECRET123""#),
                String::from(r#"password: token:"[REDACTED]""#),
            ),
            (
                String::from(r#"failed: {Credentials:token=" SECRET123"}"#),
                String::from(r#"failed: {Credentials:token="[REDACTED]"}"#),
            ),
            (
                String::from(r#"failed: map[credentials:token="a SECRET123"]"#),
                String::from(r#"failed: map[credentials:token="[REDACTED]"]"#),
            ),
            (
                String::from("credentials:\n  token=\"a SECRET123\"\n  region: us"),
                String::from("credentials:\n  token=\"[REDACTED]\"\n  region: us"),
            ),
            (
                String::from("credentials: token=SECRET123"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from(r#"credentials: token="SECRET123""#),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from(r#"got "password=SECRETP-token=", want "abc""#),
                String::from(r#"got "password=[REDACTED], want "abc""#),
            ),
            (
                String::from(r#"credentials: user="SECRET123 token=SECRET456""#),
                String::from("credentials:[REDACTED] token=[REDACTED]"),
            ),
            (
                String::from(r#"credentials: token("SECRET123 token=SECRET456")"#),
                String::from("credentials:[REDACTED] token=[REDACTED]"),
            ),
            (
                String::from(r#"password="SECRETQ-token" :"#),
                String::from(r#"password="[REDACTED]" :"#),
            ),
            (
                String::from("password=SECRETQ-token :"),
                String::from("password=[REDACTED] :"),
            ),
            (
                String::from("password=SECRETP-token\t:"),
                String::from("password=[REDACTED]\t:"),
            ),
            (
                String::from("password = SECRETP-token:"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=temp:SECRETT:"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=secret:"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=Secret:"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("secret: SECRETR.password="),
                String::from("secret:[REDACTED]"),
            ),
            (
                String::from("credentials: token\n= SECRETA"),
                String::from("credentials: token=[REDACTED]"),
            ),
            (
                String::from("credentials: token\t= SECRETA"),
                String::from("credentials: token=[REDACTED]"),
            ),
            (
                String::from("credentials: token : SECRETA"),
                String::from("credentials: token:[REDACTED]"),
            ),
            (
                String::from("password=temp:SECRET123-token: dial tcp"),
                String::from("password=[REDACTED] tcp"),
            ),
            (
                String::from("password=temp:SECRET123: dial tcp"),
                String::from("password=[REDACTED] tcp"),
            ),
            (
                String::from("password=user:temp:SECRET123: dial tcp"),
                String::from("password=[REDACTED] tcp"),
            ),
            (
                String::from("password=app:temp:SECRET123: dial tcp"),
                String::from("password=[REDACTED] tcp"),
            ),
            (
                String::from(r#"password="temp:SECRET123": dial tcp"#),
                String::from(r#"password="[REDACTED]" tcp"#),
            ),
            (
                String::from("credentials: temp:foo: SECRET123"),
                String::from("credentials:[REDACTED]"),
            ),
            (
                String::from("credentials:\n  temp:foo: SECRET123"),
                String::from("credentials:\n  temp:foo:[REDACTED]"),
            ),
            (
                String::from("credentials:\n  user:temp:foo: SECRET123"),
                String::from("credentials:\n  user:temp:foo:[REDACTED]"),
            ),
            (
                String::from(
                    r#"{"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\"","code":"token=abc"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login [400]: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123"}"#),
                String::from(UNPARSEABLE_JSON_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
                ),
                String::from(UNPARSEABLE_JSON_PLACEHOLDER),
            ),
            (
                String::from(r#"POST /v1/login: {"msg":"retry\npassword=SECRET123"} (status 400)"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password=SECRET123"}"#),
                String::from(r#"{"msg":"password=[REDACTED]}"#),
            ),
            (
                String::from(r#"{"msg":"retry\npassword=dev-SECRET123-password: dial"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"{"msg":"retry\npassword=hunter2val token=abc-SECRET123-token: dial"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"["retry\npassword=dev-SECRET123-password: dial"]"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"retry\npassword=dev-SECRET123-password"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"["retry\npassword=dev-SECRET123-password"]"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"bad token " in body: {"msg":"retry\npassword=SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"flag \"foo: {"msg":"retry\npassword=SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"err: "dir C:\work\"retry\npassword=SECRET123""#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"upstream error: {"error":"login failed\npassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{'error': 'retry\npassword=SECRET123'}"#),
                String::from(UNPARSEABLE_JSON_PLACEHOLDER),
            ),
            (
                String::from(r#"bad config: {'error': 'retry\npassword=SECRET123'}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"err=retry\npassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"retry\bpassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"retry\fpassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"retry\npassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"retry\rpassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"retry\tpassword=SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"https:\/\/user:SECRET123@example.com"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [
                    "Basic%20",
                    base64_std(
                        [
                            "a".repeat(300).as_str(),
                            ":SECRET123:",
                            "b".repeat(285).as_str(),
                        ]
                        .concat()
                        .as_bytes(),
                    )
                    .as_str(),
                ]
                .concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [
                    r#"Basic\t"#,
                    base64_std(
                        [
                            "a".repeat(300).as_str(),
                            ":SECRET123:",
                            "b".repeat(285).as_str(),
                        ]
                        .concat()
                        .as_bytes(),
                    )
                    .as_str(),
                ]
                .concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [
                    "https%3A//maps.googleapis.com/maps/api/geocode/json?address=",
                    "a".repeat(300).as_str(),
                    "&key=SECRET123&sensor=",
                    "b".repeat(300).as_str(),
                ]
                .concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [
                    r#"https:\/\/maps.googleapis.com/maps/api/geocode/json?address="#,
                    "a".repeat(300).as_str(),
                    "&key=SECRET123&sensor=",
                    "b".repeat(300).as_str(),
                ]
                .concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [
                    "https%3A//",
                    "a".repeat(260).as_str(),
                    ":SECRET123@example.com/",
                    "b".repeat(260).as_str(),
                ]
                .concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [r#"\"#.repeat(256).as_str(), "npassword=SECRET123"].concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [r#"\"#.repeat(256).as_str(), "tpassword=SECRET123"].concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                ["password", r#"\"#.repeat(256).as_str(), "u003dSECRET123"].concat(),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                [r#"\"#.repeat(600).as_str(), r#""password": "SECRET123""#].concat(),
                [r#"\"#.repeat(600).as_str(), r#""password":"[REDACTED]""#].concat(),
            ),
            (
                String::from(r#"Bearer\\u0020SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"{"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password:\n SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"POST /v1/login: 400: {"msg":"password:\n SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"bad config: {'error': 'token=abc\npassword: SECRET123'}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"upstream error: {"error":"token=abc\npassword: SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"token=abc\"password\": SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"upstream failed: "token=abc\npassword: SECRET123""#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"POST /v1/login: 400: {"msg":"token=abc%0Apassword: SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"token=abc\\u000apassword: SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"token=abc\npassword\u003a SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"token=abc\u005cnpassword: SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"token=abc\u005cnpassword: SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"token=abc%5Cnpassword: SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"POST /v1/login: 400: {"msg":"token=abc\x5cnpassword: SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(
                    r#"POST /v1/login: 400: {"msg":"token=abc\u005c\u006epassword: SECRET123"}"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password:\b SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"POST /v1/login: 400: {"msg":"password:\b SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"password:\b SECRET123"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password:\u0008 SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password:\u000b SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"{"msg":"password:\u0000 SECRET123"}"#),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from("failed: {Password:abc-token: }\u{8}SECRET123"),
                String::from("failed: {Password:[REDACTED]"),
            ),
            (
                String::from("failed: {Password:abc-token: \u{8}}"),
                String::from("failed: {Password:[REDACTED] \u{8}}"),
            ),
            (
                String::from("credentials:\n  password=SECRET123-token: OTHER456"),
                String::from("credentials:\n  password=[REDACTED]"),
            ),
            (
                String::from(r#"password=abc-token: " SECRET123""#),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from("password=abc-token: ' SECRET123'"),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from(r#"password=abc-token: \" SECRET123\""#),
                String::from("password=[REDACTED]"),
            ),
            (
                String::from(
                    r#"password:\n [REDACTED]a[REDACTED]u[REDACTED]t[REDACTED]h[REDACTED] authorization=1 authorization=2"#,
                ),
                String::from(REDACTED_SENSITIVE_TEXT_PLACEHOLDER),
            ),
            (
                String::from(r#"GET https://example.com?token=SECRET123 hello\nworld"#),
                String::from(r#"GET https://example.com?token=%5BREDACTED%5D hello\nworld"#),
            ),
        ];
        for (raw, want) in &tests {
            let (got, lost) = sanitize_raw_text(raw);
            assert_eq!((&got, lost), (want, true), "sanitize_raw_text({raw:?})");
            let (trunc_json, truncated) =
                crate::formatter::smart_truncate(raw.as_str(), 4096).unwrap();
            let trunc: String = serde_json::from_slice(&trunc_json).unwrap();
            assert!(
                trunc == *want && truncated,
                "smart_truncate({raw:?}) = ({}, {truncated}), want ({want:?}, true)",
                String::from_utf8_lossy(&trunc_json)
            );
        }
        for depth in [5, 9] {
            let mut nested = String::from(r#""retry\npassword=SECRET123""#);
            for _ in 0..depth {
                nested = go_json_string(&nested);
            }
            let prefixed = format!("upstream: {nested}");
            assert_eq!(
                redact_error_message(&prefixed),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
                "redact_error_message({depth}-deep)"
            );
            let (got, lost) = sanitize_raw_text(&prefixed);
            assert_eq!(
                (got.as_str(), lost),
                (REDACTED_SENSITIVE_TEXT_PLACEHOLDER, true),
                "sanitize_raw_text({depth}-deep)"
            );
        }
        assert_eq!(
            scrub_error_text(r#"{"msg":"password=SECRET123"}"#),
            r#"{"msg":"password=[REDACTED]}"#,
            "scrub_error_text unescaped JSON pair"
        );
        let harmless_samples: Vec<String> = vec![
            String::from(r#"C:\new\file.txt"#),
            String::from(r#"{"msg":"talk to Ken\ntomorrow"}"#),
            String::from(r#"GET [REDACTED_SENSITIVE_URI] [REDACTED] %5BREDACTED%5D hello\nworld"#),
            [r#"{"sep":""#, r#"\\"#.repeat(128).as_str(), r#""}"#].concat(),
            [r#"{"sep":""#, r#"\\"#.repeat(300).as_str(), r#""}"#].concat(),
            [r#"""#, r#"\"#.repeat(300).as_str(), r#"""#].concat(),
            [r#"hello: ""#, r#"\"#.repeat(256).as_str(), r#"""#].concat(),
            ["design review notes; ", r#"\"#.repeat(256).as_str(), "next"].concat(),
            [r#"\"#.repeat(256).as_str(), r#""token"#].concat(),
            [r#"\"#.repeat(256).as_str(), "'token"].concat(),
            ["path ", r#"\"#.repeat(256).as_str(), "/end"].concat(),
            ["mytoken=1", " ".repeat(56).as_str(), "a-b: x"].concat(),
            ["foosig: 1", " ".repeat(52).as_str(), "temp: "].concat(),
        ];
        for harmless in &harmless_samples {
            assert_eq!(
                &redact_error_message(harmless),
                harmless,
                "redact_error_message({harmless:?})"
            );
            let (got, lost) = sanitize_raw_text(harmless);
            assert_eq!(
                (&got, lost),
                (harmless, false),
                "sanitize_raw_text({harmless:?})"
            );
        }
        let pairs: Vec<(String, String)> = vec![
            (
                String::from(r#"{"msg":"hello\nworld","token":"SECRET123"}"#),
                String::from(r#"{"msg":"hello\nworld","token":"[REDACTED]"}"#),
            ),
            (
                String::from(r#"{"path":"C:\\users\\deadbeef","token":"SECRET123"}"#),
                String::from(r#"{"path":"C:\\users\\deadbeef","token":"[REDACTED]"}"#),
            ),
            (
                String::from(r#"{"msg":"hello [REDACTED]\nworld","token":"SECRET123"}"#),
                String::from(r#"{"msg":"hello [REDACTED]\nworld","token":"[REDACTED]"}"#),
            ),
            (
                [
                    r#"{"sep":""#,
                    r#"\\"#.repeat(128).as_str(),
                    r#"","token":"SECRET123"}"#,
                ]
                .concat(),
                [
                    r#"{"sep":""#,
                    r#"\\"#.repeat(128).as_str(),
                    r#"","token":"[REDACTED]"}"#,
                ]
                .concat(),
            ),
        ];
        for (raw, want) in &pairs {
            let (got, lost) = sanitize_raw_text(raw);
            assert_eq!((&got, lost), (want, false), "sanitize_raw_text({raw:?})");
        }
        assert_eq!(
            nested_pair_end("token=abc rest", "token=abc ".len(), 0),
            "token=abc ".len(),
            "nested_pair_end fallback"
        );
    }

    // quoted fragments are inspected in linear time
    {
        let filler = "b".repeat(100_000);
        let samples = [
            // Each fragment fails to decode at its line break.
            (
                "broken fragments",
                [r#""a" "#, "\"x\n".repeat(64).as_str(), filler.as_str()].concat(),
            ),
            // A key word lets the text through the credential pattern
            // prefilter, so scanning the rest of the text again for each
            // fragment would show.
            (
                "broken fragments with a key word",
                [r#""a" "#, "\"token\n".repeat(64).as_str(), filler.as_str()].concat(),
            ),
            // The fragment never ends, and each escaped quote in it starts
            // another fragment that would be decoded to the end of the text.
            (
                "unterminated fragment with escaped quotes",
                [r#""a" ""#, r#"x\""#.repeat(62).as_str(), filler.as_str()].concat(),
            ),
        ];
        for (name, text) in &samples {
            let ((got, lost), d) = timed(|| sanitize_raw_text(text));
            assert!(
                d <= TIME_LIMIT,
                "{name}: sanitize_raw_text took {d:?} for a 100 KB text"
            );
            assert!(
                &got == text && !lost,
                "{name}: sanitize_raw_text changed text without credentials (lost = {lost})"
            );
        }
    }

    // nested sensitive keys are inspected in linear time
    {
        let samples = [
            // Each value is the next key, so each pair is matched again from
            // its value.
            (
                "keys separated by spaces",
                ["token: ".repeat(3_000).as_str(), "SECRET123"].concat(),
            ),
            (
                "credentials blocks",
                [
                    "credentials:\n  ".repeat(2_000).as_str(),
                    "token: SECRET123",
                ]
                .concat(),
            ),
            // The value ends with a delimiter and a value follows it, as for a
            // key, but it is too long to be one, so it is redacted whole
            // rather than matched again from each key in it.
            (
                "keys without spaces",
                [
                    "password=SECRET123-",
                    "token:".repeat(10_000).as_str(),
                    " x",
                ]
                .concat(),
            ),
        ];
        for (name, text) in &samples {
            let ((got, lost), d) = timed(|| sanitize_raw_text(text));
            assert!(
                d <= TIME_LIMIT,
                "{name}: sanitize_raw_text took {d:?} for a {}-byte text",
                text.len()
            );
            assert!(
                !got.contains("SECRET123"),
                "{name}: sanitize_raw_text kept the credential"
            );
            assert!(lost, "{name}: sanitize_raw_text lost = false, want true");
        }
    }

    // pattern words with long backslash runs are inspected in linear time
    {
        let unchanged = [
            (
                "300 backslashes",
                ["design review notes; ", r#"\"#.repeat(300).as_str(), " end"].concat(),
            ),
            (
                "50KB prose",
                [
                    "design review notes; ".repeat(2_500).as_str(),
                    r#"\"#.repeat(256).as_str(),
                    " end",
                ]
                .concat(),
            ),
            (
                "105KB prose",
                [
                    "design review notes; ".repeat(5_000).as_str(),
                    r#"\"#.repeat(256).as_str(),
                    " end",
                ]
                .concat(),
            ),
            (
                "55KB multiline colon prose with backslashes",
                [
                    "design: review=notes;\n".repeat(2_500).as_str(),
                    r#"\"#.repeat(256).as_str(),
                    " end",
                ]
                .concat(),
            ),
            (
                "55KB multiline colon prose with quoted backslashes",
                [
                    "design: review=notes;\n".repeat(2_500).as_str(),
                    r#"""#,
                    r#"\"#.repeat(300).as_str(),
                    r#"""#,
                ]
                .concat(),
            ),
            (
                "110KB multiline colon prose with quoted backslashes",
                [
                    "design: review=notes;\n".repeat(5_000).as_str(),
                    r#"""#,
                    r#"\"#.repeat(300).as_str(),
                    r#"""#,
                ]
                .concat(),
            ),
            (
                "53KB contractions and basic prose with quoted backslashes",
                [
                    "a basic user's design; ".repeat(2_300).as_str(),
                    r#"""#,
                    r#"\"#.repeat(300).as_str(),
                    r#"""#,
                ]
                .concat(),
            ),
            (
                "105KB quoted colons with Windows path",
                [
                    r#"see "design": user notes; "#.repeat(4_100).as_str(),
                    r#"C:\\users\\alice"#,
                ]
                .concat(),
            ),
        ];
        for (name, text) in &unchanged {
            let (got, d) = timed(|| redact_error_message(text));
            assert!(
                &got == text,
                "{name}: redact_error_message redacted clean text (len={})",
                got.len()
            );
            assert!(
                d <= TIME_LIMIT,
                "{name}: redact_error_message took {d:?} for a {}-byte text",
                text.len()
            );
            let ((got, lost), d) = timed(|| sanitize_raw_text(text));
            assert!(
                d <= TIME_LIMIT,
                "{name}: sanitize_raw_text took {d:?} for a {}-byte text",
                text.len()
            );
            assert!(
                &got == text && !lost,
                "{name}: sanitize_raw_text modified clean text (len={}, lost={lost})",
                got.len()
            );
        }

        let with_cred = [
            "design review notes; ".repeat(2_500).as_str(),
            r#"retry\npassword=SECRET123"#,
        ]
        .concat();
        let ((got, lost), d) = timed(|| sanitize_raw_text(&with_cred));
        assert!(
            d <= TIME_LIMIT,
            "escaped credential after 50KB prose: sanitize_raw_text took {d:?}"
        );
        assert_eq!(
            (got.as_str(), lost),
            (REDACTED_SENSITIVE_TEXT_PLACEHOLDER, true),
            "escaped credential after 50KB prose: sanitize_raw_text"
        );
    }

    // keys quoted with escapes and padded with a Unicode space fail closed
    {
        // The quotes around each key are written as escapes, and a Unicode
        // space before the delimiter keeps the decoded pair from matching
        // the credential patterns, so only the check for a quoted sensitive
        // key in the decoded text finds it.
        let samples = [
            "[\\u0022AUTHORIZATION\\u0022\u{a0}: \\u0022SECRET123\\u0022]",
            "{\\u0022password\\u0022\u{a0}: \\u0022SECRET123\\u0022}",
            "[%22password%22\u{3000}: %22SECRET123%22]",
            "{\\x22api_key\\x22\u{a0}= \\x22SECRET123\\x22}",
            "{\\u0027secret\\u0027\u{2009}:\\u0027SECRET123\\u0027}",
            // The same with literal quotes, and without the Unicode space.
            "[\"password\"\u{a0}: \"SECRET123\"]",
            "[\\u0022password\\u0022: \\u0022SECRET123\\u0022]",
        ];
        for s in samples {
            let (got, lost) = sanitize_raw_text(s);
            assert_eq!(
                (got.as_str(), lost),
                (UNPARSEABLE_JSON_PLACEHOLDER, true),
                "sanitize_raw_text({s:?})"
            );
            assert_eq!(
                redact_error_message(s),
                UNPARSEABLE_JSON_PLACEHOLDER,
                "redact_error_message({s:?})"
            );
        }
    }

    // escaped keys and delimiters across long whitespace runs fail closed
    {
        // Only the spans of a text over 8 KiB around its escapes are
        // decoded, so each span must reach across the whitespace between
        // the key, the delimiter and the value, however long it is.
        let prose = "lorem ipsum dolor sit amet\n".repeat(340);
        let spaces = |n: usize| " ".repeat(n);
        // (name, raw, want): want is the result of sanitize_raw_text,
        // redact_error_message and SmartTruncate. scrub_error_text returns
        // REDACTED_SENSITIVE_TEXT_PLACEHOLDER for every raw.
        let tests = [
            (
                "escaped key, spaces, delimiter",
                ["pass\\u0077ord", spaces(9000).as_str(), ": SECRET123"].concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key and delimiter, spaces",
                ["pass\\u0077ord:", spaces(9000).as_str(), "SECRET123"].concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "key, spaces, escaped delimiter",
                ["password", spaces(9000).as_str(), "\\u003a SECRET123"].concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key, spaces around the delimiter",
                [
                    prose.as_str(),
                    "pass\\u0077ord",
                    spaces(6000).as_str(),
                    ":",
                    spaces(6000).as_str(),
                    "SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "long word, escaped key, spaces, delimiter",
                [
                    "x".repeat(8300).as_str(),
                    " pass\\u0077ord",
                    spaces(5000).as_str(),
                    ": SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key, spaces and a line break, delimiter",
                [
                    "pass\\u0077ord",
                    spaces(3000).as_str(),
                    "\n",
                    spaces(3000).as_str(),
                    ": SECRET123",
                    spaces(3000).as_str(),
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            // The span reaches 512 bytes past the escape before it is
            // widened.
            (
                "escaped key, 400 spaces and a line break, delimiter",
                [
                    prose.as_str(),
                    "pass\\u0077ord",
                    spaces(400).as_str(),
                    "\n: SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key, 513 spaces and a line break, delimiter",
                [
                    prose.as_str(),
                    "pass\\u0077ord",
                    spaces(513).as_str(),
                    "\n: SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key and delimiter, 520 spaces and a line break",
                [
                    prose.as_str(),
                    "pass\\u0077ord:",
                    spaces(520).as_str(),
                    "\nSECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "key and escaped delimiter, 600 spaces and a line break",
                [
                    prose.as_str(),
                    "password\\u003a",
                    spaces(600).as_str(),
                    "\nSECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key, 600 line breaks, delimiter",
                [
                    prose.as_str(),
                    "pass\\u0077ord",
                    "\n".repeat(600).as_str(),
                    ": SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped key and delimiter, 520 tabs and a line break",
                [
                    prose.as_str(),
                    "api\\u005fkey =",
                    "\t".repeat(520).as_str(),
                    "\nSECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "key, spaces, delimiter, spaces, value in escaped quotes",
                [
                    prose.as_str(),
                    "password",
                    spaces(6000).as_str(),
                    ":",
                    spaces(6000).as_str(),
                    "\\u0022a SECRET123\\u0022",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "escaped tab, bearer, spaces, token",
                [
                    "a ".repeat(5000).as_str(),
                    "\\tBearer",
                    spaces(9000).as_str(),
                    "SECRET123",
                ]
                .concat(),
                REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
            ),
            (
                "quoted key, 5,000 spaces, escaped delimiter, prose",
                [
                    "{\"password\"",
                    spaces(5000).as_str(),
                    "\\u003a \"SECRET123\"}",
                    " x".repeat(2000).as_str(),
                ]
                .concat(),
                UNPARSEABLE_JSON_PLACEHOLDER,
            ),
            // Spans are joined across a gap of up to 64 KiB between quotes.
            (
                "quoted key, 60,000 spaces, escaped delimiter",
                [
                    "{\"password\"",
                    spaces(60_000).as_str(),
                    "\\u003a \"SECRET123\"}",
                ]
                .concat(),
                UNPARSEABLE_JSON_PLACEHOLDER,
            ),
            (
                "quoted key, 70,000 spaces, escaped delimiter",
                [
                    "{\"password\"",
                    spaces(70_000).as_str(),
                    "\\u003a \"SECRET123\"}",
                ]
                .concat(),
                UNPARSEABLE_JSON_PLACEHOLDER,
            ),
        ];
        for (name, raw, want) in &tests {
            let (got, lost) = sanitize_raw_text(raw);
            assert!(
                got == *want && lost,
                "{name}: sanitize_raw_text = ({:?}, {lost}), want ({want:?}, true)",
                got.chars().take(80).collect::<String>()
            );
            let got = redact_error_message(raw);
            assert!(
                got == *want,
                "{name}: redact_error_message = {:?}, want {want:?}",
                got.chars().take(80).collect::<String>()
            );
            let got = scrub_error_text(raw);
            assert!(
                got == REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
                "{name}: scrub_error_text = {:?}, want {REDACTED_SENSITIVE_TEXT_PLACEHOLDER:?}",
                got.chars().take(80).collect::<String>()
            );
            let (trunc_json, _) = crate::formatter::smart_truncate(raw.as_str(), 1 << 20).unwrap();
            let trunc: String = serde_json::from_slice(&trunc_json).unwrap();
            assert!(
                trunc == *want,
                "{name}: smart_truncate = {:?}, want {want:?}",
                trunc.chars().take(80).collect::<String>()
            );
        }
    }

    // a quote after an even run of backslashes is not an escaped quote
    {
        // The backslashes escape each other, so the quote does not hide a
        // quoted key from the checks on the raw text, and prose that ends
        // a Windows path with it is kept.
        let prose = r#"[INFO] saved to "C:\\" ok"#;
        assert_eq!(
            redact_error_message(prose),
            prose,
            "redact_error_message({prose:?})"
        );
        // A key quoted that way is still found, as is one quoted with
        // escaped quotes.
        for s in [
            r#"[INFO] \\"password\\": \\"SECRET123\\""#,
            r#"[INFO] \"password\": \"SECRET123\""#,
        ] {
            let (got, lost) = sanitize_raw_text(s);
            assert_eq!(
                (got.as_str(), lost),
                (UNPARSEABLE_JSON_PLACEHOLDER, true),
                "sanitize_raw_text({s:?})"
            );
            assert_eq!(
                redact_error_message(s),
                UNPARSEABLE_JSON_PLACEHOLDER,
                "redact_error_message({s:?})"
            );
        }
    }

    // key-value pairs with one kind of delimiter are redacted in linear time
    {
        // Each text repeats sensitive key-value pairs that use only ":" or
        // only "=". Looking for the next delimiter of the other kind anew
        // after each match would scan the rest of the text once per match.
        //
        // Go divides the sizes by 4, or by 64 under the race detector, which
        // slows the scans about twentyfold. An unoptimized (debug) Rust build
        // runs all nine calls at Go's scale of 4 in about 4 s, too close to
        // the limit per call, so debug builds divide by 16 (about a quarter
        // of a second per call) and optimized builds by Go's 4. The 3-second
        // limit stays as in Go: a quadratic scan over the 256 KiB debug texts
        // (thousands of matches, each rescanning the rest) still exceeds it.
        let scale: usize = if cfg!(debug_assertions) { 16 } else { 4 };
        // (name, unit, size, kept): kept are the parts of unit that
        // redaction must remove.
        let cases: [(&str, &str, usize, &[&str]); 3] = [
            ("colons", "token: a: b: c: d ", 4 << 20, &["token: a"]),
            (
                "equals signs",
                "'a'='b' secret='' ",
                4 << 20,
                &["secret=''"],
            ),
            // Both the JSON and the text pass match each pair with a
            // regular expression, which takes longer per byte.
            (
                "JSON members",
                r#""token": "x", "password": "y", "#,
                1 << 20,
                &[r#""x""#, r#""y""#],
            ),
        ];
        type Redactor = (&'static str, fn(&str) -> String);
        let redactors: [Redactor; 3] = [
            ("sanitizeRawText", |s| sanitize_raw_text(s).0),
            ("redactErrorMessage", redact_error_message),
            ("scrubErrorText", scrub_error_text),
        ];
        for (name, unit, size, kept) in cases {
            let text = unit.repeat(size / scale / unit.len());
            for (redact_name, redact) in redactors {
                let (got, d) = timed(|| redact(&text));
                assert!(
                    d <= TIME_LIMIT,
                    "{name}: {redact_name} took {d:?} for a {}-byte text",
                    text.len()
                );
                for k in kept {
                    assert!(!got.contains(k), "{name}: {redact_name} kept {k:?}");
                }
            }
        }
    }
}
