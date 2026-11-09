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

//! The error sentinels both APIs return (port of ADK-Go `internal/shared/errors.go`). The
//! sentinels are [`ErrorKind`]s of the crate's [`OpenAiError`] (`crate::error`, D3); this module
//! re-exports them under the Go package's name so the API modules read as Go's do
//! (`shared::errors::ErrorKind::NoContents` ≙ `shared.ErrNoContents`).

#[allow(unused_imports)]
// D1: the API modules import the sentinels from here or from `crate::error`.
pub(crate) use crate::error::{ErrorKind, OpenAiError};
