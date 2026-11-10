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

//! Port of Go `internal/telemetry/inference.go`: the `gen_ai.provider.name` of a model call.

use std::sync::LazyLock;

use gadk_model::Backend;
use gadk_platform::go_to_lower;
use regex::Regex;

use super::gen_ai_system;

/// Go `genAIProviderName`.
pub(crate) const GEN_AI_PROVIDER_NAME: &str = "gen_ai.provider.name";

/// Go `anthropicModel`.
static ANTHROPIC_MODEL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^claude[-.]").ok());

/// Go `modelPaths`: Vertex AI and Apigee model paths.
static MODEL_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^projects/[^/]+/locations/[^/]+/publishers/[^/]+/models/(.+)$",
        r"^apigee/(?:[^/]+/)?(?:[^/]+/)?(.+)$",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Go `resourceCollections`: leading path segments that are not providers.
const RESOURCE_COLLECTIONS: &[&str] = &[
    "endpoints",
    "locations",
    "models",
    "projects",
    "publishers",
    "tunedmodels",
];

/// Go `providerName`: the provider a model name or path names, else the Google backend's
/// `gen_ai.system` value; `None` when neither can be named.
pub(crate) fn provider_name(model_name: &str, backend: Option<Backend>) -> Option<String> {
    let name = bare_model_name(model_name);
    if !name.starts_with("gemini-") {
        if ANTHROPIC_MODEL.as_ref().is_some_and(|re| re.is_match(name)) {
            return Some("anthropic".to_owned());
        }
        if let Some((provider, _)) = name.split_once('/') {
            let lower = go_to_lower(provider);
            if !provider.is_empty() && !RESOURCE_COLLECTIONS.contains(&lower.as_str()) {
                return Some(lower);
            }
        }
    }
    gen_ai_system(backend).map(str::to_owned)
}

/// Go `bareModelName`: the model ID inside a resource path.
pub(crate) fn bare_model_name(model: &str) -> &str {
    for p in MODEL_PATHS.iter() {
        if let Some(m) = p.captures(model).and_then(|c| c.get(1)) {
            return m.as_str();
        }
    }
    if let Some(rest) = model.strip_prefix("models/") {
        return rest;
    }
    if let Some(i) = model.rfind('/')
        && model[i + 1..].starts_with("gemini-")
    {
        return &model[i + 1..];
    }
    model
}
