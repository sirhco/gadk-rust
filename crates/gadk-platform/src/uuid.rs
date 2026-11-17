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

/// A source of unique identifier strings that tests can replace.
pub trait UuidProvider: Send + Sync {
    /// Returns a new unique identifier string.
    fn new_uuid(&self) -> String;
}

impl<F: Fn() -> String + Send + Sync> UuidProvider for F {
    fn new_uuid(&self) -> String {
        self()
    }
}

/// Default provider: random (v4) UUIDs.
pub(crate) struct RandomUuid;

impl UuidProvider for RandomUuid {
    fn new_uuid(&self) -> String {
        ::uuid::Uuid::new_v4().to_string()
    }
}
