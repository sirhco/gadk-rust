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

//! Identifier checks done before any request is sent.

/// A project id: `[A-Za-z0-9.:_-]+` (domain-scoped projects such as `example.com:proj` allowed).
pub(crate) fn valid_project(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '_' | '-'))
}

/// A dataset id: `[A-Za-z0-9_]+`.
pub(crate) fn valid_dataset(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A table id: `[A-Za-z0-9_-]+` (partition decorators such as `t$1` are rejected).
pub(crate) fn valid_table(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn projects() {
        assert!(valid_project("sirhco-apps"));
        assert!(valid_project("example.com:proj_1"));
        assert!(!valid_project(""));
        assert!(!valid_project("a/b"));
        assert!(!valid_project("a b"));
    }

    #[test]
    fn datasets() {
        assert!(valid_dataset("gadk_examples"));
        assert!(!valid_dataset("a.b"));
        assert!(!valid_dataset("a-b"));
        assert!(!valid_dataset(""));
    }

    #[test]
    fn tables() {
        assert!(valid_table("orders"));
        assert!(valid_table("order-items_2"));
        assert!(!valid_table("t$1"));
        assert!(!valid_table("a.b"));
        assert!(!valid_table(""));
    }
}
