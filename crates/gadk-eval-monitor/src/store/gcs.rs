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

//! Reports in a Google Cloud Storage bucket prefix, over the JSON API.

use bytes::Bytes;
use http::Method;
use serde_json::Value;

use crate::error::MonitorError;
use crate::gcp::{GcpHttp, encode};

const API: &str = "https://storage.googleapis.com/storage/v1/b";
const UPLOAD: &str = "https://storage.googleapis.com/upload/storage/v1/b";

pub(super) struct GcsStore {
    bucket: String,
    /// Empty, or ending with `/`.
    prefix: String,
    gcp: GcpHttp,
}

impl GcsStore {
    pub(super) fn new(bucket: &str, prefix: &str, gcp: GcpHttp) -> Self {
        let prefix = prefix.trim_matches('/');
        Self {
            bucket: bucket.to_owned(),
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
            gcp,
        }
    }

    /// The store under the object prefix `<prefix><name>/`.
    pub(super) fn child(&self, name: &str) -> Self {
        Self {
            bucket: self.bucket.clone(),
            prefix: format!("{}{name}/", self.prefix),
            gcp: self.gcp.clone(),
        }
    }

    /// Uploads `body` as `name` unless the object exists (`ifGenerationMatch=0`); `false` when it
    /// does.
    pub(super) async fn create(&self, name: &str, body: &str) -> Result<bool, MonitorError> {
        let url = format!(
            "{UPLOAD}/{}/o?uploadType=media&name={}&ifGenerationMatch=0",
            encode(&self.bucket),
            encode(&format!("{}{name}", self.prefix))
        );
        let reply = self
            .gcp
            .send(
                Method::POST,
                &url,
                Some(("application/json", Bytes::from(body.to_owned()))),
            )
            .await
            .map_err(MonitorError::Store)?;
        match reply.status {
            412 => Ok(false),
            _ if reply.ok() => Ok(true),
            _ => Err(MonitorError::Store(reply.error_text())),
        }
    }

