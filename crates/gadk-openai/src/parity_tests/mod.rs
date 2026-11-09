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

//! Ports of ADK-Go's `model/openaimodel/internal/*` tests (package-internal, so they live in
//! the crate), one module per Go file (`parity-check` targets
//! `src/parity_tests/<target>.rs`), except `responses/request_test.go` and
//! `responses/model_test.go`, which are directory targets
//! (`src/parity_tests/<target>/mod.rs` plus one module per group of tests). `support` is the
//! shared harness (D6).

mod support;

mod model_openaimodel_internal_completions_media;
mod model_openaimodel_internal_completions_model;
mod model_openaimodel_internal_completions_request;
mod model_openaimodel_internal_completions_response;
mod model_openaimodel_internal_completions_tools;
mod model_openaimodel_internal_responses_model;
mod model_openaimodel_internal_responses_request;
mod model_openaimodel_internal_responses_response;
mod model_openaimodel_internal_responses_servertext;
mod model_openaimodel_internal_responses_stream;
mod model_openaimodel_internal_responses_tools;
mod model_openaimodel_internal_shared_config;
mod model_openaimodel_internal_shared_contents;
mod model_openaimodel_internal_shared_media;
mod model_openaimodel_internal_shared_response;
mod model_openaimodel_internal_shared_schema;
mod model_openaimodel_internal_shared_servertext;
mod model_openaimodel_internal_shared_tools;
