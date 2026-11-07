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

//! Ports of ADK-Go's internal (`package auth` / `package gcp`) tests, one module per Go file.

#[cfg(feature = "gcp")]
mod auth_gcp_client;
#[cfg(feature = "gcp")]
mod auth_gcp_provider_internal;
#[cfg(feature = "gcp")]
mod auth_gcp_redact_fuzz;
#[cfg(feature = "gcp")]
mod auth_gcp_redact_reference;
#[cfg(feature = "gcp")]
mod auth_gcp_unescape;
mod auth_providers_internal;
mod auth_store_internal;
