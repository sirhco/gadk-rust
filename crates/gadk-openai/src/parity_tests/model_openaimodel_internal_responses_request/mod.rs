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

//! Port of ADK-Go `model/openaimodel/internal/responses/request_test.go`, a directory target:
//! - `build_params`: `TestBuildParams_*`, `TestReplayedReasoning*`, `TestUnsupportedPayload_*`,
//!   `TestCallTrackerNewFunctionResponse_UnknownCallID`, `TestNormalizeRole` (Task 5);
//! - `generation_config`: `TestApplyGenerationConfig*`, `TestReasoningEffortsCoverEveryThinkingLevel`,
//!   `TestServiceTiersCoverEveryGenaiTier`, `TestRequestTimeoutHonorsAPositiveTimeout`,
//!   `TestHeadersAffectNeitherValidationNorTimeout`, `TestTranslatedFieldsAreNotRejected` (Task 5);
//! - `schema`: `TestNewJSONSchemaFormat*`, `TestBuildParamsPreservesLargeJSONSchemaIntegers` (Task 5);
//! - `model_backed`: `TestCallerAuthorizationHeaderDisplacesTheAPIKey`,
//!   `TestHTTPOptionsTimeoutReachesTheRequest`, `TestGenerateContentIsReRangeableWithATimeout`
//!   (Task 9, D8).

mod build_params;
mod generation_config;
mod model_backed;
mod schema;
