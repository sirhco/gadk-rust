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

//! Go `google.FindDefaultCredentials` (golang.org/x/oauth2 v0.37.0 `google/default.go`) over
//! gadk's Application Default Credentials ([`gadk_model::gcp_auth::adc_credentials`]), with the
//! project ID Go reports alongside.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_platform::env::EnvProvider;

use crate::config::GoogleCredentials;

/// The scope Go's `configure` requests ADC with.
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Go `google.FindDefaultCredentials(ctx, scopes...)`: the `GOOGLE_APPLICATION_CREDENTIALS`
/// file, else gcloud's well-known file under `$HOME` (`%APPDATA%` on Windows), else — on GCE
/// ([`crate::gcp_detector::metadata::MetadataClient::on_gce`]) — the metadata server's
/// credentials and project. `env` is the injected environment. The error is Go's text
/// (callers wrap it).
pub(crate) async fn find_default_credentials(
    env: &Arc<dyn EnvProvider>,
    scopes: &[&str],
) -> Result<GoogleCredentials, String> {
    todo!()
}
