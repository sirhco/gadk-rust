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

//! Port of ADK-Go `internal/llminternal/converters/converters.go`.

use gadk_genai::{Content, FinishReason, GenerateContentResponse, ROLE_MODEL};

use crate::llm::LlmResponse;

/// Converts a genai response into an [`LlmResponse`] built from its first candidate
/// (port of ADK-Go `converters.Genai2LLMResponse`).
pub fn llm_response_from_genai(res: &GenerateContentResponse) -> LlmResponse {
    let usage_metadata = res.usage_metadata.clone();
    if let Some(candidate) = res.candidates.first() {
        let has_parts = candidate
            .content
            .as_ref()
            .is_some_and(|c| !c.parts.is_empty());
        if has_parts || candidate.finish_reason == Some(FinishReason::Stop) {
            return LlmResponse {
                content: candidate.content.clone(),
                grounding_metadata: candidate.grounding_metadata.clone(),
                finish_reason: candidate.finish_reason.clone(),
                citation_metadata: candidate.citation_metadata.clone(),
                avg_logprobs: candidate.avg_logprobs,
                logprobs_result: candidate.logprobs_result.clone(),
                usage_metadata,
                model_version: res.model_version.clone(),
                ..Default::default()
            };
        }
        return LlmResponse {
            error_code: candidate
                .finish_reason
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_default(),
            error_message: candidate.finish_message.clone(),
            grounding_metadata: candidate.grounding_metadata.clone(),
            finish_reason: candidate.finish_reason.clone(),
            citation_metadata: candidate.citation_metadata.clone(),
            avg_logprobs: candidate.avg_logprobs,
            logprobs_result: candidate.logprobs_result.clone(),
            usage_metadata,
            model_version: res.model_version.clone(),
            ..Default::default()
        };
    }
    if let Some(feedback) = &res.prompt_feedback {
        return LlmResponse {
            error_code: feedback
                .block_reason
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_default(),
            error_message: feedback.block_reason_message.clone(),
            usage_metadata,
            model_version: res.model_version.clone(),
            ..Default::default()
        };
    }
    // No candidates and no prompt feedback: some backends (e.g. gemini-3* via Vertex AI)
    // send metadata-only chunks. Treat them as valid, empty model responses so downstream
    // can still process usage metadata.
    LlmResponse {
        content: Some(Content {
            parts: Vec::new(),
            role: ROLE_MODEL.to_owned(),
        }),
        usage_metadata,
        model_version: res.model_version.clone(),
        ..Default::default()
    }
}
