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

//! Port of `internal/gke.go`: Kubernetes Engine, including the Compute API host type.

use gadk_model::gcp_auth::credential_headers;
use http::HeaderMap;
use serde_json::Value;

use super::detector::{Detector, DetectorError};
use crate::credentials::find_default_credentials;
use crate::gcp_detector::metadata::transport_text;

/// Go `k8sServiceHostEnv`.
const K8S_SERVICE_HOST_ENV: &str = "KUBERNETES_SERVICE_HOST";
/// Go `clusterNameMetadataAttr`.
const CLUSTER_NAME_METADATA_ATTR: &str = "cluster-name";
/// Go `clusterLocationMetadataAttr`.
const CLUSTER_LOCATION_METADATA_ATTR: &str = "cluster-location";

/// Go `computeInstancesGetScope`.
pub(super) const COMPUTE_INSTANCES_GET_SCOPE: &str =
    "https://www.googleapis.com/auth/compute.readonly";

/// Go `defaultComputeBaseURL`.
pub(super) const DEFAULT_COMPUTE_BASE_URL: &str = "https://compute.googleapis.com/compute/v1";

/// Go `LocationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationType {
    /// Go `UndefinedLocation`.
    #[default]
    UndefinedLocation,
    /// Go `Zone`.
    Zone,
    /// Go `Region`.
    Region,
}

impl Detector {
    /// Go `onGKE`: `KUBERNETES_SERVICE_HOST` is set and the `cluster-location` attribute is
    /// readable.
    pub(super) async fn on_gke(&self) -> bool {
        // Check if we are on k8s first.
        if self.os.var(K8S_SERVICE_HOST_ENV).is_none() {
            return false;
        }
        // If we are on k8s, make sure that we are actually on GKE, and not a different managed
        // k8s platform.
        self.metadata
            .instance_attribute_value(CLUSTER_LOCATION_METADATA_ATTR)
            .await
            .is_ok()
    }

    /// Go `GKEHostID`: GCE's host ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gke_host_id(&self) -> Result<String, DetectorError> {
        self.gce_host_id().await
    }

    /// Go `GKEClusterName`: the `cluster-name` attribute.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gke_cluster_name(&self) -> Result<String, DetectorError> {
        Ok(self
            .metadata
            .instance_attribute_value(CLUSTER_NAME_METADATA_ATTR)
            .await?)
    }

    /// Go `GKEHostType`: the instance's machine type from the Compute API `instances.get`.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including Go's `compute instances.get returned <status>`.
    pub async fn gke_host_type(&self) -> Result<String, DetectorError> {
        let project_id = self.project_id().await?;
        let zone = self.metadata.zone().await?;
        let instance_name = self.metadata.instance_name().await?;
        let machine_type = self
            .compute_instance_machine_type(&project_id, last_path_segment(&zone), &instance_name)
            .await?;
        Ok(last_path_segment(&machine_type).to_owned())
    }

    /// Go `computeInstanceMachineType`.
    async fn compute_instance_machine_type(
        &self,
        project_id: &str,
        zone: &str,
        instance_name: &str,
    ) -> Result<String, DetectorError> {
        let base_url = if self.compute_base_url.is_empty() {
            DEFAULT_COMPUTE_BASE_URL
        } else {
            &self.compute_base_url
        };
        let url = format!(
            "{}/projects/{}/zones/{}/instances/{}",
            base_url.trim_end_matches('/'),
            path_escape(project_id),
            path_escape(zone),
            path_escape(instance_name),
        );
        let (client, headers) = self.compute_http_client(&url).await?;
        let res = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| DetectorError::Other(transport_text(&url, &e)))?;
        let status = res.status();
        if !status.is_success() {
            return Err(DetectorError::Other(format!(
                "compute instances.get returned {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or_default()
            )));
        }
        let body = res
            .bytes()
            .await
            .map_err(|e| DetectorError::Other(transport_text(&url, &e)))?;
        let machine_type = decode_machine_type(&body).map_err(DetectorError::Other)?;
        if machine_type.is_empty() {
            return Err(DetectorError::Other(
                "compute instances.get response missing machineType".to_owned(),
            ));
        }
        Ok(machine_type)
    }

    /// Go `computeHTTPClient`: the injected client as is, else Go's
    /// `google.DefaultClient(ctx, computeInstancesGetScope)` — gadk's ADC
    /// ([`find_default_credentials`]) whose headers authenticate this request.
    async fn compute_http_client(
        &self,
        url: &str,
    ) -> Result<(reqwest::Client, HeaderMap), DetectorError> {
        if let Some(client) = &self.http_client {
            return Ok((client.clone(), HeaderMap::new()));
        }
        let creds = find_default_credentials(&self.os, &[COMPUTE_INSTANCES_GET_SCOPE])
            .await
            .map_err(DetectorError::Other)?;
        let headers = match &creds.credentials {
            Some(credentials) => credential_headers(credentials)
                .await
                .map_err(|e| DetectorError::Other(format!("Get \"{url}\": {e}")))?,
            None => HeaderMap::new(),
        };
        Ok((reqwest::Client::new(), headers))
    }

    /// Go `GKEAvailabilityZoneOrRegion`: the `cluster-location` attribute and whether it is a
    /// zone or a region.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including Go's `unrecognized format for cluster location: <loc>`.
    pub async fn gke_availability_zone_or_region(
        &self,
    ) -> Result<(String, LocationType), DetectorError> {
        let cluster_location = self
            .metadata
            .instance_attribute_value(CLUSTER_LOCATION_METADATA_ATTR)
            .await?;
        match cluster_location.matches('-').count() {
            1 => Ok((cluster_location, LocationType::Region)),
            2 => Ok((cluster_location, LocationType::Zone)),
            _ => Err(DetectorError::Other(format!(
                "unrecognized format for cluster location: {cluster_location}"
            ))),
        }
    }
}

