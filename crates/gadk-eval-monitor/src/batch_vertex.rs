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

//! Vertex AI batch prediction as a [`BatchPredictor`]: the lines are staged in Cloud Storage,
//! one job grades them with a Gemini model, and the replies are read back from the job's output.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gadk_gemini::{ClientConfig, MapEnv, resolve};
use gadk_model::{Backend, HttpClient};
use http::Method;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::batch::{BatchLine, BatchPredictor, BatchReply, BatchResult};
use crate::error::MonitorError;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource, encode};
use crate::spec::is_location;
use crate::store::gcs::{list_all, put_object, read_text};
use crate::store::{Gs, split_gs, split_gs_url};

/// The API version of the batch prediction endpoints.
const API_VERSION: &str = "v1";

/// How often a job is polled at first ([`VertexBatchPredictor::new`]).
pub(crate) const DEFAULT_POLL: Duration = Duration::from_secs(30);

/// How long a job is polled every `poll`; after that, every `2 × poll`.
const FAST_POLL_FOR: Duration = Duration::from_secs(600);

/// The job states that end a wait with the job's output.
const SUCCEEDED: [&str; 2] = ["JOB_STATE_SUCCEEDED", "JOB_STATE_PARTIALLY_SUCCEEDED"];

/// The job states that end a wait with an error.
const FAILED: [&str; 3] = [
    "JOB_STATE_FAILED",
    "JOB_STATE_CANCELLED",
    "JOB_STATE_EXPIRED",
];

/// How many status checks in a row may fail transiently before the wait gives up.
const MAX_TRANSIENT_CHECKS: u32 = 5;

/// The `--batch-staging` error for `staging`.
pub(crate) fn staging_error(staging: &str) -> MonitorError {
    MonitorError::Spec(format!(
        "--batch-staging must be gs://<bucket>/<prefix>, got {staging}"
    ))
}

/// `gs://<bucket>/<prefix>` as `(bucket, prefix)`, with one trailing `/` dropped; `None` unless
/// the bucket is a valid bucket name and the prefix is not empty.
pub(crate) fn parse_staging(staging: &str) -> Option<(String, String)> {
    match split_gs(staging.strip_suffix('/').unwrap_or(staging)) {
        Gs::Object(bucket, prefix) if !prefix.is_empty() => {
            Some((bucket.to_owned(), prefix.to_owned()))
        }
        _ => None,
    }
}

/// The Vertex AI batch prediction service as a [`BatchPredictor`], grading with a Gemini model.
///
/// Each [`BatchPredictor::predict`] is one batch prediction job of `model` in `project` /
/// `location`. The lines are uploaded to `<staging>/<run-id>/input.jsonl` and the job writes
/// under `<staging>/<run-id>/output/`, where `<run-id>` is `gadk-<UTC yyyymmddThhmmssZ>-<4 hex>`;
/// nothing is deleted afterwards. The job is polled every 30 s for the first 10 minutes and
/// every minute after that; when it has not finished within `timeout` it is cancelled.
#[non_exhaustive]
pub struct VertexBatchPredictor {
    gcp: GcpHttp,
    /// `https://<host>/v1/`: job names are relative to it.
    api: String,
    project: String,
    location: String,
    model: String,
    bucket: String,
    /// Not empty, without a trailing `/`.
    prefix: String,
    timeout: Duration,
    poll: Duration,
}

impl fmt::Debug for VertexBatchPredictor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VertexBatchPredictor")
            .field("api", &self.api)
            .field("project", &self.project)
            .field("location", &self.location)
            .field("model", &self.model)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("timeout", &self.timeout)
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

