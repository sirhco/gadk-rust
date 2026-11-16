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

//! URL helpers shared by the Cloud Trace / Cloud Logging client and the HTML pages.

/// Percent-encodes every byte except the RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`), as uppercase `%XX`.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::percent_encode;

    #[test]
    fn unreserved_bytes_stay_literal() {
        let unreserved = "ABCXYZabcxyz0189-._~";
        assert_eq!(percent_encode(unreserved), unreserved);
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn everything_else_is_uppercase_percent_encoded() {
        assert_eq!(percent_encode("team/greet"), "team%2Fgreet");
        assert_eq!(percent_encode("a b+c"), "a%20b%2Bc");
        assert_eq!(percent_encode(r#""x":\"#), "%22x%22%3A%5C");
        assert_eq!(percent_encode("?&=#%"), "%3F%26%3D%23%25");
        // Multi-byte UTF-8 is encoded byte by byte.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }
}
