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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! R20: `gadk::agentregistry` (feature `agentregistry`, not on by default) is
//! `gadk_agentregistry`.

use gadk::agentregistry::{ApiError, Client, Config, Error};
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_agentregistry` item.
#[test]
fn facade_agentregistry_paths_are_gadk_agentregistry() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Client>(None, None::<&gadk_agentregistry::Client>);
    same::<Config>(None, None::<&gadk_agentregistry::Config>);
    same::<Error>(None, None::<&gadk_agentregistry::Error>);
    same::<ApiError>(None, None::<&gadk_agentregistry::ApiError>);
}

/// `Client::new` refuses a config without project and location with Go's text.
#[tokio::test]
async fn facade_agentregistry_client_requires_project_and_location() {
    let err = Client::new(Config::default()).await.err().unwrap();
    assert_eq!(
        err.to_string(),
        "agentregistry: project_id and location must be set"
    );
}
