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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/gce_test.go`.

use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{
    FAKE_INSTANCE_HOSTNAME, FAKE_INSTANCE_ID, FAKE_INSTANCE_MACHINE_TYPE, FAKE_INSTANCE_NAME,
    FAKE_REGION, FAKE_ZONE, failing_metadata, new_fake_metadata, new_test_detector,
};
use crate::gcp_detector::LocationType;
use crate::gcp_detector::internal::ManagedInstanceGroup;

#[tokio::test]
async fn test_gce_host_type() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gce_host_type().await.unwrap(), FAKE_INSTANCE_MACHINE_TYPE);
}

#[tokio::test]
async fn test_gce_host_type_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gce_host_type().await.is_err());
}

#[tokio::test]
async fn test_gce_host_id() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gce_host_id().await.unwrap(), FAKE_INSTANCE_ID);
}

#[tokio::test]
async fn test_gce_host_id_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gce_host_id().await.is_err());
}

#[tokio::test]
async fn test_gce_host_name() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gce_host_name().await.unwrap(), FAKE_INSTANCE_NAME);
}

#[tokio::test]
async fn test_gce_instance_name() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gce_instance_name().await.unwrap(), FAKE_INSTANCE_NAME);
}

#[tokio::test]
async fn test_gce_instance_hostname() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(
        d.gce_instance_hostname().await.unwrap(),
        FAKE_INSTANCE_HOSTNAME
    );
}

#[tokio::test]
async fn test_gce_instance_custom_hostname() {
    let d = new_test_detector(
        &new_fake_metadata(&[
            ("instance/name", "my-host-123"),
            ("instance/hostname", "custom-dns.fakevm.example"),
        ])
        .await,
        &[],
    );
    assert_eq!(
        d.gce_instance_hostname().await.unwrap(),
        "custom-dns.fakevm.example"
    );
}

#[tokio::test]
async fn test_gce_host_name_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gce_host_name().await.is_err());
}

#[tokio::test]
async fn test_gce_availability_zone_and_region() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    let (zone, region) = d.gce_availability_zone_and_region().await.unwrap();
    assert_eq!(zone, FAKE_ZONE);
    assert_eq!(region, FAKE_REGION);
}

#[tokio::test]
async fn test_gce_availability_zone_and_region_malformed_zone() {
    let d = new_test_detector(
        &new_fake_metadata(&[("instance/zone", "us-central1")]).await,
        &[],
    );
    let err = d.gce_availability_zone_and_region().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "zone was not in the expected format: country-region-zone.  Got us-central1"
    );
}

#[tokio::test]
async fn test_gce_availability_zone_and_region_no_zone() {
    let d = new_test_detector(&new_fake_metadata(&[("instance/zone", "")]).await, &[]);
    let err = d.gce_availability_zone_and_region().await.unwrap_err();
    assert_eq!(err.to_string(), "no zone detected from GCE metadata server");
}

#[tokio::test]
async fn test_gce_availability_zone_and_region_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gce_availability_zone_and_region().await.is_err());
}

#[tokio::test]
async fn test_gce_managed_instance_group() {
    let cases: [(&str, Option<ManagedInstanceGroup>); 5] = [
        (
            "projects/123456789012/zones/us-central1-f/instanceGroupManagers/igm-metadata",
            Some(ManagedInstanceGroup {
                name: "igm-metadata".to_owned(),
                location: "us-central1-f".to_owned(),
                location_type: LocationType::Zone,
            }),
        ),
        (
            "projects/123456789012/regions/us-central1-f/instanceGroupManagers/igm-metadata",
            Some(ManagedInstanceGroup {
                name: "igm-metadata".to_owned(),
                location: "us-central1-f".to_owned(),
                location_type: LocationType::Region,
            }),
        ),
        (
            "projects/123456789012/zones/us-central1-f/someOtherInstanceGroup/igm-metadata",
            Some(ManagedInstanceGroup::default()),
        ),
        ("", Some(ManagedInstanceGroup::default())),
        ("", None),
    ];
    for (created_by, want) in cases {
        let metadata = match &want {
            // Go sets `Err` on the transport: every request fails.
            None => failing_metadata().await,
            Some(_) if created_by.is_empty() => new_fake_metadata(&[]).await,
            Some(_) => new_fake_metadata(&[("instance/attributes/created-by", created_by)]).await,
        };
        let d = new_test_detector(&metadata, &[]);
        let got = d.gce_managed_instance_group().await;
        match want {
            None => assert!(got.is_err(), "{created_by:?}"),
            Some(want) => assert_eq!(got.unwrap(), want, "{created_by:?}"),
        }
    }
}
