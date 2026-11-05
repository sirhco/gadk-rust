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

//! Request canonicalization (the replay lookup key) and unmatched-request diffs.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};

/// Replay lookup key: `(METHOD, URL, canonical body)`.
pub(crate) type Key = (String, String, Vec<u8>);

/// Builds the lookup key for a request.
pub(crate) fn key(method: &str, url: &str, body: &[u8]) -> Key {
    (
        method.to_uppercase(),
        canonical_url(url),
        canonical_body(body),
    )
}

/// Canonicalizes a request body for matching.
///
/// A JSON body is re-serialized compactly with object keys sorted and numbers
/// normalized (an integral number that fits in `i64` is written as an integer, so
/// Rust's `0.0` matches Go's `0`). Any other body is returned unchanged.
pub fn canonical_body(body: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => serde_json::to_vec(&canonical_value(v)).unwrap_or_else(|_| body.to_vec()),
        Err(_) => body.to_vec(),
    }
}

fn canonical_value(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<String, Value> = m
                .into_iter()
                .map(|(k, v)| (k, canonical_value(v)))
                .collect();
            Value::Object(sorted.into_iter().collect::<Map<String, Value>>())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(canonical_value).collect()),
        Value::Number(n) => Value::Number(canonical_number(n)),
        other => other,
    }
}

fn canonical_number(n: Number) -> Number {
    if n.is_i64() || n.is_u64() {
        return n;
    }
    match n.as_f64() {
        Some(f) if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 => {
            Number::from(f as i64)
        }
        Some(f) => Number::from_f64(f).unwrap_or(n),
        None => n,
    }
}

/// Canonicalizes a URL for matching: removes any `key` query parameter (API keys
/// are scrubbed from recorded traces).
pub(crate) fn canonical_url(url: &str) -> String {
    let (base, rest) = match url.split_once('?') {
        Some((b, r)) => (b, r),
        None => return url.to_string(),
    };
    let (query, fragment) = match rest.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (rest, None),
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && p.split('=').next() != Some("key"))
        .collect();
    let mut out = base.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Pretty text of a canonical body for diffing.
fn pretty(body: &[u8]) -> String {
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => serde_json::to_string_pretty(&v)
            .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned()),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}

/// A line diff between two bodies: `-` lines are only in `expected`, `+` lines
/// only in `actual`, and shared lines are indented by two spaces.
pub(crate) fn diff(expected: &[u8], actual: &[u8]) -> String {
    let (e, a) = (pretty(expected), pretty(actual));
    let ops = line_diff(
        &e.lines().collect::<Vec<_>>(),
        &a.lines().collect::<Vec<_>>(),
    );
    let mut out = String::new();
    for (op, line) in ops {
        out.push_str(match op {
            '-' => "- ",
            '+' => "+ ",
            _ => "  ",
        });
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Number of changed lines between two bodies (smaller is closer).
pub(crate) fn distance(expected: &[u8], actual: &[u8]) -> usize {
    let (e, a) = (pretty(expected), pretty(actual));
    line_diff(
        &e.lines().collect::<Vec<_>>(),
        &a.lines().collect::<Vec<_>>(),
    )
    .iter()
    .filter(|(op, _)| *op != ' ')
    .count()
}

/// Largest LCS table (cells) built; bigger inputs get a plain remove-all/add-all diff.
const MAX_LCS_CELLS: usize = 4_000_000;

/// LCS-based line diff.
fn line_diff<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (a.len(), b.len());
    if (n + 1).saturating_mul(m + 1) > MAX_LCS_CELLS {
        let mut out: Vec<(char, &str)> = a.iter().map(|l| ('-', *l)).collect();
        out.extend(b.iter().map(|l| ('+', *l)));
        return out;
    }
    // lcs[i][j] = LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(('-', a[i]));
            i += 1;
        } else {
            out.push(('+', b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| ('-', *l)));
    out.extend(b[j..].iter().map(|l| ('+', *l)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_key_param() {
        assert_eq!(canonical_url("https://h/p?key=x"), "https://h/p");
        assert_eq!(
            canonical_url("https://h/p?alt=sse&key=x"),
            "https://h/p?alt=sse"
        );
        assert_eq!(canonical_url("https://h/p?keys=1"), "https://h/p?keys=1");
        assert_eq!(canonical_url("https://h/p"), "https://h/p");
    }

    #[test]
    fn diff_marks_lines() {
        let d = diff(br#"{"a":1,"b":2}"#, br#"{"a":1,"b":3}"#);
        assert!(d.contains("-   \"b\": 2"), "{d}");
        assert!(d.contains("+   \"b\": 3"), "{d}");
        assert!(d.contains("    \"a\": 1,"), "{d}");
    }
}