impl VertexBatchPredictor {
    /// Jobs of the Gemini model `model` in `project` / `location`, staged under `staging`
    /// (`gs://<bucket>/<prefix>`), each bounded by `timeout`, with Application Default
    /// Credentials. The API host follows the Gemini client's Vertex AI rule:
    /// `aiplatform.googleapis.com` for `global`, `aiplatform.<location>.rep.googleapis.com` for
    /// `us` and `eu`, and `<location>-aiplatform.googleapis.com` for a region. Must be called
    /// inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (`--batch-staging must be gs://<bucket>/<prefix>, got …`) for a
    /// malformed `staging`; [`MonitorError::Judge`] when `location` does not match
    /// `[A-Za-z0-9-]+` or no credentials can be built.
    pub fn new(
        project: &str,
        location: &str,
        model: &str,
        staging: &str,
        timeout: Duration,
    ) -> Result<Self, MonitorError> {
        check(location, staging)?;
        let auth = AdcTokenSource::adc()
            .map_err(|e| MonitorError::Judge(format!("Application Default Credentials: {e}")))?;
        Self::with_http(
            project,
            location,
            model,
            staging,
            timeout,
            GcpHttp::default_http(),
            Arc::new(auth),
            DEFAULT_POLL,
        )
    }

    /// [`VertexBatchPredictor::new`] with an explicit HTTP client and credentials, polling every
    /// `poll` (tests).
    ///
    /// # Errors
    ///
    /// As [`VertexBatchPredictor::new`], for `staging` and `location`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_http(
        project: &str,
        location: &str,
        model: &str,
        staging: &str,
        timeout: Duration,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn TokenSource>,
        poll: Duration,
    ) -> Result<Self, MonitorError> {
        let (bucket, prefix) = check(location, staging)?;
        Ok(Self {
            gcp: GcpHttp::new(http, auth),
            api: api_base(project, location)?,
            project: project.to_owned(),
            location: location.to_owned(),
            model: model.to_owned(),
            bucket,
            prefix,
            timeout,
            poll,
        })
    }

    /// Uploads, creates, waits for and collects one job.
    async fn run(&self, lines: &[BatchLine]) -> Result<BatchResult, MonitorError> {
        let run_id = run_id(SystemTime::now());
        let dir = format!("{}/{run_id}", self.prefix);
        let input = format!("{dir}/input.jsonl");
        put_object(&self.gcp, &self.bucket, &input, &input_lines(lines), false)
            .await
            .map_err(helper_error)?;
        let name = self.create(&run_id, &input, &dir).await?;
        let output = match tokio::time::timeout(self.timeout, self.wait(&name)).await {
            Ok(output) => output?,
            Err(_) => {
                // Best effort: the error below is returned whether or not the cancel succeeds.
                let cancel = format!("{}{name}:cancel", self.api);
                let _ = self.gcp.json(Method::POST, &cancel, Some(&json!({}))).await;
                return Err(MonitorError::Judge(format!(
                    "batch job {name} did not finish within {:?}; cancelled",
                    self.timeout
                )));
            }
        };
        let replies = self.collect(&output).await?;
        Ok(BatchResult::new(name, replies))
    }

    /// Creates the job `run_id` reading `input` and writing under `<dir>/output/`; its name.
    async fn create(&self, run_id: &str, input: &str, dir: &str) -> Result<String, MonitorError> {
        let url = format!(
            "{}projects/{}/locations/{}/batchPredictionJobs",
            self.api,
            encode(&self.project),
            encode(&self.location)
        );
        let body = json!({
            "displayName": run_id,
            "model": format!("publishers/google/models/{}", self.model),
            "inputConfig": {
                "instancesFormat": "jsonl",
                "gcsSource": { "uris": [format!("gs://{}/{input}", self.bucket)] },
            },
            "outputConfig": {
                "predictionsFormat": "jsonl",
                "gcsDestination": {
                    "outputUriPrefix": format!("gs://{}/{dir}/output/", self.bucket),
                },
            },
        });
        let job = self
            .gcp
            .json(Method::POST, &url, Some(&body))
            .await
            .map_err(|e| MonitorError::Judge(format!("batch: {e}")))?;
        job.get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| MonitorError::Judge("batch: the created job has no name".into()))
    }

    /// Polls the job `name` until it ends; its output directory once it succeeded.
    ///
    /// A transient failure of a status check ([`StatusError::Transient`]) is retried at the
    /// next poll; after [`MAX_TRANSIENT_CHECKS`] in a row, or on any other failure, the wait ends
    /// with `batch: <error>`. A successful check resets the count.
    async fn wait(&self, name: &str) -> Result<String, MonitorError> {
        let url = format!("{}{name}", self.api);
        let started = Instant::now();
        let mut transient = 0;
        loop {
            let job = match self.status(&url).await {
                Ok(job) => {
                    transient = 0;
                    job
                }
                Err(StatusError::Transient(e)) => {
                    transient += 1;
                    if transient >= MAX_TRANSIENT_CHECKS {
                        return Err(MonitorError::Judge(format!("batch: {e}")));
                    }
                    tokio::time::sleep(self.poll_interval(started.elapsed())).await;
                    continue;
                }
                Err(StatusError::Fatal(e)) => {
                    return Err(MonitorError::Judge(format!("batch: {e}")));
                }
            };
            let state = job.get("state").and_then(Value::as_str).unwrap_or("");
            if SUCCEEDED.contains(&state) {
                return job
                    .pointer("/outputInfo/gcsOutputDirectory")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        MonitorError::Judge(format!("batch: job {name} has no output directory"))
                    });
            }
            if FAILED.contains(&state) {
                let message = job
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(MonitorError::Judge(format!(
                    "batch job {name} {state}: {message}"
                )));
            }
            tokio::time::sleep(self.poll_interval(started.elapsed())).await;
        }
    }

    /// One status check of the job at `url`: its JSON, else why not. A transport or credential
    /// failure (no HTTP status), HTTP 429 and HTTP 5xx are transient; any other status, or a
    /// 2xx reply that is not JSON, is fatal. The texts are those of `GcpHttp::json`.
    async fn status(&self, url: &str) -> Result<Value, StatusError> {
        let reply = self
            .gcp
            .send(Method::GET, url, None)
            .await
            .map_err(StatusError::Transient)?;
        if reply.ok() {
            return serde_json::from_slice(&reply.body)
                .map_err(|e| StatusError::Fatal(format!("decoding the reply: {e}")));
        }
        if reply.status == 429 || (500..600).contains(&reply.status) {
            Err(StatusError::Transient(reply.error_text()))
        } else {
            Err(StatusError::Fatal(reply.error_text()))
        }
    }

    /// The wait before the next poll, `elapsed` after the first: `poll` for the first 10
    /// minutes, then `2 × poll`.
    fn poll_interval(&self, elapsed: Duration) -> Duration {
        if elapsed < FAST_POLL_FOR {
            self.poll
        } else {
            self.poll * 2
        }
    }

    /// The replies in every `.jsonl` object under the output directory `output`, at any depth.
    async fn collect(&self, output: &str) -> Result<HashMap<String, BatchReply>, MonitorError> {
        let (bucket, path) = split_gs_url(output)
            .and_then(|(bucket, path)| Some((bucket, path?)))
            .ok_or_else(|| {
                MonitorError::Judge(format!(
                    "batch: the output directory {output:?} is not gs://<bucket>/<path>"
                ))
            })?;
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut replies = HashMap::new();
        for name in list_all(&self.gcp, bucket, &prefix)
            .await
            .map_err(helper_error)?
        {
            if !name.ends_with(".jsonl") {
                continue;
            }
            let text = read_text(&self.gcp, bucket, &name)
                .await
                .map_err(helper_error)?
                .unwrap_or_default();
            replies.extend(text.lines().filter_map(output_line));
        }
        Ok(replies)
    }
}

