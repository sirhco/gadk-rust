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

//! The error sentinels both APIs return. The sentinels are [`ErrorKind`]s of the crate's
//! [`OpenAiError`] (`crate::error`); this module re-exports them so the API modules can name
//! them as `shared::errors::ErrorKind::NoContents`.

// The API modules import the sentinels from `crate::error` directly; this is the shared path.
#[allow(unused_imports)]
pub(crate) use crate::error::{ErrorKind, OpenAiError};
