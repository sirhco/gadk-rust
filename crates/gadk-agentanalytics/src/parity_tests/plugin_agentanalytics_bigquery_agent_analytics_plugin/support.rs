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

//! The Go test helpers shared by the 7b-3a ports (owner: Task 9): ADK contexts for direct callback
//! tests (Go `mockInvocationContext`), the plugin over the in-process `FakeWriteServer` with its
//! decoded rows (Go `fakeBigQueryWriteServer`), the row-capturing plugins, and scripted models /
//! agents for runner-level tests.
