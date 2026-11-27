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

//! gadk-openapi: an OpenAPI 3.0 and 3.1 toolset for gadk agents.
//!
//! Load a document with [`OpenApiSpec`] (from a string with [`str::parse`], a file with
//! [`OpenApiSpec::from_file`], or a URL with [`OpenApiSpec::fetch`]), then build an
//! [`OpenApiToolset`] with an [`OpenApiConfig`]. Each operation becomes one tool, named after
//! its `operationId` in snake_case. A tool's arguments are the operation's path, query and
//! header parameters plus `body` for an `application/json` request body; a call returns
//! `{"status": <u16>, "body": <body>}` for any HTTP status.
//!
//! Calls of `post`, `put`, `patch` and `delete` operations need the user's confirmation by
//! default ([`RequireConfirmation::Mutating`]). Credentials come from a `gadk_auth` credential
//! provider ([`OpenApiConfig::auth`]) or an API key in the query string
//! ([`OpenApiConfig::query_api_key`]).
//!
//! ```no_run
//! # async fn example() -> Result<(), gadk_openapi::Error> {
//! use gadk_openapi::{OpenApiConfig, OpenApiSpec, OpenApiToolset};
//!
//! let spec = OpenApiSpec::fetch("https://petstore3.swagger.io/api/v3/openapi.json").await?;
//! let toolset = OpenApiToolset::new(&spec, OpenApiConfig::new().prefix("petstore"))?;
//! for skipped in toolset.skipped_operations() {
//!     println!("skipped {} {}: {}", skipped.method, skipped.path, skipped.reason);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Out of scope: Swagger 2.0, remote `$ref`s, cookie parameters, form and multipart bodies,
//! choosing credentials from `securitySchemes`, response schemas and streaming responses.

mod config;
mod error;
mod operation;
mod request;
mod resolve;
mod spec;
mod tool;
mod toolset;

pub use config::{DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, OpenApiConfig, RequireConfirmation};
pub use error::Error;
pub use operation::SkippedOperation;
pub use spec::OpenApiSpec;
pub use toolset::OpenApiToolset;
