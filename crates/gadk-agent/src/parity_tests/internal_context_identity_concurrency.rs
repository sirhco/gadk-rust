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

use crate::context::builder::test_support::session;
use crate::context::{ContextDelta, InvocationContext, ReadonlyContext};

// parity: adapted: Go races IdentityFromContext against EndInvocation/SetLiveSessionResumptionHandle; Rust readers on a multi-thread runtime race end_invocation and branch deltas (live handles are out of scope for 1b)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_identity_reads_race_the_invocation_mutators() {
    const READERS: usize = 32;
    const ROUNDS: usize = 200;
    let ic = InvocationContext::builder()
        .session(session("mapping-app", "mapping-user", "mapping-sid"))
        .build_invocation();
    let ro: ReadonlyContext = ic.as_readonly().clone(); // Go NewReadonlyContext(ic)
    let mut tasks = Vec::new();
    for _ in 0..READERS {
        let (ic, ro) = (ic.clone(), ro.clone());
        tasks.push(tokio::spawn(async move {
            for _ in 0..ROUNDS {
                assert_eq!(
                    (ic.user_id(), ic.app_name(), ic.session_id()),
                    ("mapping-user", "mapping-app", "mapping-sid"),
                    "InvocationContext"
                );
                assert_eq!(
                    (ro.user_id(), ro.app_name(), ro.session_id()),
                    ("mapping-user", "mapping-app", "mapping-sid"),
                    "ReadonlyContext"
                );
                tokio::task::yield_now().await;
            }
        }));
    }
    let writer = ic.clone();
    tasks.push(tokio::spawn(async move {
        for j in 0..ROUNDS {
            writer.end_invocation();
            let _ = writer.with_delta(ContextDelta {
                branch: Some(format!("b{j}")),
                ..Default::default()
            });
            tokio::task::yield_now().await;
        }
    }));
    for t in tasks {
        t.await.unwrap();
    }
}
