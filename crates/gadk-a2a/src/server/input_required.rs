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

//! Input-required handling (port of ADK-Go `server/adka2a/v2/input_required.go`). Bodies: plan
//! 5b Task 7.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_genai::Content;

use crate::Error;

/// Go `HandleInputRequired`: when the stored task is input-required and `content` does not
/// answer every pending long-running call, the status update that keeps it input-required.
pub async fn handle_input_required(
    ctx: &a2a_server::ExecutorContext,
    content: &Content,
) -> Result<Option<a2a::TaskStatusUpdateEvent>, Error> {
    todo!("plan 5b Task 7")
}
