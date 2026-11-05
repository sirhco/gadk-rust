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

//! Constructors (port of genai `NewPartFrom*`, `NewContentFrom*` and `Text`).

use crate::content::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    Language, Outcome, Part, ROLE_USER,
};
use bytes::Bytes;
use serde_json::{Map, Value};

/// genai `roleString`: the empty role defaults to [`ROLE_USER`].
fn role_string(role: &str) -> String {
    if role.is_empty() {
        ROLE_USER.to_string()
    } else {
        role.to_string()
    }
}

impl Part {
    /// genai `NewPartFromText`.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    /// genai `NewPartFromBytes`.
    pub fn from_bytes(data: Bytes, mime_type: &str) -> Self {
        Self {
            inline_data: Some(Blob {
                data,
                mime_type: mime_type.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// genai `NewPartFromURI`.
    pub fn from_uri(file_uri: &str, mime_type: &str) -> Self {
        Self {
            file_data: Some(FileData {
                file_uri: file_uri.to_string(),
                mime_type: mime_type.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// genai `NewPartFromFunctionCall`: sets only `name` and `args`.
    pub fn from_function_call(name: &str, args: Map<String, Value>) -> Self {
        Self {
            function_call: Some(FunctionCall {
                name: name.to_string(),
                args,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// genai `NewPartFromFunctionResponse`: sets only `name` and `response`.
    pub fn from_function_response(name: &str, response: Map<String, Value>) -> Self {
        Self {
            function_response: Some(FunctionResponse {
                name: name.to_string(),
                response,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// genai `NewPartFromExecutableCode`.
    pub fn from_executable_code(code: &str, language: Language) -> Self {
        Self {
            executable_code: Some(ExecutableCode {
                code: code.to_string(),
                language: Some(language),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// genai `NewPartFromCodeExecutionResult`.
    pub fn from_code_execution_result(outcome: Outcome, output: &str) -> Self {
        Self {
            code_execution_result: Some(CodeExecutionResult {
                outcome: Some(outcome),
                output: output.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

impl Content {
    /// genai `NewContentFromParts`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_parts(parts: Vec<Part>, role: &str) -> Self {
        Self {
            parts,
            role: role_string(role),
        }
    }

    /// genai `NewContentFromText`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_text(text: impl Into<String>, role: &str) -> Self {
        Self::from_parts(vec![Part::from_text(text)], role)
    }

    /// genai `NewContentFromFunctionCall`, taking a built [`FunctionCall`].
    /// An empty `role` defaults to [`ROLE_USER`].
    pub fn from_function_call(fc: FunctionCall, role: &str) -> Self {
        let part = Part {
            function_call: Some(fc),
            ..Default::default()
        };
        Self::from_parts(vec![part], role)
    }

    /// genai `NewContentFromFunctionResponse`, taking a built [`FunctionResponse`].
    /// An empty `role` defaults to [`ROLE_USER`].
    pub fn from_function_response(fr: FunctionResponse, role: &str) -> Self {
        let part = Part {
            function_response: Some(fr),
            ..Default::default()
        };
        Self::from_parts(vec![part], role)
    }

    /// genai `NewContentFromBytes`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_bytes(data: Bytes, mime_type: &str, role: &str) -> Self {
        Self::from_parts(vec![Part::from_bytes(data, mime_type)], role)
    }

    /// genai `NewContentFromURI`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_uri(file_uri: &str, mime_type: &str, role: &str) -> Self {
        Self::from_parts(vec![Part::from_uri(file_uri, mime_type)], role)
    }

    /// genai `NewContentFromExecutableCode`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_executable_code(code: &str, language: Language, role: &str) -> Self {
        Self::from_parts(vec![Part::from_executable_code(code, language)], role)
    }

    /// genai `NewContentFromCodeExecutionResult`. An empty `role` defaults to [`ROLE_USER`].
    pub fn from_code_execution_result(outcome: Outcome, output: &str, role: &str) -> Self {
        Self::from_parts(
            vec![Part::from_code_execution_result(outcome, output)],
            role,
        )
    }
}

/// genai `Text`: a single user [`Content`] holding one text [`Part`].
pub fn text(s: impl Into<String>) -> Vec<Content> {
    vec![Content::from_text(s, ROLE_USER)]
}
