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

//! The OpenAPI checks: response bodies validated against an OpenAPI 3.1 document with
//! `jsonschema`, and the tests of the checker itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use http::StatusCode;
use jsonschema::{Draft, Registry, Validator};
use serde_json::{Value, json};

/// The URI the document is registered under; every validator `$ref`s into it.
const DOC_URI: &str = "urn:gadk-console:openapi";
/// The schema of a body on a path the document does not list: the API fallback's error.
const API_ERROR: &str = "#/components/schemas/ApiError";

/// An OpenAPI document, its JSON Schema registry and one cached validator per schema pointer.
struct Checker {
    doc: Value,
    registry: Registry<'static>,
    validators: Mutex<HashMap<String, Arc<Validator>>>,
}

impl Checker {
    /// The checker of `doc`, registered whole under [`DOC_URI`] so that its `#/components/…`
    /// references resolve.
    fn new(doc: Value) -> Self {
        let registry = Registry::new()
            .draft(Draft::Draft202012)
            .add(DOC_URI, doc.clone())
            .and_then(|builder| builder.prepare())
            .unwrap_or_else(|e| panic!("the OpenAPI registry: {e}"));
        Self {
            doc,
            registry,
            validators: Mutex::default(),
        }
    }

    /// The validator of the schema at the URI fragment `pointer` (`#/…`), built once.
    fn validator(&self, pointer: &str) -> Result<Arc<Validator>, String> {
        let mut cache = self.validators.lock().unwrap();
        if let Some(validator) = cache.get(pointer) {
            return Ok(validator.clone());
        }
        let validator = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .with_registry(&self.registry)
            .build(&json!({ "$ref": format!("{DOC_URI}{pointer}") }))
            .map_err(|e| format!("schema {pointer}: {e}"))?;
        let validator = Arc::new(validator);
        cache.insert(pointer.to_owned(), validator.clone());
        Ok(validator)
    }

    /// Validates `body` against the schema at the URI fragment `pointer`.
    fn check_pointer(&self, pointer: &str, body: &Value) -> Result<(), String> {
        let validator = self.validator(pointer)?;
        let errors: Vec<String> = validator
            .iter_errors(body)
            .map(|e| format!("{e} (at {})", e.instance_path()))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("schema {pointer}: {}", errors.join("; ")))
        }
    }

    /// Validates the `status` response `body` of `GET path` (query and all, percent-encoded as
    /// sent): against the operation's response schema when a documented path template matches,
    /// else against [`API_ERROR`]. An undocumented status is an error.
    fn validate(&self, path: &str, status: StatusCode, body: &Value) -> Result<(), String> {
        let raw = path.split('?').next().unwrap_or(path);
        let template = self.doc["paths"]
            .as_object()
            .and_then(|paths| paths.keys().find(|t| template_matches(t, raw)));
        let pointer = match template {
            Some(template) => {
                let mut response = response_pointer(template, status);
                let Some(object) = self.doc.pointer(&response) else {
                    return Err(format!(
                        "{path}: status {}: not documented at {}",
                        status.as_u16(),
                        fragment(&response)
                    ));
                };
                // A response may be a reference to `#/components/responses/…`.
                if let Some(target) = object["$ref"].as_str() {
                    response = target.trim_start_matches('#').to_owned();
                }
                fragment(&format!("{response}/content/application~1json/schema"))
            }
            None => API_ERROR.to_owned(),
        };
        self.check_pointer(&pointer, body)
            .map_err(|e| format!("{path}: status {}: {e}", status.as_u16()))
    }
}

/// Whether the raw request path `path` matches the path template `template`: the same
/// segments, where `{…}` matches any one non-empty segment (`team%2Fgreet` is one segment).
fn template_matches(template: &str, path: &str) -> bool {
    let template: Vec<&str> = template.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    template.len() == path.len()
        && template.iter().zip(&path).all(|(t, p)| {
            if t.starts_with('{') && t.ends_with('}') {
                !p.is_empty()
            } else {
                t == p
            }
        })
}

/// The JSON pointer of the `status` response of `GET template`: `/` in the path key is `~1`
/// (and `~` is `~0`).
fn response_pointer(template: &str, status: StatusCode) -> String {
    let key = template.replace('~', "~0").replace('/', "~1");
    format!("/paths/{key}/get/responses/{}", status.as_u16())
}

/// The URI fragment of the JSON pointer `pointer`: `#` plus the pointer with `{` and `}`
/// percent-encoded.
fn fragment(pointer: &str) -> String {
    format!("#{}", pointer.replace('{', "%7B").replace('}', "%7D"))
}

