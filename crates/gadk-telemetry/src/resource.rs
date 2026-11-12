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

use gadk_platform::env::EnvProvider;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::resource::{ResourceDetector as _, TelemetryResourceDetector};

/// semconv v1.43.0's schema URL (Go `semconv.SchemaURL`; otel sdk v1.47.0
/// `resource/builtin.go:16`), the schema of Go's default resource and the GCP detector's.
pub(crate) const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.43.0";

/// Go `resourceAttrKey`.
const RESOURCE_ATTRIBUTES_ENV: &str = "OTEL_RESOURCE_ATTRIBUTES";

/// Go `svcNameKey`.
const SERVICE_NAME_ENV: &str = "OTEL_SERVICE_NAME";

/// Go `resource.NewWithAttributes(schemaURL, attrs...)` (last value wins; an empty
/// `schema_url` is schemaless).
pub(crate) fn new_with_attributes(
    schema_url: &str,
    attrs: impl IntoIterator<Item = KeyValue>,
) -> Resource {
    // Go drops invalid attributes: an empty key is one.
    let attrs = attrs.into_iter().filter(|kv| !kv.key.as_str().is_empty());
    Resource::builder_empty()
        .with_schema_url(attrs, schema_url.to_owned())
        .build()
}

/// Go `resource.Merge(a, b)`: `b`'s attributes win; the schema URL is the non-empty one, or
/// both when equal. Two different non-empty schema URLs fail with Go's
/// `conflicting Schema URL: <a> and <b>`.
pub(crate) fn go_merge(a: &Resource, b: &Resource) -> Result<Resource, String> {
    let (merged, conflict) = merge(a, b);
    match conflict {
        None => Ok(merged),
        Some(err) => Err(err),
    }
}

/// Go `resource.Merge`, returning the merged resource (schemaless on a conflict) and the
/// conflict error, as Go does.
fn merge(a: &Resource, b: &Resource) -> (Resource, Option<String>) {
    let attrs = a
        .iter()
        .chain(b.iter())
        .map(|(k, v)| KeyValue::new(k.clone(), v.clone()));
    let sa = a.schema_url().unwrap_or_default();
    let sb = b.schema_url().unwrap_or_default();
    let (schema, conflict) = if sa.is_empty() {
        (sb, None)
    } else if sb.is_empty() || sa == sb {
        (sa, None)
    } else {
        ("", Some(format!("conflicting Schema URL: {sa} and {sb}")))
    };
    (new_with_attributes(schema, attrs), conflict)
}

/// Go `resource.Default()`: `service.name` (`unknown_service:<executable>`), then
/// `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_SERVICE_NAME` from `env` (Go `fromEnv`), then the
/// `telemetry.sdk.*` attributes, under [`SCHEMA_URL`].
pub(crate) fn go_default_resource(env: &dyn EnvProvider) -> Resource {
    let sdk = TelemetryResourceDetector.detect();
    let telemetry_sdk = new_with_attributes(
        SCHEMA_URL,
        sdk.iter().map(|(k, v)| KeyValue::new(k.clone(), v.clone())),
    );
    // Go `detect`: each detector's resource is merged in order; merge errors are reported to
    // `otel.Handle` and the merged resource kept.
    [default_service_name(), from_env(env), telemetry_sdk]
        .iter()
        .fold(Resource::builder_empty().build(), |acc, r| merge(&acc, r).0)
}

/// Go `defaultServiceNameDetector`: `unknown_service:<executable base name>`.
// Divergence (ledger 7a): when the executable is unknown the name is the SDK's
// `unknown_service`, not Go's `unknown_service:go`.
fn default_service_name() -> Resource {
    let name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .map_or_else(
            || "unknown_service".to_owned(),
            |n| format!("unknown_service:{n}"),
        );
    new_with_attributes(SCHEMA_URL, [KeyValue::new("service.name", name)])
}

/// Go `fromEnv.Detect`: the trimmed `OTEL_RESOURCE_ATTRIBUTES` pairs, then the trimmed
/// `OTEL_SERVICE_NAME` as `service.name`; schemaless. Invalid pairs are dropped (Go's partial
/// error goes to `otel.Handle`).
fn from_env(env: &dyn EnvProvider) -> Resource {
    let attrs = env.var(RESOURCE_ATTRIBUTES_ENV).unwrap_or_default();
    let attrs = attrs.trim();
    let svc_name = env.var(SERVICE_NAME_ENV).unwrap_or_default();
    let svc_name = svc_name.trim();
    if attrs.is_empty() && svc_name.is_empty() {
        return Resource::builder_empty().build();
    }
    let from_attrs = construct_ot_resources(attrs);
    if svc_name.is_empty() {
        return from_attrs;
    }
    let service = new_with_attributes("", [KeyValue::new("service.name", svc_name.to_owned())]);
    merge(&from_attrs, &service).0
}

