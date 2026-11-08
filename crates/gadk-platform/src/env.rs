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

//! Environment variable access, injectable for tests (port of genai's `envVarProvider`).
//!
//! Moved here from gadk-gemini so every model backend shares it; gadk-gemini re-exports it
//! under the old paths (`gadk_gemini::{EnvProvider, MapEnv, ProcessEnv}`).

use std::collections::HashMap;

/// Source of environment variables used by model backends' configuration resolution.
pub trait EnvProvider: Send + Sync {
    /// Returns the value of `key`, or `None` if it is not set. A variable set to the empty
    /// string returns `Some("")`, matching genai's `os.LookupEnv` presence semantics.
    fn var(&self, key: &str) -> Option<String>;
}

/// Reads the process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvProvider for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// A fixed map of variables, for tests.
#[derive(Debug, Clone, Default)]
pub struct MapEnv(pub HashMap<String, String>);

impl EnvProvider for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_env_reports_presence_including_empty_values() {
        let env = MapEnv(HashMap::from([
            ("SET".to_string(), "v".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]));
        assert_eq!(env.var("SET").as_deref(), Some("v"));
        assert_eq!(env.var("EMPTY").as_deref(), Some(""));
        assert_eq!(env.var("UNSET"), None);
    }

    #[test]
    fn process_env_misses_an_unset_variable() {
        assert_eq!(
            ProcessEnv.var("GADK_PLATFORM_ENV_TEST_SURELY_UNSET_7F3A"),
            None
        );
    }
}
