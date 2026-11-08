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

//! Go `fmt` verb ports used by gadk-agent and gadk-engine: `strconv.Quote` (`%q`), shared
//! from gadk-platform so every crate formats `%q` with one implementation.

pub use gadk_platform::go_quote;

#[cfg(test)]
mod tests {
    /// Call sites that Go formats with `%q` use the port, so a non-printable rune is escaped the
    /// Go way (`­`), not Rust Debug's way (`\u{ad}`).
    #[test]
    fn percent_q_call_sites_use_go_quote() {
        let err = crate::AgentError::Tool {
            tool: "t\u{ad}".into(),
            source: crate::ToolError::ConfirmationRejected,
        };
        assert_eq!(err.to_string(), "error tool \"t\\u00ad\" call is rejected");

        let fc = gadk_genai::FunctionCall {
            id: "c1".into(),
            args: serde_json::json!({"x": 1}).as_object().unwrap().clone(),
            ..Default::default()
        };
        let err = crate::original_call_from(&fc).unwrap_err();
        assert_eq!(
            err.to_string(),
            "required argument \"originalFunctionCall\" is missing from call with ID c1"
        );
    }
}