/// A small document in the shape of the console's: one templated path, a `$ref`'d row with a
/// timestamp, a nullable field and a tuple, a 404 that references `components/responses`, the
/// error schema and the 405 response.
fn sample() -> Checker {
    Checker::new(json!({
        "openapi": "3.1.0",
        "info": { "title": "sample", "version": "0.0.0" },
        "paths": {
            "/api/v1/things/{id}/parts": {
                "get": {
                    "operationId": "parts",
                    "responses": {
                        "200": {
                            "description": "The parts.",
                            "content": { "application/json": { "schema": {
                                "$ref": "#/components/schemas/Part"
                            } } }
                        },
                        "404": { "$ref": "#/components/responses/NotFound" }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "Part": {
                    "type": "object",
                    "required": ["at", "note", "pair"],
                    "properties": {
                        "at": { "type": "string", "format": "date-time" },
                        "note": { "type": ["string", "null"] },
                        "pair": {
                            "type": "array",
                            "prefixItems": [{ "type": "string" }, { "type": "integer" }],
                            "items": false
                        }
                    }
                },
                "ApiError": {
                    "type": "object",
                    "required": ["error"],
                    "properties": { "error": {
                        "type": "object",
                        "required": ["kind", "message", "hint"],
                        "properties": {
                            "kind": { "type": "string" },
                            "message": { "type": "string" },
                            "hint": { "type": ["string", "null"] }
                        }
                    } }
                }
            },
            "responses": {
                "NotFound": {
                    "description": "Not found.",
                    "content": { "application/json": { "schema": {
                        "$ref": "#/components/schemas/ApiError"
                    } } }
                },
                "MethodNotAllowed": {
                    "description": "A write method.",
                    "content": { "application/json": { "schema": {
                        "$ref": "#/components/schemas/ApiError"
                    } } }
                }
            }
        }
    }))
}

fn part() -> Value {
    json!({ "at": "2026-11-16T12:00:00Z", "note": null, "pair": ["m", 3] })
}

fn error() -> Value {
    json!({ "error": { "kind": "not_found", "message": "no such API route", "hint": null } })
}

#[test]
fn a_placeholder_matches_one_non_empty_segment() {
    let t = "/api/v1/things/{id}/parts";
    assert!(template_matches(t, "/api/v1/things/a-1/parts"));
    assert!(template_matches(t, "/api/v1/things/team%2Fgreet/parts"));
    assert!(!template_matches(t, "/api/v1/things//parts"));
    assert!(!template_matches(t, "/api/v1/things/a/b/parts"));
    assert!(!template_matches(t, "/api/v1/things/a/parts/x"));
    assert!(!template_matches(t, "/api/v1/other/a/parts"));
    assert!(template_matches("/api/v1/users", "/api/v1/users"));
    assert!(!template_matches("/api/v1/users", "/api/v1/users/"));
}

#[test]
fn pointers_escape_the_path_key() {
    let response = response_pointer("/api/v1/sessions/{id}/turns", StatusCode::OK);
    assert_eq!(
        response,
        "/paths/~1api~1v1~1sessions~1{id}~1turns/get/responses/200"
    );
    assert_eq!(
        fragment(&format!("{response}/content/application~1json/schema")),
        "#/paths/~1api~1v1~1sessions~1%7Bid%7D~1turns/get/responses/200/content/application~1json/schema"
    );
}

#[test]
fn a_documented_response_validates_through_its_refs() {
    let checker = sample();
    let path = "/api/v1/things/team%2Fgreet/parts?range=7d";
    assert_eq!(checker.validate(path, StatusCode::OK, &part()), Ok(()));
    assert_eq!(
        checker.validate(path, StatusCode::NOT_FOUND, &error()),
        Ok(())
    );
    let e = checker
        .validate(path, StatusCode::NOT_FOUND, &part())
        .unwrap_err();
    assert!(
        e.starts_with("/api/v1/things/team%2Fgreet/parts?range=7d: status 404: schema #/components/responses/NotFound/content/application~1json/schema: "),
        "{e}"
    );
}

#[test]
fn a_body_that_breaks_the_schema_names_the_path_status_and_schema() {
    let checker = sample();
    let path = "/api/v1/things/a/parts";
    let mut missing = part();
    missing.as_object_mut().unwrap().remove("note");
    let e = checker
        .validate(path, StatusCode::OK, &missing)
        .unwrap_err();
    assert!(
        e.starts_with(
            "/api/v1/things/a/parts: status 200: schema #/paths/~1api~1v1~1things~1%7Bid%7D~1parts/get/responses/200/content/application~1json/schema: "
        ),
        "{e}"
    );
    assert!(e.contains("\"note\" is a required property"), "{e}");

    for (bad, why) in [
        (json!({ "at": 1, "note": null, "pair": ["m", 3] }), "at"),
        (json!({ "at": "x", "note": 2, "pair": ["m", 3] }), "note"),
        (
            json!({ "at": "x", "note": null, "pair": [3, "m"] }),
            "pair order",
        ),
        (
            json!({ "at": "x", "note": null, "pair": ["m", 3, 4] }),
            "pair length",
        ),
    ] {
        assert!(
            checker.validate(path, StatusCode::OK, &bad).is_err(),
            "{why}"
        );
    }
}

#[test]
fn an_undocumented_status_fails() {
    let e = sample()
        .validate("/api/v1/things/a/parts", StatusCode::BAD_GATEWAY, &error())
        .unwrap_err();
    assert_eq!(
        e,
        "/api/v1/things/a/parts: status 502: not documented at #/paths/~1api~1v1~1things~1%7Bid%7D~1parts/get/responses/502"
    );
}

#[test]
fn an_unknown_path_must_be_the_api_error() {
    let checker = sample();
    assert_eq!(
        checker.validate("/api/v1/nope", StatusCode::NOT_FOUND, &error()),
        Ok(())
    );
    let e = checker
        .validate("/api/v1/things//parts", StatusCode::NOT_FOUND, &part())
        .unwrap_err();
    assert!(
        e.starts_with("/api/v1/things//parts: status 404: schema #/components/schemas/ApiError: "),
        "{e}"
    );
}

#[test]
fn an_explicit_pointer_validates_and_is_cached() {
    let checker = sample();
    let pointer = "#/components/responses/MethodNotAllowed/content/application~1json/schema";
    assert_eq!(checker.check_pointer(pointer, &error()), Ok(()));
    assert!(checker.check_pointer(pointer, &json!({})).is_err());
    assert_eq!(checker.validators.lock().unwrap().len(), 1);
    assert!(
        checker
            .check_pointer("#/components/schemas/Nope", &error())
            .is_err()
    );
}
