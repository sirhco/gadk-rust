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

//! Parity target for Go artifact/clone_test.go.
//!
//! Go's `clonePart` deep-copies a `*genai.Part` field by field so the in-memory service never
//! aliases a caller's pointers, maps or slices. A gadk `Part` is an owned value whose derived
//! `Clone` copies every member, and `InMemoryArtifactService` takes and returns parts by value,
//! so there is no copy routine to test; `artifact_inmemory.rs` checks the service-level
//! behaviour (`test_in_memory_service_does_not_alias_stored_part*`).

// parity: skipped TestClonePartCopiesEveryField: walks genai.Part with Go reflection and compares pointer addresses; a gadk Part is an owned value whose derived Clone shares nothing
// parity: skipped TestClonePartKeepsNilAndEmptyDistinct: Go nil vs empty slices/maps and clonePart(nil); gadk's Option/empty members are plain values that Clone preserves
// parity: skipped TestClonePartStopsAtDepthBound: Go's maxCloneDepth bound on copying nested map[string]any; serde_json values are owned trees that Clone copies whole
// parity: skipped TestClonePartSelfReferencingMetadata: a self-referencing Go map has no counterpart in an owned serde_json value