/// Go `constructOTResources`: `key=value` pairs separated by `,`, keys trimmed, values trimmed
/// and percent-decoded (the raw value kept when decoding fails).
fn construct_ot_resources(s: &str) -> Resource {
    if s.is_empty() {
        return Resource::builder_empty().build();
    }
    let attrs = s.split(',').filter_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        let val = path_unescape(v.trim()).unwrap_or_else(|| v.to_owned());
        Some(KeyValue::new(k.trim().to_owned(), val))
    });
    new_with_attributes("", attrs)
}

/// Go `url.PathUnescape`: `%XX` escapes decoded, `+` kept; `None` on a malformed escape.
fn path_unescape(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use opentelemetry::Key;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::test_support::env;

    fn attr(r: &Resource, key: &str) -> Option<String> {
        r.get(&Key::new(key.to_owned())).map(|v| v.to_string())
    }

    #[test]
    fn new_with_attributes_keeps_the_last_value_and_the_schema() {
        let r = new_with_attributes(
            "https://example.com/s",
            [
                KeyValue::new("a", "1"),
                KeyValue::new("a", "2"),
                KeyValue::new("", "x"),
            ],
        );
        assert_eq!(r.schema_url(), Some("https://example.com/s"));
        assert_eq!(r.len(), 1);
        assert_eq!(attr(&r, "a").as_deref(), Some("2"));
        assert_eq!(
            new_with_attributes("", [KeyValue::new("a", "1")]).schema_url(),
            None
        );
    }

    #[test]
    fn merge_prefers_the_second_resources_attributes() {
        let a = new_with_attributes("", [KeyValue::new("k", "a"), KeyValue::new("only_a", "1")]);
        let b = new_with_attributes("", [KeyValue::new("k", "b"), KeyValue::new("only_b", "2")]);
        let m = go_merge(&a, &b).unwrap();
        assert_eq!(attr(&m, "k").as_deref(), Some("b"));
        assert_eq!(attr(&m, "only_a").as_deref(), Some("1"));
        assert_eq!(attr(&m, "only_b").as_deref(), Some("2"));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn merge_follows_gos_schema_url_rule() {
        let s = |url: &str| new_with_attributes(url, [KeyValue::new("k", "v")]);
        assert_eq!(
            go_merge(&s(""), &s("x://b")).unwrap().schema_url(),
            Some("x://b")
        );
        assert_eq!(
            go_merge(&s("x://a"), &s("")).unwrap().schema_url(),
            Some("x://a")
        );
        assert_eq!(
            go_merge(&s("x://a"), &s("x://a")).unwrap().schema_url(),
            Some("x://a")
        );
        assert_eq!(go_merge(&s(""), &s("")).unwrap().schema_url(), None);
        assert_eq!(
            go_merge(&s("x://a"), &s("x://b")).unwrap_err(),
            "conflicting Schema URL: x://a and x://b"
        );
        let empty = Resource::builder_empty().build();
        assert_eq!(go_merge(&empty, &s("x://b")).unwrap(), s("x://b"));
    }

    #[test]
    fn default_resource_has_the_sdk_attributes_and_schema() {
        let r = go_default_resource(env(&[]).as_ref());
        assert_eq!(r.schema_url(), Some(SCHEMA_URL));
        assert_eq!(
            attr(&r, "telemetry.sdk.name").as_deref(),
            Some("opentelemetry")
        );
        assert_eq!(attr(&r, "telemetry.sdk.language").as_deref(), Some("rust"));
        assert_eq!(attr(&r, "telemetry.sdk.version").as_deref(), Some("0.33.1"));
        let exe = std::env::current_exe().unwrap();
        let exe = exe.file_name().unwrap().to_string_lossy();
        assert_eq!(
            attr(&r, "service.name"),
            Some(format!("unknown_service:{exe}"))
        );
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn default_resource_reads_the_injected_environment() {
        let r = go_default_resource(
            env(&[
                ("OTEL_SERVICE_NAME", " env-service "),
                (
                    "OTEL_RESOURCE_ATTRIBUTES",
                    " a=1, b = x%20y ,bad,service.name=attr-service,raw= %zz ,telemetry.sdk.language=go ",
                ),
            ])
            .as_ref(),
        );
        assert_eq!(attr(&r, "service.name").as_deref(), Some("env-service"));
        assert_eq!(attr(&r, "a").as_deref(), Some("1"));
        assert_eq!(attr(&r, "b").as_deref(), Some("x y"));
        assert_eq!(attr(&r, "bad"), None);
        // Go keeps the raw (untrimmed) value when it cannot be unescaped.
        assert_eq!(attr(&r, "raw").as_deref(), Some(" %zz "));
        assert_eq!(attr(&r, "telemetry.sdk.language").as_deref(), Some("rust"));
        assert_eq!(r.schema_url(), Some(SCHEMA_URL));
    }

    #[test]
    fn resource_attributes_name_the_service_without_otel_service_name() {
        let r = go_default_resource(
            env(&[("OTEL_RESOURCE_ATTRIBUTES", "service.name=attr-service")]).as_ref(),
        );
        assert_eq!(attr(&r, "service.name").as_deref(), Some("attr-service"));
    }
}
