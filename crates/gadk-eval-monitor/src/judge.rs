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

//! The judge model: Gemini on Vertex AI, with Application Default Credentials.

use std::sync::Arc;

use gadk_gemini::{ClientConfig, GeminiModel};
use gadk_model::{Backend, HttpClient, Llm};

use crate::error::MonitorError;
use crate::spec::JudgeSpec;

/// The Gemini model `spec.model` on Vertex AI in `spec.project` / `spec.location`.
///
/// # Errors
///
/// [`MonitorError::Judge`] when the model cannot be built (for example, no Application Default
/// Credentials).
pub async fn vertex_judge(spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
    build(spec, None).await
}

/// [`vertex_judge`] with an explicit HTTP client (which also skips the ADC lookup).
async fn build(
    spec: &JudgeSpec,
    http: Option<Arc<dyn HttpClient>>,
) -> Result<Arc<dyn Llm>, MonitorError> {
    GeminiModel::new(
        &spec.model,
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(spec.project.clone()),
            location: Some(spec.location.clone()),
            http_client: http,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| MonitorError::Judge(format!("building {} on Vertex AI: {e}", spec.model)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gadk_model::rest::test_util::ScriptedClient;

    use super::*;

    #[tokio::test]
    async fn builds_a_vertex_gemini_model() {
        let spec = JudgeSpec {
            model: "gemini-2.5-flash".into(),
            project: "my-proj".into(),
            location: "us-central1".into(),
            timeout: Duration::from_secs(120),
        };
        let judge = build(&spec, Some(ScriptedClient::json("{}")))
            .await
            .unwrap();
        assert_eq!(judge.name(), "gemini-2.5-flash");
        assert_eq!(judge.google_variant(), Some(Backend::VertexAi));
    }
}
