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

//! Default-credentials lookup over gadk's Application Default Credentials
//! ([`gadk_model::gcp_auth::adc_credentials`]), with the project ID found alongside.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_model::gcp_auth::adc_credentials;
use gadk_platform::env::EnvProvider;
use serde_json::Value;

use crate::config::GoogleCredentials;
use crate::gcp_detector::metadata::MetadataClient;

/// The scope telemetry setup requests ADC with.
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The environment variable that names the credentials file.
const CREDENTIALS_ENV: &str = "GOOGLE_APPLICATION_CREDENTIALS";

/// Where to find instructions for setting up ADC.
const ADC_SETUP_URL: &str = "https://cloud.google.com/docs/authentication/external/set-up-adc";

/// Finds the default credentials: the `GOOGLE_APPLICATION_CREDENTIALS`
/// file, else gcloud's well-known file under `$HOME` (`%APPDATA%` on Windows), else — on GCE
/// ([`crate::gcp_detector::metadata::MetadataClient::on_gce`]) — the metadata server's
/// credentials and project. `env` is the injected environment. The error text is
/// byte-identical to ADK's (callers wrap it).
// The file read and parse error details are gadk's (Rust's I/O and serde_json texts,
// google-cloud-auth's builder errors), inside the wrapping texts ADK uses.
pub(crate) async fn find_default_credentials(
    env: &Arc<dyn EnvProvider>,
    scopes: &[&str],
) -> Result<GoogleCredentials, String> {
    // First, try the environment variable.
    if let Some(filename) = env.var(CREDENTIALS_ENV).filter(|f| !f.is_empty()) {
        return credentials_from_file(Path::new(&filename), scopes).map_err(|e| {
            format!("google: error getting credentials using {CREDENTIALS_ENV} environment variable: {e}")
        });
    }

    // Second, try a well-known file.
    if let Some(filename) = well_known_file(env.as_ref()).filter(|f| std::fs::read(f).is_ok()) {
        return credentials_from_file(&filename, scopes);
    }

    // Third, if we're on Google Compute Engine, use the metadata server.
    let metadata = MetadataClient::new(Arc::clone(env));
    if metadata.on_gce().await {
        let project_id = metadata.project_id().await.unwrap_or_default();
        let credentials = adc_credentials(None, scopes).map_err(|e| e.to_string())?;
        return Ok(GoogleCredentials {
            credentials: Some(credentials),
            project_id,
        });
    }

    // None are found; return helpful error.
    Err(format!(
        "google: could not find default credentials. See {ADC_SETUP_URL} for more information"
    ))
}

/// Credentials from a JSON file and its `project_id` (empty when absent).
fn credentials_from_file(path: &Path, scopes: &[&str]) -> Result<GoogleCredentials, String> {
    let credentials =
        adc_credentials(Some(&path.to_string_lossy()), scopes).map_err(|e| e.to_string())?;
    let project_id = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("project_id")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(GoogleCredentials {
        credentials: Some(credentials),
        project_id,
    })
}

/// The well-known credentials file: `%APPDATA%\gcloud\…` on Windows, `$HOME/.config/gcloud/…`
/// elsewhere.
// Without `HOME` in the injected environment there is no well-known file (no fallback to the
// current user's home directory).
fn well_known_file(env: &dyn EnvProvider) -> Option<PathBuf> {
    const FILE: &str = "application_default_credentials.json";
    if cfg!(windows) {
        let appdata = env.var("APPDATA")?;
        return Some(Path::new(&appdata).join("gcloud").join(FILE));
    }
    let home = env.var("HOME").filter(|h| !h.is_empty())?;
    Some(Path::new(&home).join(".config").join("gcloud").join(FILE))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::test_support::env;

    const SCOPES: &[&str] = &[CLOUD_PLATFORM_SCOPE];

    fn user_credentials(project_id: Option<&str>) -> String {
        let project = project_id
            .map(|p| format!(r#","project_id":"{p}""#))
            .unwrap_or_default();
        format!(
            r#"{{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r"{project}}}"#
        )
    }

    #[tokio::test]
    async fn a_missing_credentials_file_is_gos_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let missing = missing.to_string_lossy().into_owned();
        let err = find_default_credentials(
            &env(&[("GOOGLE_APPLICATION_CREDENTIALS", &missing)]),
            SCOPES,
        )
        .await
        .unwrap_err();
        let io = std::fs::read(&missing).unwrap_err();
        assert_eq!(
            err,
            format!(
                "google: error getting credentials using GOOGLE_APPLICATION_CREDENTIALS environment variable: {io}"
            )
        );
    }

    #[tokio::test]
    async fn the_credentials_file_fills_the_project_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("creds.json");
        std::fs::write(&file, user_credentials(Some("file-project"))).unwrap();
        let file = file.to_string_lossy().into_owned();
        let creds =
            find_default_credentials(&env(&[("GOOGLE_APPLICATION_CREDENTIALS", &file)]), SCOPES)
                .await
                .unwrap();
        assert!(creds.credentials.is_some());
        assert_eq!(creds.project_id, "file-project");
    }

    #[tokio::test]
    async fn a_credentials_file_without_project_id_has_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("creds.json");
        std::fs::write(&file, user_credentials(None)).unwrap();
        let file = file.to_string_lossy().into_owned();
        let creds =
            find_default_credentials(&env(&[("GOOGLE_APPLICATION_CREDENTIALS", &file)]), SCOPES)
                .await
                .unwrap();
        assert!(creds.credentials.is_some());
        assert_eq!(creds.project_id, "");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn gclouds_well_known_file_is_read_under_home() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".config").join("gcloud");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("application_default_credentials.json"),
            user_credentials(Some("gcloud-project")),
        )
        .unwrap();
        let home = home.path().to_string_lossy().into_owned();
        let creds = find_default_credentials(&env(&[("HOME", &home)]), SCOPES)
            .await
            .unwrap();
        assert_eq!(creds.project_id, "gcloud-project");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_bad_well_known_file_is_its_parse_error() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".config").join("gcloud");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("application_default_credentials.json");
        std::fs::write(&file, "not json").unwrap();
        let want = gadk_model::gcp_auth::adc_credentials(Some(&file.to_string_lossy()), SCOPES)
            .unwrap_err()
            .to_string();
        let home = home.path().to_string_lossy().into_owned();
        let err = find_default_credentials(&env(&[("HOME", &home)]), SCOPES)
            .await
            .unwrap_err();
        assert_eq!(err, want);
    }
}
