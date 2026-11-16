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

//! How a FirestoreStore reaches Firestore.

use std::sync::Arc;

use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;

pub(crate) const DEFAULT_ENDPOINT: &str = "https://firestore.googleapis.com";
pub(crate) const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/datastore",
    "https://www.googleapis.com/auth/cloud-platform",
];

/// Where and how to reach Firestore.
#[derive(Clone)]
pub struct FirestoreConfig {
    /// The Google Cloud project.
    pub project: String,
    /// The database ID (`(default)` by default).
    pub database: String,
    /// A document path (even number of segments, each `[A-Za-z0-9._-]+` and not `.` / `..`,
    /// e.g. `envs/prod`) under which the `prompts` collection lives; `None` or empty for the
    /// database root.
    ///
    /// Every registry refresh reads every prompt and every version under the root. With an
    /// empty root the versions query scans every `versions` collection in the database (other
    /// data's included, billed before it is filtered out), so prefer a dedicated non-empty root.
    pub root: Option<String>,
    /// `scheme://host[:port]`; `None` uses `http://{FIRESTORE_EMULATOR_HOST}` when set, else
    /// `https://firestore.googleapis.com`.
    ///
    /// Requests carry no credentials when `FIRESTORE_EMULATOR_HOST` is set or when the resolved
    /// endpoint is plain `http://`: only the emulator (or a local test server) is reachable that
    /// way, and it takes no auth, so the store never demands Application Default Credentials
    /// for it (the same rule the GCS artifact service applies to its emulator).
    pub endpoint: Option<String>,
    /// Credentials; `None` resolves Application Default Credentials (no auth for the emulator).
    pub credentials: Option<Credentials>,
    /// The HTTP client; `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Where `FIRESTORE_EMULATOR_HOST` and `GOOGLE_APPLICATION_CREDENTIALS` are read.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for FirestoreConfig {
    fn default() -> Self {
        Self {
            project: String::new(),
            database: "(default)".into(),
            root: None,
            endpoint: None,
            credentials: None,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for FirestoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreConfig")
            .field("project", &self.project)
            .field("database", &self.database)
            .field("root", &self.root)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .finish_non_exhaustive()
    }
}
