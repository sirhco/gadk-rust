// Copyright 2026 Chris Olson
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

//! The type-path fixup table and its application: each entry names a Go type path from a
//! recording root (e.g. `LLMRequest.Contents[].Parts[].InlineData.Data`, never a user map such
//! as `args` or `response`) and its class: Go `[]byte` (an `!!int` sequence), a non-pointer
//! `TextMarshaler` (`civil.Date`), `time.Duration` / `time.Time`, `genai.Schema` titles, and
//! `*jsonschema.Schema` values. The table equals the probe's reflection-generated fixture
//! (`tests/recordings_differential.rs`). Task 5 owns this file; nothing outside `gonode` uses
//! its items directly.
