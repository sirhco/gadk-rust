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

//! The client's error type.

/// The most bytes of a non-JSON error body that [`Error::Http`] keeps.
pub(crate) const HTTP_BODY_LIMIT: usize = 1024;

/// A failed console API call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A non-2xx reply with the console's JSON error body.
    #[error("api {status} {kind}: {message}")]
    Api {
        /// The HTTP status.
        status: u16,
        /// The error kind: `bad_request`, `needs_bigquery`, `not_found`, `method_not_allowed`,
        /// or for a 502 the source's kind (`bigquery`, `transport`, …).
        kind: String,
        /// What went wrong.
        message: String,
        /// How to fix it, when the console knows.
        hint: Option<String>,
    },
    /// A non-2xx reply whose body is not the console's JSON error, such as a proxy's page.
    #[error("http {status}: {body}")]
    Http {
        /// The HTTP status.
        status: u16,
        /// The body as lossy UTF-8, cut to its first 1024 bytes (at a char boundary).
        body: String,
    },
    /// The request could not be sent or the reply could not be read.
    #[error("transport: {0}")]
    Transport(#[source] reqwest::Error),
    /// A 2xx reply whose body does not decode into the operation's type.
    #[error("decode {status}: {message}")]
    Decode {
        /// The HTTP status.
        status: u16,
        /// The serde error.
        message: String,
    },
    /// A base URL that is not `http` or `https` with an empty path, or a paging link that
    /// leaves the console.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

impl Error {
    /// Whether this is the console's `needs_bigquery` error: the console is not using the
    /// BigQuery trace source.
    pub fn is_needs_bigquery(&self) -> bool {
        matches!(self, Error::Api { kind, .. } if kind == "needs_bigquery")
    }

    /// Whether this is the console's `not_found` error: an unknown trace, a session without
    /// activity in the range, or an unknown API path.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Api { kind, .. } if kind == "not_found")
    }
}

/// `bytes` as lossy UTF-8, cut to its first [`HTTP_BODY_LIMIT`] bytes and then back to a char
/// boundary.
pub(crate) fn http_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut end = text.len().min(HTTP_BODY_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_body_is_cut_at_a_char_boundary() {
        assert_eq!(http_body(b"bad gateway"), "bad gateway");
        let long = "é".repeat(600); // 1200 bytes
        let cut = http_body(long.as_bytes());
        assert_eq!(cut.len(), 1024);
        assert!(cut.chars().all(|c| c == 'é'));
        let odd = format!("x{long}"); // the 1024th byte is inside a char
        assert_eq!(http_body(odd.as_bytes()).len(), 1023);
        assert_eq!(http_body(&[0xff, b'a']), "\u{fffd}a");
    }

    #[test]
    fn the_predicates_match_only_their_api_kind() {
        let api = |kind: &str| Error::Api {
            status: 404,
            kind: kind.to_owned(),
            message: "m".to_owned(),
            hint: None,
        };
        assert!(api("needs_bigquery").is_needs_bigquery());
        assert!(!api("needs_bigquery").is_not_found());
        assert!(api("not_found").is_not_found());
        assert!(!api("not_found").is_needs_bigquery());
        let http = Error::Http {
            status: 404,
            body: "needs_bigquery".to_owned(),
        };
        assert!(!http.is_needs_bigquery() && !http.is_not_found());
        assert_eq!(api("not_found").to_string(), "api 404 not_found: m");
    }
}
