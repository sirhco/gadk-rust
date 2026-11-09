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

//! The Responses API stream event types ADK-Go's translator and stream loop switch on (port
//! of ADK-Go `internal/responses/consts.go`).

/// `response.output_text.delta`.
pub(crate) const RESPONSE_OUTPUT_TEXT_DELTA: &str = "response.output_text.delta";
/// `response.refusal.delta`.
pub(crate) const RESPONSE_REFUSAL_DELTA: &str = "response.refusal.delta";
/// `response.reasoning_text.delta`.
pub(crate) const RESPONSE_REASONING_TEXT_DELTA: &str = "response.reasoning_text.delta";
/// `response.reasoning_summary_text.delta`.
pub(crate) const RESPONSE_REASONING_SUMMARY_TEXT_DELTA: &str =
    "response.reasoning_summary_text.delta";
/// `response.function_call_arguments.delta`.
pub(crate) const RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA: &str =
    "response.function_call_arguments.delta";
/// `response.function_call_arguments.done`.
pub(crate) const RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE: &str =
    "response.function_call_arguments.done";
/// `response.output_text.done`.
pub(crate) const RESPONSE_OUTPUT_TEXT_DONE: &str = "response.output_text.done";
/// `response.refusal.done`.
pub(crate) const RESPONSE_REFUSAL_DONE: &str = "response.refusal.done";
/// `response.reasoning_text.done`.
pub(crate) const RESPONSE_REASONING_TEXT_DONE: &str = "response.reasoning_text.done";
/// `response.reasoning_summary_text.done`.
pub(crate) const RESPONSE_REASONING_SUMMARY_TEXT_DONE: &str =
    "response.reasoning_summary_text.done";
/// `response.created`.
pub(crate) const RESPONSE_CREATED: &str = "response.created";
/// `response.completed`.
pub(crate) const RESPONSE_COMPLETED: &str = "response.completed";
/// `response.incomplete`.
pub(crate) const RESPONSE_INCOMPLETE: &str = "response.incomplete";
/// `response.in_progress`.
pub(crate) const RESPONSE_IN_PROGRESS: &str = "response.in_progress";
/// `response.output_item.added`.
pub(crate) const RESPONSE_OUTPUT_ITEM_ADDED: &str = "response.output_item.added";
/// `response.output_item.done`.
pub(crate) const RESPONSE_OUTPUT_ITEM_DONE: &str = "response.output_item.done";
/// `response.failed`.
pub(crate) const RESPONSE_FAILED: &str = "response.failed";
/// `error`.
pub(crate) const ERROR_EVENT: &str = "error";
