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

//! A name-based model registry (port of ADK-Go `model/registry.go`).
//!
//! An [`Llm`] can be constructed from just a model-name string via
//! [`Registry::new_llm`]. Registration is opt-in: provider crates do not register
//! themselves. A user who wants name-based resolution registers a factory explicitly,
//! using an anchored pattern such as `"^(?i)gemini-.*"`.
//!
//! [`Registry::new_llm`] requires exactly one registered pattern to match a given name:
//! zero matches or two-or-more matches are reported as errors, so resolution never
//! depends on registration order.

use std::sync::{Arc, LazyLock};

use futures::future::BoxFuture;
use parking_lot::RwLock;
use regex::Regex;

use crate::{Llm, ModelError};

/// Constructs an [`Llm`] for the given model name.
///
/// The name is the same string that was matched against the registered pattern, so a
/// single factory can serve a family of models (for example, all `gemini-*` names).
pub type Factory =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<Arc<dyn Llm>, ModelError>> + Send + Sync>;

/// Errors returned by [`Registry`].
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The pattern is not a valid regular expression: `(pattern, reason)`.
    #[error("model: invalid LLM pattern {0:?}: {1}")]
    InvalidPattern(String, String),
    /// The exact pattern string is already registered.
    #[error("model: LLM pattern {0:?} is already registered")]
    DuplicatePattern(String),
    /// No registered pattern matches the name.
    #[error("model: no registered LLM matches {0:?}")]
    NoMatch(String),
    /// More than one registered pattern matches the name.
    #[error(
        "model: {name:?} matches multiple registered LLM patterns {patterns:?}; patterns must be unambiguous"
    )]
    Ambiguous {
        /// The model name that was looked up.
        name: String,
        /// The matching patterns, in registration order.
        patterns: Vec<String>,
    },
    /// The matching factory failed.
    #[error(transparent)]
    Factory(ModelError),
}

struct Registration {
    regex: Regex,
    pattern: String,
    factory: Factory,
}

/// A registry mapping model-name patterns to [`Factory`] functions.
///
/// Safe for concurrent use.
#[derive(Default)]
pub struct Registry {
    entries: RwLock<Vec<Registration>>,
}

static GLOBAL: LazyLock<Registry> = LazyLock::new(Registry::new);

impl Registry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide registry (the equivalent of ADK-Go's package-level registry).
    pub fn global() -> &'static Registry {
        &GLOBAL
    }

    /// Associates a model-name pattern with a [`Factory`].
    ///
    /// `pattern` is a regular expression matched against candidate names with an
    /// unanchored, partial match; anchor it with `^` and `$` to match a name exactly.
    /// Registration order is not significant, so patterns must not overlap for names a
    /// caller intends to resolve.
    ///
    /// Returns [`RegistryError::InvalidPattern`] if `pattern` does not compile and
    /// [`RegistryError::DuplicatePattern`] if the same pattern string is already
    /// registered (ADK-Go panics in both cases).
    pub fn register(&self, pattern: &str, f: Factory) -> Result<(), RegistryError> {
        let regex = Regex::new(pattern)
            .map_err(|e| RegistryError::InvalidPattern(pattern.to_string(), e.to_string()))?;
        let mut entries = self.entries.write();
        if entries.iter().any(|r| r.pattern == pattern) {
            return Err(RegistryError::DuplicatePattern(pattern.to_string()));
        }
        entries.push(Registration {
            regex,
            pattern: pattern.to_string(),
            factory: f,
        });
        Ok(())
    }

    /// Builds an [`Llm`] for `name` using the single registration whose pattern matches.
    ///
    /// Returns [`RegistryError::NoMatch`] if no pattern matches,
    /// [`RegistryError::Ambiguous`] if two or more match, and
    /// [`RegistryError::Factory`] if the matching factory fails. The registry lock is
    /// not held while the factory runs.
    pub async fn new_llm(&self, name: &str) -> Result<Arc<dyn Llm>, RegistryError> {
        let (factory, matched) = {
            let entries = self.entries.read();
            let mut factory = None;
            let mut matched = Vec::new();
            for r in entries.iter().filter(|r| r.regex.is_match(name)) {
                factory = Some(r.factory.clone());
                matched.push(r.pattern.clone());
            }
            (factory, matched)
        };

        match (factory, matched.len()) {
            (Some(f), 1) => f(name.to_string()).await.map_err(RegistryError::Factory),
            (_, 0) | (None, _) => Err(RegistryError::NoMatch(name.to_string())),
            _ => Err(RegistryError::Ambiguous {
                name: name.to_string(),
                patterns: matched,
            }),
        }
    }
}
