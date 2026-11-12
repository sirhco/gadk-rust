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

//! Ports of ADK-Go's `telemetry` tests and of the contrib GCP detector's tests
//! (`go.opentelemetry.io/contrib/detectors/gcp` v1.47.0, ledger `docs/parity/7a.md`). A contrib
//! test file `<path>_test.go` is ported to `contrib_detectors_gcp_<path with _>.rs` (Go file
//! names with `-` keep it, loaded with `#[path]`).

#[path = "contrib_detectors_gcp_cloud-function.rs"]
mod contrib_detectors_gcp_cloud_function;
#[path = "contrib_detectors_gcp_cloud-run.rs"]
mod contrib_detectors_gcp_cloud_run;
mod contrib_detectors_gcp_detector;
mod contrib_detectors_gcp_internal_app_engine;
mod contrib_detectors_gcp_internal_bms;
mod contrib_detectors_gcp_internal_detector;
mod contrib_detectors_gcp_internal_detector_print;
mod contrib_detectors_gcp_internal_faas;
mod contrib_detectors_gcp_internal_gce;
mod contrib_detectors_gcp_internal_gke;
mod contrib_detectors_gcp_version;
mod telemetry_telemetry;
