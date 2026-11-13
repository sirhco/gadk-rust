// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Errors: a port of yaml.v3 v3.0.1 `yaml.go`'s `TypeError`, `fail`/`failf` and `handleErr`.
//!
//! Go reports failures by panicking with a `yamlError` that `handleErr` turns into the returned
//! `error`; the port returns [`Error`] values instead. `Display` is exactly Go's `err.Error()`.

use std::fmt;

/// Every error the crate returns. `Display` is the exact text yaml.v3 returns.
#[derive(Debug)]
pub enum Error {
    /// A `failf` error: the complete text including Go's `yaml: ` prefix
    /// (`yaml: line 1: did not find expected key`, `yaml: unknown anchor 'a' referenced`).
    Syntax(String),
    /// Go's `*yaml.TypeError`: the collected `line N: cannot unmarshal …` messages.
    Type(TypeError),
    /// An I/O error surfaced as-is (Go turns reader and writer errors into `Syntax` texts:
    /// `yaml: input error: …`, `yaml: write error: …`).
    Io(std::io::Error),
    /// A serde shape yaml.v3 has no form for (`#[serde(flatten)]`, maps of unknown length, an
    /// `INLINE_FIELD` that is not an `InlineMap`), or a Go panic text the port returns instead
    /// of panicking. Displayed verbatim.
    Unsupported(String),
    /// A message raised through `serde::ser::Error::custom` / `serde::de::Error::custom`
    /// (for example serde's `missing field `x``). Displayed verbatim.
    Custom(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(msg) | Error::Unsupported(msg) | Error::Custom(msg) => f.write_str(msg),
            Error::Type(e) => fmt::Display::fmt(e, f),
            Error::Io(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Type(e) => Some(e),
            Error::Syntax(_) | Error::Unsupported(_) | Error::Custom(_) => None,
        }
    }
}

impl From<TypeError> for Error {
    fn from(e: TypeError) -> Self {
        Error::Type(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::Custom(msg.to_string())
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::Custom(msg.to_string())
    }
}

/// Go's `yaml.TypeError`: returned when one or more fields in the YAML document cannot be
/// decoded into the requested types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeError {
    /// The individual messages, in document order (`line 3: cannot unmarshal !!str `x` into int`).
    pub errors: Vec<String>,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Go: yaml.go (*TypeError).Error
        write!(f, "yaml: unmarshal errors:\n  {}", self.errors.join("\n  "))
    }
}

impl std::error::Error for TypeError {}

/// Go's `failf(format, args...)`: an [`Error::Syntax`] with the `yaml: ` prefix.
#[allow(dead_code)] // Until Tasks 5–9 call it; Task 12 removes this line.
pub(crate) fn failf(msg: impl fmt::Display) -> Error {
    // Go: yaml.go failf
    Error::Syntax(format!("yaml: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_error_text_matches_go() {
        let e = TypeError {
            errors: vec![
                "line 1: cannot unmarshal !!str `a` into int".to_string(),
                "line 2: cannot unmarshal !!seq into string".to_string(),
            ],
        };
        assert_eq!(
            e.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `a` into int\n  line 2: cannot unmarshal !!seq into string"
        );
        assert_eq!(Error::from(e.clone()).to_string(), e.to_string());
    }

    #[test]
    fn failf_prefixes_yaml() {
        let e = failf(format_args!("unknown anchor '{}' referenced", "a"));
        assert!(matches!(&e, Error::Syntax(m) if m == "yaml: unknown anchor 'a' referenced"));
        assert_eq!(e.to_string(), "yaml: unknown anchor 'a' referenced");
    }

    #[test]
    fn verbatim_variants_and_serde_custom() {
        assert_eq!(Error::Unsupported("x y".into()).to_string(), "x y");
        let e = <Error as serde::de::Error>::missing_field("name");
        assert!(matches!(&e, Error::Custom(m) if m == "missing field `name`"));
        let e = <Error as serde::ser::Error>::custom("boom");
        assert_eq!(e.to_string(), "boom");
        let io = Error::from(std::io::Error::other("disk"));
        assert_eq!(io.to_string(), "disk");
        assert!(std::error::Error::source(&io).is_some());
    }
}
