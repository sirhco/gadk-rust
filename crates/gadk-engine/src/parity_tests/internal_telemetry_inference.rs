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

//! Ports of ADK-Go `internal/telemetry/inference_test.go`. Go's `BackendUnspecified` is `None`.

use gadk_model::Backend;
use pretty_assertions::assert_eq;
use rstest::rstest;

use crate::telemetry::inference::provider_name;

const VERTEX: Option<Backend> = Some(Backend::VertexAi);
const GEMINI: Option<Backend> = Some(Backend::GeminiApi);
const UNSPECIFIED: Option<Backend> = None;

#[rstest]
#[case::claude_sonnet_4_5_vertexai("claude-sonnet-4-5", VERTEX, Some("anthropic"))]
#[case::claude_3_5_haiku_latest_unspecified(
    "claude-3-5-haiku-latest",
    UNSPECIFIED,
    Some("anthropic")
)]
#[case::claude_3_opus_unspecified("Claude-3-Opus", UNSPECIFIED, Some("anthropic"))]
#[case::anthropic_claude_sonnet_4_5_unspecified(
    "anthropic/claude-sonnet-4-5",
    UNSPECIFIED,
    Some("anthropic")
)]
#[case::vertex_publisher_anthropic(
    "projects/p/locations/l/publishers/anthropic/models/claude-sonnet-4-5",
    VERTEX,
    Some("anthropic")
)]
#[case::openai_gpt_4o_unspecified("openai/gpt-4o", UNSPECIFIED, Some("openai"))]
#[case::openai_mixed_case_unspecified("OpenAI/gpt-4o", UNSPECIFIED, Some("openai"))]
#[case::vertex_publisher_meta(
    "projects/p/locations/l/publishers/meta/models/llama-3",
    VERTEX,
    Some("gcp.vertex_ai")
)]
#[case::vertex_endpoint(
    "projects/p/locations/l/endpoints/123456",
    VERTEX,
    Some("gcp.vertex_ai")
)]
#[case::tuned_models_geminiapi("tunedModels/my-tuned-model", GEMINI, Some("gcp.gemini"))]
#[case::gemini_2_0_flash_vertexai("gemini-2.0-flash", VERTEX, Some("gcp.vertex_ai"))]
#[case::gemini_slash_gemini_geminiapi("gemini/gemini-2.0-flash", GEMINI, Some("gcp.gemini"))]
#[case::models_gemini_geminiapi("models/gemini-2.0-flash", GEMINI, Some("gcp.gemini"))]
#[case::gemini_version_suffix_vertexai("gemini-2.0-flash/001", VERTEX, Some("gcp.vertex_ai"))]
#[case::models_claude_geminiapi("models/claude-3-haiku", GEMINI, Some("anthropic"))]
#[case::apigee_openai_unspecified("apigee/openai/gpt-4o", UNSPECIFIED, None)]
#[case::some_model_vertexai("some-model", VERTEX, Some("gcp.vertex_ai"))]
#[case::slash_foo_vertexai("/foo", VERTEX, Some("gcp.vertex_ai"))]
#[case::gemini_2_0_flash_unspecified("gemini-2.0-flash", UNSPECIFIED, None)]
#[case::some_model_unspecified("some-model", UNSPECIFIED, None)]
#[case::empty_unspecified("", UNSPECIFIED, None)]
fn test_provider_name(
    #[case] model: &str,
    #[case] backend: Option<Backend>,
    #[case] want: Option<&str>,
) {
    assert_eq!(
        provider_name(model, backend).as_deref(),
        want,
        "provider_name({model:?}, {backend:?})"
    );
}