impl BatchPredictor for VertexBatchPredictor {
    fn predict<'a>(
        &'a self,
        lines: &'a [BatchLine],
    ) -> BoxFuture<'a, Result<BatchResult, MonitorError>> {
        Box::pin(self.run(lines))
    }
}

/// Why a status check failed.
enum StatusError {
    /// Worth retrying: no HTTP status, HTTP 429 or HTTP 5xx.
    Transient(String),
    /// Not worth retrying.
    Fatal(String),
}

/// The bucket and prefix of `staging`, once `location` and `staging` are checked.
fn check(location: &str, staging: &str) -> Result<(String, String), MonitorError> {
    let parsed = parse_staging(staging).ok_or_else(|| staging_error(staging))?;
    if !is_location(location) {
        return Err(MonitorError::Judge(format!(
            "batch location {location:?} must match [A-Za-z0-9-]+"
        )));
    }
    Ok(parsed)
}

/// `https://<host>/v1/` for `location`, with the host the Gemini client uses on Vertex AI.
fn api_base(project: &str, location: &str) -> Result<String, MonitorError> {
    let cfg = ClientConfig {
        backend: Some(Backend::VertexAi),
        project: Some(project.to_owned()),
        location: Some(location.to_owned()),
        // The host depends only on the location, never on the environment.
        env: Arc::new(MapEnv(HashMap::new())),
        ..Default::default()
    };
    let base = resolve(&cfg)
        .map_err(|e| MonitorError::Judge(format!("batch: {e}")))?
        .base_url;
    Ok(format!("{}/{API_VERSION}/", base.trim_end_matches('/')))
}

