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

//! Unit tests of [`OpenApiSpec`] loading and the version check.

use std::path::PathBuf;

use serde_json::json;

use super::OpenApiSpec;
use crate::error::Error;

pub(crate) const PETSTORE: &str = include_str!("../tests/fixtures/petstore.yaml");
pub(crate) const ECHO_3_1: &str = include_str!("../tests/fixtures/echo-3.1.json");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn unsupported(text: &str) -> String {
    match text.parse::<OpenApiSpec>() {
        Err(Error::UnsupportedVersion { found }) => found,
        other => panic!("want UnsupportedVersion, got {other:?}"),
    }
}

#[test]
fn yaml_json_and_file_load_the_same_document() {
    let yaml: OpenApiSpec = PETSTORE.parse().unwrap();
    // Unquoted integer response keys (`200:`) become string keys.
    assert_eq!(
        yaml.document()["paths"]["/pets"]["get"]["responses"]["200"]["description"],
        json!("The pets.")
    );
    let json_text = serde_json::to_string(yaml.document()).unwrap();
    let json: OpenApiSpec = json_text.parse().unwrap();
    assert_eq!(json.document(), yaml.document());
    let file = OpenApiSpec::from_file(fixture("petstore.yaml")).unwrap();
    assert_eq!(file.document(), yaml.document());
    assert_eq!(yaml.version(), "3.0.3");
    assert!(yaml.is_3_0());
    assert_eq!(yaml.source_url(), None);
}

#[test]
fn paths_and_methods_keep_document_order() {
    let yaml: OpenApiSpec = PETSTORE.parse().unwrap();
    let order: Vec<(&str, Vec<&str>)> = yaml
        .path_order()
        .iter()
        .map(|(path, keys)| (path.as_str(), keys.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("/pets", vec!["get", "post"]),
            ("/pets/{petId}", vec!["parameters", "get", "delete"]),
        ]
    );
    let json: OpenApiSpec =
        r#"{"openapi": "3.1.0", "paths": {"/z": {"post": {}, "get": {}}, "/a": {}}}"#
            .parse()
            .unwrap();
    assert_eq!(
        json.path_order(),
        &[
            (
                "/z".to_string(),
                vec!["post".to_string(), "get".to_string()]
            ),
            ("/a".to_string(), vec![]),
        ]
    );
}

#[test]
fn a_3_1_json_document_loads() {
    let spec = OpenApiSpec::from_file(fixture("echo-3.1.json")).unwrap();
    assert_eq!(spec.version(), "3.1.0");
    assert!(!spec.is_3_0());
    assert_eq!(
        ECHO_3_1.parse::<OpenApiSpec>().unwrap().document(),
        spec.document()
    );
}

#[test]
fn only_3_0_and_3_1_are_supported() {
    assert!("openapi: 3.0.3\npaths: {}\n".parse::<OpenApiSpec>().is_ok());
    assert!("openapi: 3.1.0\npaths: {}\n".parse::<OpenApiSpec>().is_ok());
    assert_eq!(unsupported("swagger: \"2.0\"\npaths: {}\n"), "swagger 2.0");
    assert_eq!(unsupported("info: {title: x}\n"), "no openapi field");
    assert_eq!(unsupported("openapi: 4.0.0\n"), "4.0.0");
    assert_eq!(unsupported("openapi: 3.1\n"), "3.1");
    let err = "openapi: 4.0.0\n".parse::<OpenApiSpec>().unwrap_err();
    assert_eq!(
        err.to_string(),
        "unsupported OpenAPI version 4.0.0: only 3.0.x and 3.1.x are supported"
    );
}

#[test]
fn text_that_is_not_an_object_is_a_parse_error() {
    for text in ["just text", "[1, 2]", "{\"openapi\": "] {
        match text.parse::<OpenApiSpec>() {
            Err(Error::Parse { .. }) => {}
            other => panic!("{text:?}: want Parse, got {other:?}"),
        }
    }
}

#[test]
fn a_missing_file_is_a_parse_error_naming_the_path() {
    let path = fixture("missing.yaml");
    match OpenApiSpec::from_file(&path) {
        Err(Error::Parse { message }) => {
            assert!(
                message.starts_with(&format!("cannot read {}: ", path.display())),
                "{message}"
            );
        }
        other => panic!("want Parse, got {other:?}"),
    }
}
