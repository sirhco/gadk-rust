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

//! Go `resource` semantics over [`opentelemetry_sdk::Resource`]: `resource.Merge` with its
//! schema-URL rule, `resource.NewWithAttributes`, and Go's `resource.Default()` read from the
//! injected environment.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_platform::env::EnvProvider;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

/// semconv v1.43.0's schema URL (Go `semconv.SchemaURL`; otel sdk v1.47.0
/// `resource/builtin.go:16`), the schema of Go's default resource and the GCP detector's.
pub(crate) const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.43.0";

/// Go `resource.NewWithAttributes(schemaURL, attrs...)` (last value wins; an empty
/// `schema_url` is schemaless).
pub(crate) fn new_with_attributes(
    schema_url: &str,
    attrs: impl IntoIterator<Item = KeyValue>,
) -> Resource {
    todo!()
}

/// Go `resource.Merge(a, b)`: `b`'s attributes win; the schema URL is the non-empty one, or
/// both when equal. Two different non-empty schema URLs fail with Go's
/// `conflicting Schema URL: <a> and <b>`.
pub(crate) fn go_merge(a: &Resource, b: &Resource) -> Result<Resource, String> {
    todo!()
}

/// Go `resource.Default()`: `service.name` (`unknown_service:<executable>`), then
/// `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_SERVICE_NAME` from `env` (Go `fromEnv`), then the
/// `telemetry.sdk.*` attributes, under [`SCHEMA_URL`].
pub(crate) fn go_default_resource(env: &dyn EnvProvider) -> Resource {
    todo!()
}
