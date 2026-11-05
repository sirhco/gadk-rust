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

//! Port of ADK-Go `session/inmemory_test.go` `Test_inMemoryService`.

use gadk_session::testsuite::{SuiteOptions, run};
use gadk_session::{InMemorySessionService, SessionService};
use std::sync::Arc;

#[tokio::test]
async fn test_in_memory_service() {
    run(
        SuiteOptions {
            supports_user_provided_session_id: true,
            ..Default::default()
        },
        || async { Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService> },
    )
    .await;
}