/// Go `lastPathSegment`.
pub(super) fn last_path_segment(s: &str) -> &str {
    let s = s.trim_end_matches('/');
    match s.rfind('/') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

/// Go `url.PathEscape`: every byte but `A-Z a-z 0-9 - _ . ~ $ & + : = @` as uppercase `%XX`.
fn path_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~$&+:=@".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Go `json.NewDecoder(res.Body).Decode(&struct{ MachineType string \`json:"machineType"\` })`:
/// the first JSON value's `machineType` (key matched case-insensitively, later keys win, `null`
/// skipped), with Go's type-mismatch and EOF texts. Syntax errors carry serde_json's text.
///
/// Divergence (ledger 7a): JSON syntax-error texts are serde_json's, not Go's `encoding/json`
/// `*json.SyntaxError` texts.
fn decode_machine_type(body: &[u8]) -> Result<String, String> {
    let value = match serde_json::Deserializer::from_slice(body)
        .into_iter::<Value>()
        .next()
    {
        Some(Ok(value)) => value,
        Some(Err(e)) if e.is_eof() => return Err("unexpected EOF".to_owned()),
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("EOF".to_owned()),
    };
    match value {
        Value::Object(map) => {
            let mut machine_type = String::new();
            for (key, v) in &map {
                if !key.eq_ignore_ascii_case("machineType") {
                    continue;
                }
                match v {
                    Value::String(s) => machine_type.clone_from(s),
                    Value::Null => {}
                    other => {
                        return Err(format!(
                            "json: cannot unmarshal {} into Go struct field .machineType of type string",
                            json_kind(other)
                        ));
                    }
                }
            }
            Ok(machine_type)
        }
        Value::Null => Ok(String::new()),
        other => Err(format!(
            "json: cannot unmarshal {} into Go value of type struct {{ MachineType string \"json:\\\"machineType\\\"\" }}",
            json_kind(&other)
        )),
    }
}

/// Go's `UnmarshalTypeError.Value` names.
fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_escape_matches_gos() {
        // Go: url.PathEscape over ASCII keeps exactly these bytes.
        let kept: String = (0u8..128)
            .map(char::from)
            .filter(|c| path_escape(&c.to_string()) == c.to_string())
            .collect();
        assert_eq!(
            kept,
            "$&+-.0123456789:=@ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~"
        );
        assert_eq!(path_escape("a b/ü%"), "a%20b%2F%C3%BC%25");
    }

    #[test]
    fn last_path_segment_matches_gos() {
        assert_eq!(
            last_path_segment("projects/1/zones/us-central1-a"),
            "us-central1-a"
        );
        assert_eq!(last_path_segment("a/b/"), "b");
        assert_eq!(last_path_segment("plain"), "plain");
        assert_eq!(last_path_segment(""), "");
    }

    #[test]
    fn decode_machine_type_matches_gos_decoder() {
        let cases: &[(&str, Result<&str, &str>)] = &[
            (r#"{"machineType":"a"}"#, Ok("a")),
            (r#"{"MACHINETYPE":"a"}"#, Ok("a")),
            (r#"{"machineType":"a"} trailing"#, Ok("a")),
            (r#"{"machineType":null}"#, Ok("")),
            ("null", Ok("")),
            ("", Err("EOF")),
            (r#"{"machineType":"#, Err("unexpected EOF")),
            (
                r#"{"machineType":5}"#,
                Err(
                    "json: cannot unmarshal number into Go struct field .machineType of type string",
                ),
            ),
            (
                r#"{"machineType":{"a":1}}"#,
                Err(
                    "json: cannot unmarshal object into Go struct field .machineType of type string",
                ),
            ),
            (
                "[1]",
                Err(
                    r#"json: cannot unmarshal array into Go value of type struct { MachineType string "json:\"machineType\"" }"#,
                ),
            ),
            (
                "true",
                Err(
                    r#"json: cannot unmarshal bool into Go value of type struct { MachineType string "json:\"machineType\"" }"#,
                ),
            ),
        ];
        for (body, want) in cases {
            let got = decode_machine_type(body.as_bytes());
            assert_eq!(
                got.as_deref().map_err(String::as_str),
                *want,
                "body {body:?}"
            );
        }
    }
}
