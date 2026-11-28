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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Live test: write one Cloud Monitoring point.
//!
//! ```sh
//! GADK_EVAL_MONITOR_LIVE_PROJECT=<project> \
//!   cargo test -p gadk-eval-monitor --all-features live_metrics_write -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (Monitoring Metric Writer). It writes one `INT64` point
//! of `custom.googleapis.com/gadk/check/ping` with the label `spec = live-test`, which creates
//! that metric descriptor on the first run; delete it afterwards with
//! `projects.metricDescriptors.delete` if it is not wanted.

use std::collections::BTreeMap;

use chrono::Utc;
use gadk_eval_monitor::{CloudMetricsWriter, MetricPoint, MetricValue, MetricsWriter};

#[tokio::test]
#[ignore = "needs GADK_EVAL_MONITOR_LIVE_PROJECT and ADC"]
async fn live_metrics_write() {
    let Ok(project) = std::env::var("GADK_EVAL_MONITOR_LIVE_PROJECT") else {
        panic!("set GADK_EVAL_MONITOR_LIVE_PROJECT");
    };
    let writer = CloudMetricsWriter::new().unwrap();
    let labels = BTreeMap::from([("spec".to_owned(), "live-test".to_owned())]);
    let point = MetricPoint::new("check/ping", labels, MetricValue::Int(1), Utc::now());
    writer.write(&project, &[point]).await.unwrap();
    println!("wrote custom.googleapis.com/gadk/check/ping to {project}");
}
