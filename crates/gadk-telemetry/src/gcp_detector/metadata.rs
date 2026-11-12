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

//! The `cloud.google.com/go/compute/metadata` v0.10.0 subset the detector and ADC use:
//! `OnGCE`, `Get`, `ProjectID`, `InstanceName`, `Hostname`, `Zone` and `InstanceAttributeValue`,
//! with Go's error texts.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;
use std::sync::Arc;

use gadk_platform::env::EnvProvider;

/// Go `metadataHostEnv`.
pub const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";

/// Go `metadataIP`.
pub const METADATA_IP: &str = "169.254.169.254";

/// A metadata lookup failure; `Display` is Go's text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// Go `NotDefinedError` (HTTP 404): `metadata: GCE metadata "<suffix>" not defined`.
    #[error("metadata: GCE metadata \"{0}\" not defined")]
    NotDefined(String),
    /// Go `*metadata.Error` (any other non-200 status): ``compute: Received <code> `<body>` ``.
    #[error("compute: Received {code} `{message}`")]
    Status {
        /// The HTTP status code.
        code: u16,
        /// The response body.
        message: String,
    },
    /// The request failed before a response (Go's transport error).
    #[error("{0}")]
    Transport(String),
}

/// Go `*metadata.Client`: `GET <base>/computeMetadata/v1/<suffix>` with `Metadata-Flavor: Google`,
/// where `<base>` is `http://$GCE_METADATA_HOST` (from the injected environment), else
/// `http://169.254.169.254`, or the injected base URL.
#[derive(Clone)]
pub struct MetadataClient {
    client: reqwest::Client,
    env: Arc<dyn EnvProvider>,
    base_url: Option<String>,
}

impl MetadataClient {
    /// Go `metadata.NewClient(nil)` reading `GCE_METADATA_HOST` from `env`.
    pub fn new(env: Arc<dyn EnvProvider>) -> Self {
        todo!()
    }

    /// A client sending every request to `base_url` (e.g. `http://127.0.0.1:<port>`, a test
    /// server) instead of the metadata host; `env` still decides [`MetadataClient::on_gce`].
    pub fn with_base_url(env: Arc<dyn EnvProvider>, base_url: impl Into<String>) -> Self {
        todo!()
    }

    /// Go `OnGCE`: `true` when `GCE_METADATA_HOST` is set (non-empty), else the result of
    /// probing the metadata server raced with a DNS lookup of `metadata.google.internal.`.
    pub async fn on_gce(&self) -> bool {
        todo!()
    }

    /// Go `GetWithContext`: the untrimmed value at `suffix`, with Go's retries.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn get(&self, suffix: &str) -> Result<String, MetadataError> {
        todo!()
    }

    /// Go `ProjectIDWithContext`: `project/project-id`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn project_id(&self) -> Result<String, MetadataError> {
        todo!()
    }

    /// Go `InstanceNameWithContext`: `instance/name`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn instance_name(&self) -> Result<String, MetadataError> {
        todo!()
    }

    /// Go `HostnameWithContext`: `instance/hostname`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn hostname(&self) -> Result<String, MetadataError> {
        todo!()
    }

    /// Go `ZoneWithContext`: the last path segment of `instance/zone`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn zone(&self) -> Result<String, MetadataError> {
        todo!()
    }

    /// Go `InstanceAttributeValueWithContext`: `instance/attributes/<attr>`, untrimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn instance_attribute_value(&self, attr: &str) -> Result<String, MetadataError> {
        todo!()
    }
}

impl fmt::Debug for MetadataClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}