/// `gadk-<UTC yyyymmddThhmmssZ>-<4 hex>` for `now`: the hex is the low 16 bits of its subsecond
/// nanoseconds.
fn run_id(now: SystemTime) -> String {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let stamp = DateTime::<Utc>::from(now).format("%Y%m%dT%H%M%SZ");
    format!("gadk-{stamp}-{:04x}", nanos & 0xffff)
}

/// The input file: one request line per [`BatchLine`], each ending with a newline.
fn input_lines(lines: &[BatchLine]) -> String {
    let mut out = String::new();
    for line in lines {
        let request = json!({
            "key": line.key,
            "request": {
                "contents": [{ "role": "user", "parts": [{ "text": line.prompt }] }],
                "generationConfig": { "temperature": 0, "responseMimeType": "application/json" },
            },
        });
        out.push_str(&request.to_string());
        out.push('\n');
    }
    out
}

/// The key and reply of one output line; `None` when the line is not JSON or has no string
/// `key`.
fn output_line(line: &str) -> Option<(String, BatchReply)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let key = v.get("key")?.as_str()?.to_owned();
    if let Some(status) = v
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some((key, BatchReply::Error(status.to_owned())));
    }
    let Some(candidate) = v.pointer("/response/candidates/0") else {
        return Some((key, BatchReply::Error("no candidates".into())));
    };
    let text: String = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    Some((key, BatchReply::Text(text)))
}

