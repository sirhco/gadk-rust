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

//! Port of ADK-Go tool/skilltoolset/skill/frontmatter_preload_test.go.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::skills::skill::{
    Frontmatter, ResourceReader, SkillError, SkillErrorKind, Source, with_frontmatter_preload,
};

/// Go `mockFrontmatterPreloadBaseSource`. Its state sits behind a mutex because the test
/// changes it after handing the mock to the wrapper. The methods Go leaves to the embedded nil
/// `Source` are never called; they fail.
#[derive(Default)]
struct MockFrontmatterPreloadBaseSource {
    state: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    frontmatters: Vec<Frontmatter>,
    list_frontmatters_err: Option<&'static str>,
    list_frontmatters_calls: usize,
    load_frontmatter_calls: usize,
}

impl MockFrontmatterPreloadBaseSource {
    fn new(frontmatters: Vec<Frontmatter>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(MockState {
                frontmatters,
                ..Default::default()
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().unwrap()
    }
}

fn not_mocked() -> SkillError {
    SkillError::new(SkillErrorKind::Other, "not implemented by the mock")
}

#[async_trait]
impl Source for MockFrontmatterPreloadBaseSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        let mut state = self.state();
        state.list_frontmatters_calls += 1;
        if let Some(msg) = state.list_frontmatters_err {
            return Err(SkillError::new(SkillErrorKind::Other, msg));
        }
        Ok(state.frontmatters.clone())
    }

    async fn list_resources(&self, _name: &str, _subpath: &str) -> Result<Vec<String>, SkillError> {
        Err(not_mocked())
    }

    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        let mut state = self.state();
        state.load_frontmatter_calls += 1;
        state
            .frontmatters
            .iter()
            .find(|fm| fm.name == name)
            .cloned()
            .ok_or_else(|| SkillError::new(SkillErrorKind::SkillNotFound, "skill not found"))
    }

    async fn load_instructions(&self, _name: &str) -> Result<String, SkillError> {
        Err(not_mocked())
    }

    async fn load_resource(
        &self,
        _name: &str,
        _resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        Err(not_mocked())
    }
}

fn named(names: &[&str]) -> Vec<Frontmatter> {
    names
        .iter()
        .map(|n| Frontmatter {
            name: (*n).into(),
            ..Default::default()
        })
        .collect()
}

/// Go `errors.Is(err, target)` for a plain error value: some link of the chain is it.
fn chain_has(err: &SkillError, text: &str) -> bool {
    let mut next: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = next {
        if e.to_string() == text {
            return true;
        }
        next = e.source();
    }
    false
}

#[tokio::test]
async fn test_with_frontmatter_preload_source_success() {
    // Test Init.
    let mock = MockFrontmatterPreloadBaseSource::new(named(&["B", "A"]));
    let (source, reload) = with_frontmatter_preload(mock.clone()).await.unwrap();
    assert_eq!(
        mock.state().list_frontmatters_calls,
        1,
        "mock.listFrontmattersCalls"
    );
    let got = source.list_frontmatters().await.unwrap();
    assert_eq!(got, named(&["A", "B"]), "ListFrontmatters mismatch");

    // Test Reload.
    mock.state().frontmatters = named(&["C"]);
    reload().await.unwrap();
    assert_eq!(
        mock.state().list_frontmatters_calls,
        2,
        "mock.listFrontmattersCalls after reload"
    );
    let got = source.list_frontmatters().await.unwrap();
    assert_eq!(got, named(&["C"]), "ListFrontmatters mismatch after reload");
}

#[tokio::test]
async fn test_with_frontmatter_preload_source_duplicate_skill() {
    let mock = MockFrontmatterPreloadBaseSource::new(named(&["A", "A"]));

    let err = with_frontmatter_preload(mock).await.err().unwrap();

    assert_eq!(
        err.kind(),
        SkillErrorKind::DuplicateSkill,
        "WithFrontmatterPreloadSource error = {err}"
    );
}

#[tokio::test]
async fn test_with_frontmatter_preload_source_reload_failure_does_not_invalidate_cache() {
    let mock = MockFrontmatterPreloadBaseSource::new(named(&["A"]));
    let (source, reload) = with_frontmatter_preload(mock.clone()).await.unwrap();
    mock.state().list_frontmatters_err = Some("reload error");

    let err = reload().await.unwrap_err();

    // If reload fails, the cache should still be valid.
    assert!(
        chain_has(&err, "reload error"),
        "Reload error = {err}, want reload error"
    );
    assert_eq!(err.to_string(), "preload: reload error");
    let got = source.list_frontmatters().await.unwrap();
    assert_eq!(got, named(&["A"]), "ListFrontmatters mismatch");
}

#[tokio::test]
async fn test_frontmatter_preload_source_list_frontmatters() {
    let mock = MockFrontmatterPreloadBaseSource::new(named(&["A"]));

    let (source, _reload) = with_frontmatter_preload(mock.clone()).await.unwrap();

    // Should be called once on init.
    assert_eq!(
        mock.state().list_frontmatters_calls,
        1,
        "mock.listFrontmattersCalls"
    );
    // Calls should be cached.
    let got1 = source.list_frontmatters().await.unwrap();
    let got2 = source.list_frontmatters().await.unwrap();
    // Should not change since list is cached.
    assert_eq!(
        mock.state().list_frontmatters_calls,
        1,
        "mock.listFrontmattersCalls"
    );
    assert_eq!(got1, got2, "ListFrontmatters results differ");
}

#[tokio::test]
async fn test_frontmatter_preload_source_load_frontmatter() {
    let want = Frontmatter {
        name: "A".into(),
        description: "desc A".into(),
        ..Default::default()
    };
    let mock = MockFrontmatterPreloadBaseSource::new(vec![want.clone()]);

    let (source, _reload) = with_frontmatter_preload(mock.clone()).await.unwrap();

    // Success case.
    let got = source.load_frontmatter("A").await.unwrap();
    assert_eq!(got, want, "LoadFrontmatter mismatch");

    // NotFound case.
    let err = source.load_frontmatter("B").await.unwrap_err();
    assert_eq!(
        err.kind(),
        SkillErrorKind::SkillNotFound,
        "LoadFrontmatter error = {err}"
    );

    // Check LoadFrontmatter was not called for existing or non-existing skill - they were
    // preloaded.
    assert_eq!(
        mock.state().load_frontmatter_calls,
        0,
        "mock.loadFrontmatterCalls (cached)"
    );
}
