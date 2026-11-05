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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_model::*;

struct FakeLlm {
    name: String,
    backend: Option<Backend>,
}

impl Llm for FakeLlm {
    fn name(&self) -> &str {
        &self.name
    }
    fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> futures::stream::BoxStream<'static, Result<LlmResponse, ModelError>> {
        Box::pin(futures::stream::empty())
    }
    fn google_variant(&self) -> Option<Backend> {
        self.backend
    }
}

fn fake(name: &str, backend: Option<Backend>) -> FakeLlm {
    FakeLlm {
        name: name.into(),
        backend,
    }
}

#[test]
fn test_is_gemini25_or_lower() {
    let cases = [
        ("gemini-1.5-pro", true),
        ("gemini-2.0-flash", true),
        ("gemini-2.5-flash-lite", true),
        ("gemini-2.0-flash-exp", true),
        ("gemini-1.0-pro", true),
        ("projects/p/locations/l/models/gemini-2.0-flash", true),
        ("models/gemini-1.5-pro", true),
        ("not-a-gemini-model", false),
        ("gemini-2", true),
        ("gemini-3.0", false),
        ("gemini-3-pro", false),
    ];
    for (model, want) in cases {
        assert_eq!(
            is_gemini25_or_lower(model),
            want,
            "is_gemini25_or_lower({model:?})"
        );
    }
}

#[test]
fn test_is_gemini1() {
    let cases = [
        ("gemini-1", true),
        ("gemini-1.5", true),
        ("gemini-1.5-flash", true),
        ("gemini-1.5-pro", true),
        ("gemini-10", false),
        ("gemini-10-flash", false),
        ("gemini-2.0-flash", false),
        ("gemini-2.5-flash-lite", false),
        ("gemini-2.0-flash-exp", false),
        ("gemini-1.0-pro", true),
        ("projects/p/locations/l/models/gemini-2.0-flash", false),
        ("projects/p/locations/l/models/gemini-1.0-flash", true),
        ("projects/p/locations/l/models/gemini-10-flash", false),
        ("models/gemini-1.5-pro", true),
        ("not-a-gemini-model", false),
        ("gemini-2", false),
        ("gemini-3.0", false),
        ("gemini-3-pro", false),
    ];
    for (model, want) in cases {
        assert_eq!(is_gemini1_model(model), want, "is_gemini1_model({model:?})");
    }
}

#[test]
fn test_is_gemini_model() {
    let cases = [
        ("gemini-1.5-pro", true),
        ("models/gemini-2.0-flash", true),
        ("claud-3.5-sonnet", false),
    ];
    for (model, want) in cases {
        assert_eq!(is_gemini_model(model), want, "is_gemini_model({model:?})");
    }
}

#[test]
fn test_needs_output_schema_processor() {
    let cases = [
        (
            "Gemini2.0_Vertex",
            "gemini-2.0-flash",
            Some(Backend::VertexAi),
            false,
        ),
        (
            "Gemini2.0_GeminiAPI",
            "gemini-2.0-flash",
            Some(Backend::GeminiApi),
            true,
        ),
        (
            "NonGemini_Vertex",
            "not-a-gemini",
            Some(Backend::VertexAi),
            false,
        ),
        // Gemini API never supports output schema + tools natively, so the
        // processor is required regardless of version.
        (
            "Gemini3.0_GeminiAPI",
            "gemini-3.0",
            Some(Backend::GeminiApi),
            true,
        ),
        (
            "Gemini3.0_Vertex",
            "gemini-3.0",
            Some(Backend::VertexAi),
            false,
        ),
        // Non-Vertex backends (incl. unspecified) need the processor too.
        ("CustomGemini2", "gemini-2.0-hack", None, true),
        ("CustomGemini3", "gemini-3.0-hack", None, true),
        // Gemini 1.x on Vertex predates native combo support, so it needs the processor.
        (
            "Gemini1.5_Vertex",
            "gemini-1.5-pro",
            Some(Backend::VertexAi),
            true,
        ),
    ];
    for (name, model, variant, want) in cases {
        let llm = fake(model, variant);
        assert_eq!(
            needs_output_schema_processor(Some(&llm)),
            want,
            "{name}: {model:?}"
        );
    }
    assert!(!needs_output_schema_processor(None));
}

#[test]
fn test_can_use_output_schema_with_tools() {
    let cases = [
        (
            "Gemini2.0_Vertex",
            "gemini-2.0-flash",
            Some(Backend::VertexAi),
            true,
        ),
        (
            "Gemini3.0_Vertex",
            "gemini-3.0",
            Some(Backend::VertexAi),
            true,
        ),
        (
            "Gemini1.5_Vertex",
            "gemini-1.5-pro",
            Some(Backend::VertexAi),
            false,
        ),
        (
            "Gemini3.0_GeminiAPI",
            "gemini-3.0",
            Some(Backend::GeminiApi),
            false,
        ),
        (
            "Gemini2.0_GeminiAPI",
            "gemini-2.0-flash",
            Some(Backend::GeminiApi),
            false,
        ),
        (
            "NonGemini_Vertex",
            "not-a-gemini",
            Some(Backend::VertexAi),
            false,
        ),
    ];
    for (name, model, variant, want) in cases {
        let llm = fake(model, variant);
        assert_eq!(
            can_use_output_schema_with_tools(Some(&llm)),
            want,
            "{name}: {model:?}"
        );
    }
    assert!(!can_use_output_schema_with_tools(None));
}

#[test]
fn variant_helpers_read_google_variant() {
    let api = fake("gemini-2.0-flash", Some(Backend::GeminiApi));
    let vertex = fake("gemini-2.0-flash", Some(Backend::VertexAi));
    let none = fake("gemini-2.0-flash", None);
    assert!(is_gemini_api_variant(&api));
    assert!(!is_gemini_api_variant(&vertex));
    assert!(!is_gemini_api_variant(&none));
    assert_eq!(google_variant(&vertex), Some(Backend::VertexAi));
    assert_eq!(google_variant(&none), None);
    assert!(is_gemini2_or_above("models/gemini-2.0-flash"));
    assert!(!is_gemini2_or_above("gemini-1.5-pro"));
    assert!(!is_gemini2_or_above("not-gemini"));
}
