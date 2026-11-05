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

//! Port of ADK-Go `model/registry_test.go`.

use gadk_model::*;
use std::sync::Arc;

struct Named(String);

impl Llm for Named {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        &self.0
    }

    fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> futures::stream::BoxStream<'static, Result<LlmResponse, ModelError>> {
        Box::pin(futures::stream::empty())
    }
}

fn factory(tag: &'static str) -> Factory {
    Arc::new(move |name: String| {
        Box::pin(async move { Ok(Arc::new(Named(format!("{tag}:{name}"))) as Arc<dyn Llm>) })
    })
}

fn failing_factory() -> Factory {
    Arc::new(|_name: String| Box::pin(async move { Err(ModelError::Config("boom".to_string())) }))
}

#[tokio::test]
async fn test_new_llm_single_match() {
    let r = Registry::new();
    r.register("^gemini-", factory("g")).unwrap();
    assert_eq!(
        r.new_llm("gemini-2.5-flash").await.unwrap().name(),
        "g:gemini-2.5-flash"
    );
}

#[test]
fn test_register_duplicate_pattern_panics() {
    let r = Registry::new();
    r.register("^a$", factory("a")).unwrap();
    let err = r.register("^a$", factory("b"));
    assert!(matches!(err, Err(RegistryError::DuplicatePattern(p)) if p == "^a$"));
}

#[tokio::test]
async fn test_new_llm_no_match() {
    let r = Registry::new();
    r.register("^gemini-", factory("g")).unwrap();
    let err = r.new_llm("claude-3").await;
    assert!(matches!(err, Err(RegistryError::NoMatch(n)) if n == "claude-3"));
    assert!(matches!(
        Registry::new().new_llm("x").await,
        Err(RegistryError::NoMatch(_))
    ));
}

#[tokio::test]
async fn test_new_llm_multiple_matches_error() {
    let r = Registry::new();
    r.register("^gem", factory("a")).unwrap();
    r.register("ini", factory("b")).unwrap();
    let err = r.new_llm("gemini").await;
    assert!(matches!(
        &err,
        Err(RegistryError::Ambiguous { name, patterns })
            if name == "gemini" && patterns == &["^gem".to_string(), "ini".to_string()]
    ));
    let msg = err.err().unwrap().to_string();
    assert!(
        msg.contains("matches multiple registered LLM patterns"),
        "{msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_registry_concurrent_access() {
    let r = Arc::new(Registry::new());
    let mut hs = Vec::new();
    for i in 0..16 {
        let r = r.clone();
        hs.push(tokio::spawn(async move {
            r.register(&format!("^m{i}$"), factory("c")).unwrap();
            let llm = r.new_llm(&format!("m{i}")).await.unwrap();
            assert_eq!(llm.name(), format!("c:m{i}"));
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
}

#[test]
fn invalid_pattern_errors() {
    assert!(matches!(
        Registry::new().register("(", factory("x")),
        Err(RegistryError::InvalidPattern(p, _)) if p == "("
    ));
}

#[tokio::test]
async fn factory_error_is_propagated() {
    let r = Registry::new();
    r.register("^bad$", failing_factory()).unwrap();
    assert!(matches!(
        r.new_llm("bad").await,
        Err(RegistryError::Factory(ModelError::Config(m))) if m == "boom"
    ));
}

#[tokio::test]
async fn global_registry_is_shared() {
    assert!(std::ptr::eq(Registry::global(), Registry::global()));
    Registry::global()
        .register("^global-registry-test-model$", factory("glob"))
        .unwrap();
    let llm = Registry::global()
        .new_llm("global-registry-test-model")
        .await
        .unwrap();
    assert_eq!(llm.name(), "glob:global-registry-test-model");
}