    /// The object names directly under the prefix, with the prefix removed.
    pub(super) async fn names(&self) -> Result<Vec<String>, MonitorError> {
        let mut names = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!(
                "{API}/{}/o?prefix={}&delimiter=%2F",
                encode(&self.bucket),
                encode(&self.prefix)
            );
            if let Some(p) = &page {
                url.push_str(&format!("&pageToken={}", encode(p)));
            }
            let v = self
                .gcp
                .json(Method::GET, &url, None)
                .await
                .map_err(MonitorError::Store)?;
            for item in v
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = item
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|n| n.strip_prefix(self.prefix.as_str()))
                {
                    names.push(name.to_owned());
                }
            }
            page = v
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if page.is_none() {
                return Ok(names);
            }
        }
    }

    /// The contents of `name`, or `None` when the object does not exist.
    pub(super) async fn read(&self, name: &str) -> Result<Option<String>, MonitorError> {
        let url = format!(
            "{API}/{}/o/{}?alt=media",
            encode(&self.bucket),
            encode(&format!("{}{name}", self.prefix))
        );
        let reply = self
            .gcp
            .send(Method::GET, &url, None)
            .await
            .map_err(MonitorError::Store)?;
        match reply.status {
            404 => Ok(None),
            _ if reply.ok() => String::from_utf8(reply.body.to_vec())
                .map(Some)
                .map_err(|e| MonitorError::Store(format!("{name}: {e}"))),
            _ => Err(MonitorError::Store(reply.error_text())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::gcp::NoAuth;
    use crate::store::ReportStore;
    use crate::store::tests::{experiment, guard, report};
    use crate::testutil::{exchange, replay};

    const OBJECTS: &str = "https://storage.googleapis.com/storage/v1/b/evals/o";
    const UPLOADS: &str = "https://storage.googleapis.com/upload/storage/v1/b/evals/o";

    #[tokio::test]
    async fn put_takes_the_next_free_name() {
        let r = report("support", 1_800_000_000, true);
        let json = r.to_json().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media&name=monitors%2Fsupport-20270115T080000Z.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    412,
                    r#"{"error":{"code":412,"message":"At least one of the pre-conditions you specified did not hold."}}"#,
                ),
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media&name=monitors%2Fsupport-20270115T080000Z-2.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    200,
                    r#"{"name":"monitors/support-20270115T080000Z-2.json","generation":"1"}"#,
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors/", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.put(&r).await.unwrap(),
            "support-20270115T080000Z-2.json"
        );
    }

    #[tokio::test]
    async fn list_pages_and_reads_the_newest() {
        let older = report("support", 1_800_000_000, false);
        let newer = report("support", 1_800_003_600, true);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/support-20270115T080000Z.json"},{"name":"monitors/readme.txt"}],"nextPageToken":"t 2"}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2F&delimiter=%2F&pageToken=t%202"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/support-20270115T090000Z.json"}]}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}/monitors%2Fsupport-20270115T090000Z.json?alt=media"),
                    "",
                    200,
                    &newer.to_json().unwrap(),
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}/monitors%2Fsupport-20270115T080000Z.json?alt=media"),
                    "",
                    200,
                    &older.to_json().unwrap(),
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors", http, Arc::new(NoAuth)).unwrap();
        let list = store.list(10).await.unwrap();
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "support-20270115T090000Z.json",
                "support-20270115T080000Z.json"
            ]
        );
        assert_eq!(list[0].pass_rate, Some(1.0));
        assert_eq!(list[1].pass_rate, Some(0.0));
    }

    #[tokio::test]
    async fn get_reports_missing_objects_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "GET",
                    &format!("{OBJECTS}/support-20270115T080000Z.json?alt=media"),
                    "",
                    404,
                    r#"{"error":{"code":404,"message":"No such object"}}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}/support-20270115T090000Z.json?alt=media"),
                    "",
                    403,
                    r#"{"error":{"code":403,"message":"caller lacks storage.objects.get"}}"#,
                ),
            ],
        );
        let store = ReportStore::gcs_with_client("gs://evals", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store
                .get("support-20270115T080000Z.json")
                .await
                .unwrap_err()
                .to_string(),
            "report store: report support-20270115T080000Z.json not found"
        );
        assert_eq!(
            store
                .get("support-20270115T090000Z.json")
                .await
                .unwrap_err()
                .to_string(),
            "report store: HTTP 403: caller lacks storage.objects.get"
        );
    }

    #[tokio::test]
    async fn names_pages_without_reading_objects() {
        let dir = tempfile::tempdir().unwrap();
        // Replay fails any request not listed here, so a report read would fail the test.
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/support-20270115T080000Z.json"},{"name":"monitors/readme.txt"}],"nextPageToken":"t 2"}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2F&delimiter=%2F&pageToken=t%202"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/support-20270115T090000Z.json"}]}"#,
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.names(10).await.unwrap(),
            [
                "support-20270115T090000Z.json",
                "support-20270115T080000Z.json"
            ]
        );
    }

    #[tokio::test]
    async fn find_is_none_for_a_missing_object() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange(
                "GET",
                &format!("{OBJECTS}/support-20270115T080000Z.json?alt=media"),
                "",
                404,
                r#"{"error":{"code":404,"message":"No such object"}}"#,
            )],
        );
        let store = ReportStore::gcs_with_client("gs://evals", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.find("support-20270115T080000Z.json").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn put_experiment_takes_the_next_free_name_under_experiments() {
        let r = experiment("exp-e2e", 1_800_000_000);
        let json = r.to_json().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media\
                         &name=monitors%2Fexperiments%2Fexp-e2e-20270115T080000Z.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    412,
                    r#"{"error":{"code":412,"message":"At least one of the pre-conditions you specified did not hold."}}"#,
                ),
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media\
                         &name=monitors%2Fexperiments%2Fexp-e2e-20270115T080000Z-2.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    200,
                    r#"{"name":"monitors/experiments/exp-e2e-20270115T080000Z-2.json","generation":"1"}"#,
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors/", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.put_experiment(&r).await.unwrap(),
            "exp-e2e-20270115T080000Z-2.json"
        );
    }

    #[tokio::test]
    async fn experiments_without_a_prefix_are_under_experiments() {
        let r = experiment("exp-e2e", 1_800_000_000);
        let json = r.to_json().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media\
                         &name=experiments%2Fexp-e2e-20270115T080000Z.json&ifGenerationMatch=0"
                    ),
                    &json,
                    200,
                    r#"{"name":"experiments/exp-e2e-20270115T080000Z.json","generation":"1"}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=experiments%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"experiments/exp-e2e-20270115T080000Z.json"},{"name":"experiments/exp-e2e-20270115T090000Z.json"},{"name":"experiments/readme.txt"}]}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}/experiments%2Fexp-e2e-20270115T080000Z.json?alt=media"),
                    "",
                    200,
                    &json,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}/experiments%2Fexp-e2e-20270115T100000Z.json?alt=media"),
                    "",
                    404,
                    r#"{"error":{"code":404,"message":"No such object"}}"#,
                ),
            ],
        );
        let store = ReportStore::gcs_with_client("gs://evals", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.put_experiment(&r).await.unwrap(),
            "exp-e2e-20270115T080000Z.json"
        );
        assert_eq!(
            store.experiment_names(10).await.unwrap(),
            [
                "exp-e2e-20270115T090000Z.json",
                "exp-e2e-20270115T080000Z.json"
            ]
        );
        assert_eq!(
            store
                .find_experiment("exp-e2e-20270115T080000Z.json")
                .await
                .unwrap(),
            Some(r)
        );
        assert_eq!(
            store
                .find_experiment("exp-e2e-20270115T100000Z.json")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn guards_are_under_guards() {
        let r = guard("support-guard", 1_800_000_000);
        let json = r.to_json().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media\
                         &name=monitors%2Fguards%2Fsupport-guard-20270115T080000Z.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    412,
                    r#"{"error":{"code":412,"message":"At least one of the pre-conditions you specified did not hold."}}"#,
                ),
                exchange(
                    "POST",
                    &format!(
                        "{UPLOADS}?uploadType=media\
                         &name=monitors%2Fguards%2Fsupport-guard-20270115T080000Z-2.json\
                         &ifGenerationMatch=0"
                    ),
                    &json,
                    200,
                    r#"{"name":"monitors/guards/support-guard-20270115T080000Z-2.json","generation":"1"}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2Fguards%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/guards/support-guard-20270115T080000Z.json"},{"name":"monitors/guards/support-guard-20270115T080000Z-2.json"},{"name":"monitors/guards/readme.txt"}]}"#,
                ),
                exchange(
                    "GET",
                    &format!(
                        "{OBJECTS}/monitors%2Fguards%2Fsupport-guard-20270115T080000Z-2.json?alt=media"
                    ),
                    "",
                    200,
                    &json,
                ),
                exchange(
                    "GET",
                    &format!(
                        "{OBJECTS}/monitors%2Fguards%2Fsupport-guard-20270115T100000Z.json?alt=media"
                    ),
                    "",
                    404,
                    r#"{"error":{"code":404,"message":"No such object"}}"#,
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors/", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.put_guard(&r).await.unwrap(),
            "support-guard-20270115T080000Z-2.json"
        );
        assert_eq!(
            store.guard_names(10).await.unwrap(),
            [
                "support-guard-20270115T080000Z-2.json",
                "support-guard-20270115T080000Z.json"
            ]
        );
        assert_eq!(
            store
                .find_guard("support-guard-20270115T080000Z-2.json")
                .await
                .unwrap(),
            Some(r)
        );
        assert_eq!(
            store
                .find_guard("support-guard-20270115T100000Z.json")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn report_names_skip_the_experiments_prefix() {
        let dir = tempfile::tempdir().unwrap();
        // The delimiter listing returns experiments/ as a prefix, not as objects.
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/support-20270115T080000Z.json"}],"prefixes":["monitors/experiments/"]}"#,
                ),
                exchange(
                    "GET",
                    &format!("{OBJECTS}?prefix=monitors%2Fexperiments%2F&delimiter=%2F"),
                    "",
                    200,
                    r#"{"items":[{"name":"monitors/experiments/exp-e2e-20270115T090000Z.json"}]}"#,
                ),
            ],
        );
        let store =
            ReportStore::gcs_with_client("gs://evals/monitors", http, Arc::new(NoAuth)).unwrap();
        assert_eq!(
            store.names(10).await.unwrap(),
            ["support-20270115T080000Z.json"]
        );
        assert_eq!(
            store.experiment_names(10).await.unwrap(),
            ["exp-e2e-20270115T090000Z.json"]
        );
    }
}
