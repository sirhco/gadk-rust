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

//! Ports of ADK-Go tests that need crate-internal access, one module per Go test file (plan
//! Appendix A): Go internal-package tests, plus the external-package tests that Appendix A marks
//! `external→internal` (plan decision D2).
//! Go packages: `artifact`.
//! Each module's owning task fills it.

mod artifact_artifact_key;
mod artifact_request_validation;