/// A Cloud Storage helper's error as `MonitorError::Judge("batch: <its message>")`.
fn helper_error(e: MonitorError) -> MonitorError {
    let text = match e {
        MonitorError::Spec(t)
        | MonitorError::Source(t)
        | MonitorError::Judge(t)
        | MonitorError::Store(t) => t,
        MonitorError::Io(e) => e.to_string(),
    };
    MonitorError::Judge(format!("batch: {text}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gadk_model::rest::test_util::{FakeResponse, RecordedRequest, ScriptedClient};
    use regex::Regex;

    use crate::gcp::NoAuth;

    use super::*;

    const API: &str = "https://aiplatform.us.rep.googleapis.com/v1/";
    const JOB: &str = "projects/p/locations/us/batchPredictionJobs/42";
    const UPLOADS: &str = "https://storage.googleapis.com/upload/storage/v1/b/bkt/o";
    const OBJECTS: &str = "https://storage.googleapis.com/storage/v1/b/bkt/o";
    const OUT_DIR: &str =
        "gs://bkt/stage/gadk-x/output/prediction-model-2026-11-28T00:11:06.939882Z";

    /// A success line as the live job wrote it (trimmed): a reply with a thought signature.
    const SUCCESS: &str = r#"{"key":"r0","request":{"contents":[{"parts":[{"text":"Grade."}],"role":"user"}],"generationConfig":{"responseMimeType":"application/json","temperature":0},"labels":null},"status":"","response":{"candidates":[{"content":{"parts":[{"text":"{\"pass\": true, \"score\": 5, \"rationale\": \"correct\"}","thoughtSignature":"AY89a1+YyND2"}],"role":"model"},"finishReason":"STOP"}],"modelVersion":"gemini-3.8-flash","usageMetadata":{"promptTokenCount":63,"totalTokenCount":373}},"processed_time":"2026-11-28T00:16:10.400922+00:00"}"#;

    /// A reply split over a thought part and two text parts.
    const PARTS: &str = r#"{"key":"r1","status":"","response":{"candidates":[{"content":{"parts":[{"text":"thinking","thought":true},{"text":"{\"pass\": false, "},{"text":"\"score\": 2}"}],"role":"model"}}]}}"#;

    const FAILED_LINE: &str =
        r#"{"key":"r2","status":"Bad Request: invalid argument","request":{}}"#;

    const NO_CANDIDATES: &str =
        r#"{"key":"r3","status":"","response":{"promptFeedback":{"blockReason":"SAFETY"}}}"#;

    fn lines() -> Vec<BatchLine> {
        vec![
            BatchLine::new("r0", "Grade \"this\".\nReply with JSON."),
            BatchLine::new("r1", "Grade that."),
        ]
    }

    fn predictor(http: Arc<ScriptedClient>, timeout: Duration) -> VertexBatchPredictor {
        VertexBatchPredictor::with_http(
            "p",
            "us",
            "gemini-3.8-flash",
            "gs://bkt/stage/",
            timeout,
            http,
            Arc::new(NoAuth),
            Duration::from_millis(1),
        )
        .unwrap()
    }

    /// `(method, url)` of each request.
    fn calls(requests: &[RecordedRequest]) -> Vec<(String, String)> {
        requests
            .iter()
            .map(|r| (r.method.clone(), r.url.clone()))
            .collect()
    }

    /// A client answering the upload, the create, `states` (one per poll, the last repeating)
    /// and, from `objects`, the listing pages and object reads.
    fn service(
        states: &'static [&'static str],
        objects: Vec<(String, String)>,
    ) -> Arc<ScriptedClient> {
        let polls = Mutex::new(0usize);
        ScriptedClient::new(move |r| {
            let url = r.url.as_str();
            if url.starts_with(UPLOADS) {
                return FakeResponse::json(r#"{"name":"stage/x/input.jsonl"}"#);
            }
            if url == format!("{API}projects/p/locations/us/batchPredictionJobs") {
                return FakeResponse::json(format!(
                    r#"{{"name":"{JOB}","state":"JOB_STATE_PENDING"}}"#
                ));
            }
            if url == format!("{API}{JOB}") {
                let mut n = polls.lock().unwrap();
                let state = states[(*n).min(states.len() - 1)];
                *n += 1;
                return FakeResponse::json(
                    json!({
                        "name": JOB,
                        "state": state,
                        "error": {"code": 3, "message": "quota exceeded"},
                        "outputInfo": {"gcsOutputDirectory": OUT_DIR},
                    })
                    .to_string(),
                );
            }
            if url == format!("{API}{JOB}:cancel") {
                return FakeResponse::json("{}");
            }
            for (want, body) in &objects {
                if url == *want {
                    return FakeResponse::json(body.clone());
                }
            }
            FakeResponse::json_status(404, r#"{"error":{"code":404,"message":"No such object"}}"#)
        })
    }

    #[test]
    fn staging_needs_a_bucket_and_a_prefix() {
        assert_eq!(
            parse_staging("gs://bkt/stage/batch/"),
            Some(("bkt".into(), "stage/batch".into()))
        );
        assert_eq!(
            parse_staging("gs://bkt/stage"),
            Some(("bkt".into(), "stage".into()))
        );
        for bad in [
            "gs://bkt",
            "gs://bkt/",
            "gs:///stage",
            "gs://Bad/stage",
            "s3://bkt/stage",
            "bkt/stage",
        ] {
            assert_eq!(parse_staging(bad), None, "{bad}");
        }
        assert_eq!(
            staging_error("gs://bkt").to_string(),
            "monitor spec: --batch-staging must be gs://<bucket>/<prefix>, got gs://bkt"
        );
    }

    #[test]
    fn the_host_follows_the_gemini_vertex_rule() {
        assert_eq!(
            api_base("p", "global").unwrap(),
            "https://aiplatform.googleapis.com/v1/"
        );
        assert_eq!(api_base("p", "us").unwrap(), API);
        assert_eq!(
            api_base("p", "eu").unwrap(),
            "https://aiplatform.eu.rep.googleapis.com/v1/"
        );
        assert_eq!(
            api_base("p", "us-central1").unwrap(),
            "https://us-central1-aiplatform.googleapis.com/v1/"
        );
    }

    #[test]
    fn a_bad_location_or_staging_is_refused() {
        let http = ScriptedClient::json("{}");
        let err = VertexBatchPredictor::with_http(
            "p",
            "us/x",
            "m",
            "gs://bkt/stage",
            Duration::from_secs(1),
            http.clone(),
            Arc::new(NoAuth),
            DEFAULT_POLL,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "judge: batch location \"us/x\" must match [A-Za-z0-9-]+"
        );
        let err = VertexBatchPredictor::with_http(
            "p",
            "us",
            "m",
            "gs://bkt",
            Duration::from_secs(1),
            http,
            Arc::new(NoAuth),
            DEFAULT_POLL,
        )
        .unwrap_err();
        assert!(matches!(err, MonitorError::Spec(_)), "{err}");
    }

    #[test]
    fn run_ids_carry_the_time_and_four_hex_digits() {
        let t = UNIX_EPOCH + Duration::new(1_800_000_000, 0x1234_abcd);
        assert_eq!(run_id(t), "gadk-20270115T080000Z-abcd");
        let t = UNIX_EPOCH + Duration::new(1_800_000_000, 7);
        assert_eq!(run_id(t), "gadk-20270115T080000Z-0007");
    }

    #[test]
    fn polls_slow_down_after_ten_minutes() {
        let p = predictor(ScriptedClient::json("{}"), Duration::from_secs(1));
        assert_eq!(p.poll_interval(Duration::ZERO), Duration::from_millis(1));
        assert_eq!(
            p.poll_interval(Duration::from_secs(599)),
            Duration::from_millis(1)
        );
        assert_eq!(
            p.poll_interval(Duration::from_secs(600)),
            Duration::from_millis(2)
        );
    }

    #[test]
    fn output_lines_give_text_errors_or_nothing() {
        assert_eq!(
            output_line(SUCCESS),
            Some((
                "r0".into(),
                BatchReply::Text(r#"{"pass": true, "score": 5, "rationale": "correct"}"#.into())
            ))
        );
        assert_eq!(
            output_line(PARTS),
            Some((
                "r1".into(),
                BatchReply::Text(r#"{"pass": false, "score": 2}"#.into())
            ))
        );
        assert_eq!(
            output_line(FAILED_LINE),
            Some((
                "r2".into(),
                BatchReply::Error("Bad Request: invalid argument".into())
            ))
        );
        assert_eq!(
            output_line(NO_CANDIDATES),
            Some(("r3".into(), BatchReply::Error("no candidates".into())))
        );
        assert_eq!(output_line("not json"), None);
        assert_eq!(output_line(r#"{"status":"","response":{}}"#), None);
        assert_eq!(output_line(""), None);
    }

    #[tokio::test]
    async fn uploads_creates_polls_and_collects() {
        let listing = format!(
            "{OBJECTS}?prefix=stage%2Fgadk-x%2Foutput%2Fprediction-model-2026-11-28T00%3A11%3A06.939882Z%2F"
        );
        let base = "stage/gadk-x/output/prediction-model-2026-11-28T00:11:06.939882Z";
        let http = service(
            &[
                "JOB_STATE_PENDING",
                "JOB_STATE_RUNNING",
                "JOB_STATE_SUCCEEDED",
            ],
            vec![
                (
                    listing.clone(),
                    json!({
                        "items": [{"name": format!("{base}/predictions.jsonl")}],
                        "nextPageToken": "t2",
                    })
                    .to_string(),
                ),
                (
                    format!("{listing}&pageToken=t2"),
                    json!({"items": [
                        {"name": format!("{base}/more/predictions-2.jsonl")},
                        {"name": format!("{base}/errors_stats.txt")},
                    ]})
                    .to_string(),
                ),
                (
                    format!(
                        "{OBJECTS}/{}?alt=media",
                        encode(&format!("{base}/predictions.jsonl"))
                    ),
                    format!("{SUCCESS}\nnot json\n{PARTS}\n"),
                ),
                (
                    format!(
                        "{OBJECTS}/{}?alt=media",
                        encode(&format!("{base}/more/predictions-2.jsonl"))
                    ),
                    format!("{FAILED_LINE}\n{NO_CANDIDATES}\n"),
                ),
            ],
        );
        let result = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap();
        assert_eq!(result.job, JOB);
        assert_eq!(
            result.replies,
            HashMap::from([
                (
                    "r0".into(),
                    BatchReply::Text(
                        r#"{"pass": true, "score": 5, "rationale": "correct"}"#.into()
                    )
                ),
                (
                    "r1".into(),
                    BatchReply::Text(r#"{"pass": false, "score": 2}"#.into())
                ),
                (
                    "r2".into(),
                    BatchReply::Error("Bad Request: invalid argument".into())
                ),
                ("r3".into(), BatchReply::Error("no candidates".into())),
            ])
        );

        let requests = http.requests();
        let upload = &requests[0];
        let id = Regex::new(
            r"^https://storage\.googleapis\.com/upload/storage/v1/b/bkt/o\?uploadType=media&name=stage%2F(gadk-\d{8}T\d{6}Z-[0-9a-f]{4})%2Finput\.jsonl$",
        )
        .unwrap()
        .captures(&upload.url)
        .unwrap_or_else(|| panic!("upload url {}", upload.url))[1]
            .to_owned();
        assert_eq!(upload.method, "POST");
        assert_eq!(upload.header("content-type"), "application/json");
        let sent: Vec<Value> = upload
            .body_text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            sent,
            [
                json!({"key": "r0", "request": {
                    "contents": [{"role": "user", "parts": [{"text": "Grade \"this\".\nReply with JSON."}]}],
                    "generationConfig": {"temperature": 0, "responseMimeType": "application/json"},
                }}),
                json!({"key": "r1", "request": {
                    "contents": [{"role": "user", "parts": [{"text": "Grade that."}]}],
                    "generationConfig": {"temperature": 0, "responseMimeType": "application/json"},
                }}),
            ]
        );
        assert!(upload.body_text().ends_with("}\n"));

        let create = &requests[1];
        assert_eq!(
            (create.method.as_str(), create.url.as_str()),
            (
                "POST",
                format!("{API}projects/p/locations/us/batchPredictionJobs").as_str()
            )
        );
        assert_eq!(
            create.json(),
            json!({
                "displayName": id,
                "model": "publishers/google/models/gemini-3.8-flash",
                "inputConfig": {
                    "instancesFormat": "jsonl",
                    "gcsSource": {"uris": [format!("gs://bkt/stage/{id}/input.jsonl")]},
                },
                "outputConfig": {
                    "predictionsFormat": "jsonl",
                    "gcsDestination": {"outputUriPrefix": format!("gs://bkt/stage/{id}/output/")},
                },
            })
        );
        let poll = ("GET".to_owned(), format!("{API}{JOB}"));
        assert_eq!(
            calls(&requests[2..]),
            [
                poll.clone(),
                poll.clone(),
                poll,
                ("GET".to_owned(), listing.clone()),
                ("GET".to_owned(), format!("{listing}&pageToken=t2")),
                (
                    "GET".to_owned(),
                    format!(
                        "{OBJECTS}/{}?alt=media",
                        encode(&format!("{base}/predictions.jsonl"))
                    )
                ),
                (
                    "GET".to_owned(),
                    format!(
                        "{OBJECTS}/{}?alt=media",
                        encode(&format!("{base}/more/predictions-2.jsonl"))
                    )
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_job_is_an_error_with_its_message() {
        let http = service(&["JOB_STATE_RUNNING", "JOB_STATE_FAILED"], Vec::new());
        let err = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("judge: batch job {JOB} JOB_STATE_FAILED: quota exceeded")
        );
        assert_eq!(http.hits(), 4);
    }

    #[tokio::test]
    async fn a_job_past_the_timeout_is_cancelled() {
        let http = service(&["JOB_STATE_RUNNING"], Vec::new());
        let err = predictor(http.clone(), Duration::from_millis(20))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("judge: batch job {JOB} did not finish within 20ms; cancelled")
        );
        let last = http.last_request();
        assert_eq!(
            (last.method.as_str(), last.url.as_str()),
            ("POST", format!("{API}{JOB}:cancel").as_str())
        );
    }

    /// A client answering the upload, the create, the polls with `polls` in order (the last
    /// repeating; `None` is a transport error), and every listing with no objects.
    fn polling(polls: Vec<Option<FakeResponse>>) -> Arc<ScriptedClient> {
        let polls = Mutex::new(polls.into_iter().collect::<std::collections::VecDeque<_>>());
        ScriptedClient::new(move |r| {
            let url = r.url.as_str();
            if url.starts_with(UPLOADS) {
                return FakeResponse::json("{}");
            }
            if url == format!("{API}projects/p/locations/us/batchPredictionJobs") {
                return FakeResponse::json(format!(r#"{{"name":"{JOB}"}}"#));
            }
            if url == format!("{API}{JOB}") {
                let mut polls = polls.lock().unwrap();
                let next = if polls.len() > 1 {
                    polls.pop_front().unwrap()
                } else {
                    polls.front().cloned().unwrap()
                };
                return next.unwrap_or_else(|| FakeResponse::reset("connection reset"));
            }
            FakeResponse::json("{}")
        })
    }

    fn job(state: &str) -> Option<FakeResponse> {
        Some(FakeResponse::json(
            json!({"name": JOB, "state": state, "outputInfo": {"gcsOutputDirectory": OUT_DIR}})
                .to_string(),
        ))
    }

    fn unavailable() -> Option<FakeResponse> {
        Some(FakeResponse::json_status(
            503,
            r#"{"error":{"code":503,"message":"backend unavailable"}}"#,
        ))
    }

    /// The number of status checks `http` received.
    fn polls(http: &ScriptedClient) -> usize {
        let url = format!("{API}{JOB}");
        http.requests().iter().filter(|r| r.url == url).count()
    }

    #[tokio::test]
    async fn a_transient_status_failure_is_retried() {
        let http = polling(vec![unavailable(), job("JOB_STATE_SUCCEEDED")]);
        let result = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap();
        assert_eq!(result.job, JOB);
        assert_eq!(polls(&http), 2);
    }

    #[tokio::test]
    async fn five_transient_failures_in_a_row_end_the_wait() {
        let http = polling(vec![unavailable()]);
        let err = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "judge: batch: HTTP 503: backend unavailable"
        );
        assert_eq!(polls(&http), 5);
    }

    #[tokio::test]
    async fn another_status_failure_ends_the_wait_at_once() {
        let http = polling(vec![Some(FakeResponse::json_status(
            403,
            r#"{"error":{"code":403,"message":"caller lacks aiplatform.batchPredictionJobs.get"}}"#,
        ))]);
        let err = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "judge: batch: HTTP 403: caller lacks aiplatform.batchPredictionJobs.get"
        );
        assert_eq!(polls(&http), 1);
    }

    #[tokio::test]
    async fn a_successful_check_resets_the_transient_count() {
        // Four transport errors, a check, four 503s: never five in a row.
        let mut seq = vec![None; 4];
        seq.push(job("JOB_STATE_RUNNING"));
        seq.extend(std::iter::repeat_with(unavailable).take(4));
        seq.push(job("JOB_STATE_SUCCEEDED"));
        let http = polling(seq);
        let result = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap();
        assert_eq!(result.job, JOB);
        assert_eq!(polls(&http), 10);
    }

    #[tokio::test]
    async fn helper_failures_are_batch_errors() {
        let http = ScriptedClient::always(FakeResponse::json_status(
            403,
            r#"{"error":{"code":403,"message":"caller lacks storage.objects.create"}}"#,
        ));
        let err = predictor(http.clone(), Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "judge: batch: HTTP 403: caller lacks storage.objects.create"
        );
        assert_eq!(http.hits(), 1);

        let http = ScriptedClient::new(|r| {
            if r.url.starts_with(UPLOADS) {
                FakeResponse::json("{}")
            } else {
                FakeResponse::json_status(
                    400,
                    r#"{"error":{"code":400,"message":"model not found"}}"#,
                )
            }
        });
        let err = predictor(http, Duration::from_secs(60))
            .predict(&lines())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "judge: batch: HTTP 400: model not found");
    }
}
