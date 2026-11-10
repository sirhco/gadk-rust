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

//! Helpers shared by the artifact services (gadk-artifact's in-memory service and
//! gadk-artifact-gcs): the `request validation failed: ` wrapping, Go's `fmt.Sprint` of a
//! custom-metadata value, the `user:` file-name namespace check, Go's `url.PathEscape` and the
//! JSON API's URL path-segment escape.

use std::fmt::Write as _;

use serde_json::Value;

use crate::service::ArtifactError;

/// Wraps a validation error as Go's services do: `fmt.Errorf("request validation failed: %w",
/// err)`. Any other error is returned unchanged.
pub fn validation_failed(err: ArtifactError) -> ArtifactError {
    match err {
        ArtifactError::InvalidRequest(m) => {
            ArtifactError::InvalidRequest(format!("request validation failed: {m}"))
        }
        other => other,
    }
}

/// Port of Go fileHasUserNamespace: whether `file_name` names a user-scoped artifact (it
/// starts with `user:`).
pub fn file_has_user_namespace(file_name: &str) -> bool {
    file_name.starts_with("user:")
}

/// Port of Go `fmt.Sprint(value)` for a custom-metadata value. Arrays and objects use their
/// JSON text where Go would print `[...]` / `map[...]`.
pub fn go_sprint(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "<nil>".to_owned(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// Port of Go net/url PathEscape: keeps ALPHA / DIGIT / "-_.~" / "$&+:=@", percent-encodes
/// every other byte (upper-case hex), so `/` becomes `%2F` and a space `%20`.
pub fn go_path_escape(s: &str) -> String {
    percent_escape(s, |b| matches!(b, b'$' | b'&' | b'+' | b':' | b'=' | b'@'))
}

/// Port of the simple-expansion escape of Go's `googleapi.Expand` (google.golang.org/api
/// `internal/third_party/uritemplates`, `escape(s, false)`), which builds the JSON API URL
/// path segments: keeps only the unreserved ALPHA / DIGIT / "-._~" and percent-encodes every
/// other byte (upper-case hex), so `:` becomes `%3A`, `+` `%2B` and `/` `%2F`.
pub fn go_uri_template_escape(s: &str) -> String {
    percent_escape(s, |_| false)
}

/// Percent-encodes (upper-case hex) every byte of `s` but ALPHA / DIGIT / "-._~" and the bytes
/// `also_keep` accepts.
fn percent_escape(s: &str, also_keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || also_keep(b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn validation_failed_prefixes_invalid_requests_only() {
        let err = validation_failed(ArtifactError::InvalidRequest(
            "invalid save request: missing required fields: AppName".into(),
        ));
        assert_eq!(
            err.to_string(),
            "request validation failed: invalid save request: missing required fields: AppName"
        );
        assert!(matches!(
            validation_failed(ArtifactError::NotFound),
            ArtifactError::NotFound
        ));
    }

    #[test]
    fn file_has_user_namespace_checks_the_prefix() {
        assert!(file_has_user_namespace("user:file.txt"));
        assert!(file_has_user_namespace("user:"));
        assert!(!file_has_user_namespace("file.txt"));
        assert!(!file_has_user_namespace("User:file.txt"));
        assert!(!file_has_user_namespace("a/user:file"));
    }

    #[test]
    fn go_sprint_matches_fmt_sprint() {
        for (value, want) in [
            (json!("value"), "value"),
            (json!(42), "42"),
            (json!(-7), "-7"),
            (json!(1.5), "1.5"),
            (json!(true), "true"),
            (json!(false), "false"),
            (json!(null), "<nil>"),
            (json!([1, "a"]), r#"[1,"a"]"#),
            (json!({"k": "v"}), r#"{"k":"v"}"#),
        ] {
            assert_eq!(go_sprint(&value), want, "{value}");
        }
    }

    #[test]
    fn go_path_escape_matches_url_path_escape() {
        for (input, want) in [
            ("", ""),
            ("abcXYZ019", "abcXYZ019"),
            ("-_.~$&+:=@", "-_.~$&+:=@"),
            ("a b", "a%20b"),
            ("app/one", "app%2Fone"),
            ("100%", "100%25"),
            ("q?;,#", "q%3F%3B%2C%23"),
            ("résumé", "r%C3%A9sum%C3%A9"),
            ("user:file.txt/1", "user:file.txt%2F1"),
        ] {
            assert_eq!(go_path_escape(input), want, "PathEscape({input:?})");
        }
    }

    #[test]
    fn go_uri_template_escape_keeps_only_unreserved() {
        for (input, want) in [
            ("", ""),
            ("abcXYZ019-_.~", "abcXYZ019-_.~"),
            ("$&+:=@", "%24%26%2B%3A%3D%40"),
            ("user:a+b=c@d.txt", "user%3Aa%2Bb%3Dc%40d.txt"),
            ("a b/c", "a%20b%2Fc"),
            ("100%", "100%25"),
            ("!*'();,?#[]", "%21%2A%27%28%29%3B%2C%3F%23%5B%5D"),
            ("résumé", "r%C3%A9sum%C3%A9"),
        ] {
            assert_eq!(go_uri_template_escape(input), want, "{input:?}");
        }
    }
}
