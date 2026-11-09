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

//! Helpers for tests that read the requests gadk-model's fake HTTP server recorded (feature
//! `testutil`): the parts of a multipart upload and form-decoded query parameters.

use gadk_model::rest::test_util::RecordedRequest;

/// The parts of a `multipart/related` upload: each part's header block and body. Empty when
/// the request has no multipart boundary.
pub fn multipart_parts(req: &RecordedRequest) -> Vec<(String, String)> {
    let content_type = req.header("content-type");
    let Some(boundary) = content_type.strip_prefix("multipart/related; boundary=") else {
        return Vec::new();
    };
    req.body_text()
        .split(&format!("--{boundary}"))
        .filter_map(|part| part.strip_prefix("\r\n"))
        .filter_map(|part| {
            let (head, body) = part.split_once("\r\n\r\n")?;
            Some((
                head.to_owned(),
                body.strip_suffix("\r\n").unwrap_or(body).to_owned(),
            ))
        })
        .collect()
}

/// Go `r.URL.Query().Get(key)`, as an `Option`: the first value of query parameter `key`,
/// form-decoded.
pub fn query_param(req: &RecordedRequest, key: &str) -> Option<String> {
    url::form_urlencoded::parse(req.query.as_deref().unwrap_or_default().as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}
