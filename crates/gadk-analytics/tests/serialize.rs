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

//! The JSON shape of the public types (`serde::Serialize`).

use gadk_analytics::{BigQueryTable, Service, SourceError, TimeRange, TraceId};
use serde_json::json;

#[test]
fn the_shared_types_serialize() {
    assert_eq!(
        serde_json::to_value(BigQueryTable::new("p", "d", "t")).unwrap(),
        json!({ "project": "p", "dataset": "d", "table": "t" })
    );
    assert_eq!(
        serde_json::to_value(TimeRange::Hours24).unwrap(),
        json!("Hours24")
    );
    assert_eq!(
        serde_json::to_value(TraceId::parse("0123456789ABCDEF0123456789ABCDEF").unwrap()).unwrap(),
        json!("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        serde_json::to_value(Service::Trace).unwrap(),
        json!("Trace")
    );
    assert_eq!(
        serde_json::to_value(SourceError::bigquery(Some(403), None, "Access Denied")).unwrap(),
        json!({ "BigQuery": { "status": 403, "reason": null, "detail": "Access Denied" } })
    );
    assert_eq!(
        serde_json::to_value(SourceError::NotFound).unwrap(),
        json!("NotFound")
    );
}
