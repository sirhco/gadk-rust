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

//! R16: `gadk::a2a` (feature `a2a`, not on by default) is `gadk_a2a`, with its `server` and
//! `remoteagent` halves and the a2a-rs crates (`a2a`, `a2a_server`, `a2a_client`) it re-exports.

use gadk::a2a::remoteagent::{A2AConfig, CardResolveOptions, new_a2a};
use gadk::a2a::server::{Executor, ExecutorConfig, IDENTITY_HEADER, OutputMode};
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_a2a` item.
#[test]
fn facade_a2a_paths_are_gadk_a2a() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Executor>(None, None::<&gadk_a2a::server::Executor>);
    same::<ExecutorConfig>(None, None::<&gadk_a2a::server::ExecutorConfig>);
    same::<OutputMode>(None, None::<&gadk_a2a::server::OutputMode>);
    same::<A2AConfig>(None, None::<&gadk_a2a::remoteagent::A2AConfig>);
    same::<CardResolveOptions>(None, None::<&gadk_a2a::remoteagent::CardResolveOptions>);
    same::<gadk::a2a::Error>(None, None::<&gadk_a2a::Error>);
    same::<gadk::a2a::a2a::AgentCard>(None, None::<&gadk_a2a::a2a::AgentCard>);
    same::<dyn gadk::a2a::a2a_server::AgentExecutor>(
        None,
        None::<&dyn gadk_a2a::a2a_server::AgentExecutor>,
    );
    same::<gadk::a2a::a2a_client::A2AClientFactory>(
        None,
        None::<&gadk_a2a::a2a_client::A2AClientFactory>,
    );
    assert_eq!(IDENTITY_HEADER, "x-gadk-a2a-user");
    assert_eq!(
        gadk::a2a::convert::ADK_EXTENSION_URI,
        "https://google.github.io/adk-docs/a2a/a2a-extension/"
    );
}

/// The façade builds a server executor and a remote agent.
#[test]
fn facade_a2a_builds_an_executor_and_a_remote_agent() {
    let _executor = Executor::new(ExecutorConfig::default());
    let err = new_a2a(A2AConfig {
        name: "remote".into(),
        ..Default::default()
    })
    .err()
    .expect("a remote agent without a card is refused");
    assert_eq!(
        err.to_string(),
        "either AgentCard or AgentCardProvider must be provided"
    );
    let agent = new_a2a(A2AConfig {
        name: "remote".into(),
        agent_card_provider: Some(gadk::a2a::remoteagent::new_agent_card_provider(
            "http://127.0.0.1:1",
            CardResolveOptions::default(),
        )),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(agent.name(), "remote");
}

/// With the façade's `server` feature too, `gadk::a2a::server::caller_user` reads the user that
/// `gadk::server::authn` stored, so the identity layer can sit behind the REST server's authn.
#[cfg(feature = "server")]
#[test]
fn facade_a2a_with_server_names_caller_user() {
    let extract: fn(&axum::http::Extensions) -> Option<String> = gadk::a2a::server::caller_user;
    let mut ext = axum::http::Extensions::new();
    assert_eq!(extract(&ext), None);
    ext.insert(gadk::server::authn::Caller {
        user_id: "alice".into(),
        claims: None,
    });
    assert_eq!(extract(&ext), Some("alice".to_owned()));
}
