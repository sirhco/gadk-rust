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

//! The OpenAPI route: `/api/v1/openapi.json`, the OpenAPI 3.1 description of the JSON API.

use std::sync::OnceLock;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::web::AppState;

/// The document's path, relative to `/api`.
pub(super) const PATH: &str = "/v1/openapi.json";

/// The document as written; its `info.version` is `0.0.0` until [`document`] sets it.
const DOC: &str = include_str!("openapi.json");

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(PATH, get(openapi))
}

/// The OpenAPI document with `info.version` set to the console's version. The document is
/// embedded and parsed once, and a test checks that it parses.
pub(crate) fn document() -> Value {
    static DOC_VALUE: OnceLock<Value> = OnceLock::new();
    DOC_VALUE
        .get_or_init(|| {
            let mut doc: Value = match serde_json::from_str(DOC) {
                Ok(doc) => doc,
                Err(e) => panic!("openapi.json: {e}"),
            };
            doc["info"]["version"] = json!(env!("CARGO_PKG_VERSION"));
            doc
        })
        .clone()
}

/// `GET /api/v1/openapi.json`: the document, with or without the BigQuery source.
async fn openapi() -> Json<Value> {
    Json(document())
}
