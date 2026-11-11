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

//! Part conversions (port of ADK-Go `server/adka2a/v2/parts.go`). Bodies: plan 5b Task 3.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;

use gadk_genai::{Content, Part};
use serde_json::Value;

use super::A2APartConverter;
use crate::Error;

/// Go `IsPartial`: whether A2A metadata marks the artifact or message as partial.
pub fn is_partial(meta: Option<&HashMap<String, Value>>) -> bool {
    todo!("plan 5b Task 3")
}

/// Go `IsPartialFlagSet`: whether the partial flag key is present in A2A metadata.
pub fn is_partial_flag_set(meta: Option<&HashMap<String, Value>>) -> bool {
    todo!("plan 5b Task 3")
}

/// Go `ToA2APart`.
pub fn to_a2a_part(part: &Part, long_running_tool_ids: &[String]) -> Result<a2a::Part, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToA2AParts`.
pub fn to_a2a_parts(
    parts: &[Part],
    long_running_tool_ids: &[String],
) -> Result<Vec<a2a::Part>, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToGenAIPart`.
pub fn to_genai_part(part: &a2a::Part) -> Result<Part, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToGenAIParts`.
pub fn to_genai_parts(parts: &[a2a::Part]) -> Result<Vec<Part>, Error> {
    todo!("plan 5b Task 3")
}

/// Go `toGenAIContent`: the message's parts as genai content, through `converter` when set.
pub async fn to_genai_content(
    msg: &a2a::Message,
    converter: Option<&A2APartConverter>,
) -> Result<Content, Error> {
    todo!("plan 5b Task 3")
}
