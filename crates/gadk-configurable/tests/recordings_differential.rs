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

//! Differential tests against the Go probe's recording corpus (`tests/fixtures/`, written by
//! `scripts/gen-go-configurable`): Rust record bytes equal Go's, Rust replay decoding equals
//! Go's expected values (or Go's decode error), record → replay round trips, and the type-path
//! fixup table equals the probe's reflection-generated table (Task 5).
