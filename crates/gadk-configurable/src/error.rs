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

//! [`ConfigError`]: the errors of the configurable-agent loader (agent, tool, callback,
//! workflow and conformance registration failures), displayed with the exact text ADK's own
//! loader gives the corresponding error chain.
//!
//! Errors are built from a leaf message ([`ConfigError::Msg`]), a context wrapped around a
//! cause, displayed `<context>: <cause>` ([`ConfigError::Wrap`]), a file-system failure
//! ([`ConfigError::Path`], as `open <path>: <errno text>`), a yaml.v3 error
//! ([`ConfigError::Yaml`]) and another crate's error passed through unchanged
//! ([`ConfigError::Other`]). Each variant displays that text, so a whole chain displays as one
//! message.

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// An error of the configurable-agent loader: `Display` is the text of the whole chain.
#[derive(Debug, Clone)]
pub enum ConfigError {
    /// A leaf error: the complete text.
    Msg(String),
    /// A context around a cause: displays `<context>: <source>`.
    Wrap {
        /// The text before the cause.
        context: String,
        /// The wrapped error.
        source: Box<ConfigError>,
    },
    /// A file-system failure (read, stat): displays `<op> <path>: <errno text>`.
    Path {
        /// The failing operation (`open`, `read`, `stat`).
        op: &'static str,
        /// The path as printed.
        path: String,
        /// The `io::ErrorKind`, for not-found checks.
        kind: io::ErrorKind,
        /// The OS error text (`no such file or directory`).
        errno: String,
    },
    /// A yaml.v3 error (a type error or a `yaml: …` failure) with yaml.v3's text.
    Yaml(Arc<gadk_goyaml::Error>),
    /// Another crate's error returned through unchanged (the LLM agent, workflow, Gemini
    /// model, MCP toolset and function tool constructors): its own `Display` is the text.
    Other(Arc<dyn std::error::Error + Send + Sync>),
}

impl ConfigError {
    /// A leaf error with the complete text.
    pub fn msg(text: impl Into<String>) -> ConfigError {
        ConfigError::Msg(text.into())
    }

    /// A context wrapped around a cause (`<context>: <source>`).
    pub fn wrap(context: impl Into<String>, source: impl Into<ConfigError>) -> ConfigError {
        ConfigError::Wrap {
            context: context.into(),
            source: Box::new(source.into()),
        }
    }

    /// A file-system failure on `path` (`<op> <path>: <errno text>`).
    pub fn path(op: &'static str, path: &Path, err: &io::Error) -> ConfigError {
        ConfigError::Path {
            op,
            path: path.to_string_lossy().into_owned(),
            kind: err.kind(),
            errno: go_errno_text(err),
        }
    }

    /// Another crate's error, returned unchanged (its `Display` is the text).
    pub fn other(err: impl std::error::Error + Send + Sync + 'static) -> ConfigError {
        ConfigError::Other(Arc::new(err))
    }

    /// Whether this is a [`ConfigError::Path`] whose kind is `NotFound`.
    pub fn is_not_exist(&self) -> bool {
        matches!(self, ConfigError::Path { kind, .. } if *kind == io::ErrorKind::NotFound)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Msg(text) => f.write_str(text),
            ConfigError::Wrap { context, source } => write!(f, "{context}: {source}"),
            ConfigError::Path {
                op, path, errno, ..
            } => write!(f, "{op} {path}: {errno}"),
            ConfigError::Yaml(e) => fmt::Display::fmt(e, f),
            ConfigError::Other(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Wrap { source, .. } => Some(source.as_ref()),
            ConfigError::Yaml(e) => Some(e.as_ref()),
            ConfigError::Other(e) => Some(e.as_ref()),
            ConfigError::Msg(_) | ConfigError::Path { .. } => None,
        }
    }
}

impl From<gadk_goyaml::Error> for ConfigError {
    fn from(e: gadk_goyaml::Error) -> Self {
        ConfigError::Yaml(Arc::new(e))
    }
}

/// The OS error text (shared with `gadk-a2a`; see [`gadk_platform::go_errno_text`]).
pub use gadk_platform::go_errno_text;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn msg_displays_go_text() {
        assert_eq!(
            ConfigError::msg("'name' is required").to_string(),
            "'name' is required"
        );
    }

    #[test]
    fn wrap_chain_displays_like_go_percent_v() {
        // Go: newLLMAgent → toLLMAgentConfig → resolveSubAgents → ResolveAgentReference.
        let leaf = ConfigError::msg(format!(
            "path traversal detected: config_path {} resolves outside agent directory",
            gadk_platform::go_quote("../x.yaml")
        ));
        let err = ConfigError::wrap(
            "failed to create LLM agent config",
            ConfigError::wrap("failed to resolve agent reference ../x.yaml", leaf),
        );
        assert_eq!(
            err.to_string(),
            "failed to create LLM agent config: failed to resolve agent reference ../x.yaml: \
             path traversal detected: config_path \"../x.yaml\" resolves outside agent directory"
        );
        let inner = err.source().expect("wrap has a source");
        assert_eq!(
            inner.to_string(),
            "failed to resolve agent reference ../x.yaml: path traversal detected: config_path \
             \"../x.yaml\" resolves outside agent directory"
        );
        assert!(inner.source().expect("inner wraps").source().is_none());
    }

    #[test]
    fn path_error_displays_go_path_error() {
        let dir = std::env::temp_dir().join("gadk-configurable-missing-dir");
        let missing = dir.join("nope.yaml");
        let io_err = std::fs::read(&missing).expect_err("missing file");
        let err = ConfigError::path("open", &missing, &io_err);
        assert_eq!(
            err.to_string(),
            format!("open {}: no such file or directory", missing.display())
        );
        assert!(err.is_not_exist());
        assert!(!ConfigError::msg("x").is_not_exist());
    }

    #[test]
    fn yaml_error_keeps_yaml_text() {
        let e = gadk_goyaml::unmarshal_node(b"a: [").expect_err("syntax error");
        let want = e.to_string();
        let err = ConfigError::wrap("invalid YAML content", e);
        assert_eq!(err.to_string(), format!("invalid YAML content: {want}"));
        assert!(want.starts_with("yaml: "));
    }

    #[test]
    fn other_is_display_transparent() {
        let err = ConfigError::other(gadk_platform::GoWrapped::new("outer", "inner"));
        assert_eq!(err.to_string(), "outer: inner");
        assert_eq!(
            ConfigError::wrap("failed to create model", err).to_string(),
            "failed to create model: outer: inner"
        );
    }
}
