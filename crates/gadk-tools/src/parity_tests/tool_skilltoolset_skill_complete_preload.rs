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

//! Port of ADK-Go tool/skilltoolset/skill/complete_preload_test.go.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::skills::skill::{
    Frontmatter, MAX_RESOURCE_BYTES, ResourceReader, SkillError, SkillErrorKind, Source,
    with_complete_preload,
};

/// Go's `testError := fmt.Errorf("test error")`.
const TEST_ERR: &str = "test error";

type Resources = HashMap<String, BTreeMap<String, Vec<u8>>>;

/// Go `mockCompletePreloadBaseSource`. Its state sits behind a mutex because the tests change
/// it after handing the mock to the wrapper. `load_frontmatter`, which Go leaves to the
/// embedded nil `Source`, is never called; it fails.
#[derive(Default)]
struct MockCompletePreloadBaseSource {
    state: Mutex<MockState>,
}

#[derive(Default, Clone)]
struct MockState {
    frontmatters: Vec<Frontmatter>,
    instructions: HashMap<String, String>,
    resources: Resources,
    list_frontmatters_calls: usize,
    list_resources_calls: usize,
    load_instructions_calls: usize,
    load_resource_calls: usize,
    list_frontmatters_err: Option<&'static str>,
    load_instructions_err: Option<&'static str>,
    list_resources_err: Option<&'static str>,
    load_resource_err: Option<&'static str>,
}

impl MockCompletePreloadBaseSource {
    fn new(state: MockState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
        })
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap()
    }

    /// Go `verifyCallCount`.
    fn verify_call_count(
        &self,
        list_frontmatters_calls: usize,
        list_resources_calls: usize,
        load_instructions_calls: usize,
        load_resource_calls: usize,
    ) -> Result<(), String> {
        let s = self.state();
        if s.list_frontmatters_calls != list_frontmatters_calls {
            return Err(format!(
                "listFrontmattersCalls: {}, want {list_frontmatters_calls}",
                s.list_frontmatters_calls
            ));
        }
        if s.list_resources_calls != list_resources_calls {
            return Err(format!(
                "listResourcesCalls: {}, want {list_resources_calls}",
                s.list_resources_calls
            ));
        }
        if s.load_instructions_calls != load_instructions_calls {
            return Err(format!(
                "loadInstructionsCalls: {}, want {load_instructions_calls}",
                s.load_instructions_calls
            ));
        }
        if s.load_resource_calls != load_resource_calls {
            return Err(format!(
                "loadResourceCalls: {}, want {load_resource_calls}",
                s.load_resource_calls
            ));
        }
        Ok(())
    }
}

fn other(msg: &str) -> SkillError {
    SkillError::new(SkillErrorKind::Other, msg)
}

#[async_trait]
impl Source for MockCompletePreloadBaseSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        let mut s = self.state();
        s.list_frontmatters_calls += 1;
        if let Some(msg) = s.list_frontmatters_err {
            return Err(other(msg));
        }
        Ok(s.frontmatters.clone())
    }

    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        let mut s = self.state();
        s.list_resources_calls += 1;
        if let Some(msg) = s.list_resources_err {
            return Err(other(msg));
        }
        let Some(res) = s.resources.get(name) else {
            return Err(SkillError::new(
                SkillErrorKind::SkillNotFound,
                "skill not found",
            ));
        };
        let paths: Vec<String> = res.keys().cloned().collect();
        if subpath != "." {
            return Err(other(&format!(
                "unexpected subpath {subpath:?}: mock only supports '.' as required by complete preload"
            )));
        }
        Ok(paths)
    }

    async fn load_frontmatter(&self, _name: &str) -> Result<Frontmatter, SkillError> {
        Err(other("not implemented by the mock"))
    }

    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        let mut s = self.state();
        s.load_instructions_calls += 1;
        if let Some(msg) = s.load_instructions_err {
            return Err(other(msg));
        }
        s.instructions
            .get(name)
            .cloned()
            .ok_or_else(|| SkillError::new(SkillErrorKind::SkillNotFound, "skill not found"))
    }

    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        let mut s = self.state();
        s.load_resource_calls += 1;
        if let Some(msg) = s.load_resource_err {
            return Err(other(msg));
        }
        let Some(res) = s.resources.get(name) else {
            return Err(SkillError::new(
                SkillErrorKind::SkillNotFound,
                "skill not found",
            ));
        };
        let Some(data) = res.get(resource_path) else {
            return Err(SkillError::new(
                SkillErrorKind::ResourceNotFound,
                "resource not found",
            ));
        };
        Ok(Box::pin(std::io::Cursor::new(data.clone())))
    }
}

/// Go `collectedSourceData`.
#[derive(Debug, Default, PartialEq)]
struct CollectedSourceData {
    frontmatters: Vec<Frontmatter>,
    instructions: BTreeMap<String, String>,
    resources: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

/// Go `collectSourceData`.
async fn collect_source_data(source: &dyn Source) -> Result<CollectedSourceData, String> {
    let mut collected = CollectedSourceData::default();
    let frontmatters = source
        .list_frontmatters()
        .await
        .map_err(|e| e.to_string())?;
    collected.frontmatters = frontmatters.clone();
    for fm in &frontmatters {
        let loaded = source
            .load_frontmatter(&fm.name)
            .await
            .map_err(|e| e.to_string())?;
        if &loaded != fm {
            return Err(format!(
                "frontmatter mismatch for {:?}: {loaded:?}",
                fm.name
            ));
        }
        let instructions = source
            .load_instructions(&fm.name)
            .await
            .map_err(|e| e.to_string())?;
        collected.instructions.insert(fm.name.clone(), instructions);
        let resources = source
            .list_resources(&fm.name, ".")
            .await
            .map_err(|e| e.to_string())?;
        if resources.is_empty() {
            continue;
        }
        let entry = collected.resources.entry(fm.name.clone()).or_default();
        for resource_path in resources {
            let mut reader = source
                .load_resource(&fm.name, &resource_path)
                .await
                .map_err(|e| format!("loading resource {:?}:{resource_path}: {e}", fm.name))?;
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .await
                .map_err(|e| format!("reading resource {:?}:{resource_path}: {e}", fm.name))?;
            entry.insert(resource_path, data);
        }
    }
    Ok(collected)
}

fn fm(name: &str, description: &str) -> Frontmatter {
    Frontmatter {
        name: name.into(),
        description: description.into(),
        ..Default::default()
    }
}

fn strings(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn files(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_vec()))
        .collect()
}

fn resources(skills: Vec<(&str, BTreeMap<String, Vec<u8>>)>) -> Resources {
    skills
        .into_iter()
        .map(|(name, files)| (name.to_string(), files))
        .collect()
}

fn ordered(
    skills: Vec<(&str, BTreeMap<String, Vec<u8>>)>,
) -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
    skills
        .into_iter()
        .map(|(name, files)| (name.to_string(), files))
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
async fn test_with_complete_preload_source() {
    // Test initial load.
    let mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "desc1"), fm("skill2", "desc2")],
        instructions: strings(&[("skill1", "instructions1"), ("skill2", "instructions2")]),
        resources: resources(vec![
            (
                "skill1",
                files(&[
                    ("assets/image.png", b"image-data"),
                    ("references/doc.txt", b"doc-data"),
                    ("scripts/script.py", b"script-data"),
                ]),
            ),
            ("skill2", files(&[])),
        ]),
        ..Default::default()
    });

    let (source, reload) = with_complete_preload(mock.clone()).await.unwrap();

    mock.verify_call_count(1, 2, 2, 3).unwrap();
    let collected = collect_source_data(source.as_ref()).await.unwrap();
    if let Err(e) = mock.verify_call_count(1, 2, 2, 3) {
        panic!("the base mock should not have been called: {e}");
    }
    let want_collected = CollectedSourceData {
        frontmatters: vec![fm("skill1", "desc1"), fm("skill2", "desc2")],
        instructions: [("skill1", "instructions1"), ("skill2", "instructions2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        resources: ordered(vec![(
            "skill1",
            files(&[
                ("assets/image.png", b"image-data"),
                ("references/doc.txt", b"doc-data"),
                ("scripts/script.py", b"script-data"),
            ]),
        )]),
    };
    assert_eq!(collected, want_collected, "collectedSourceData mismatch");

    // Test reload.
    {
        let mut s = mock.state();
        s.frontmatters = vec![fm("skill1", "desc1_updated")];
        s.instructions
            .insert("skill1".into(), "instructions1_updated".into());
        s.resources = resources(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data-2")]),
        )]);
    }

    reload().await.unwrap();

    mock.verify_call_count(1 + 1, 2 + 1, 2 + 1, 3 + 1).unwrap();
    let collected = collect_source_data(source.as_ref()).await.unwrap();
    if let Err(e) = mock.verify_call_count(1 + 1, 2 + 1, 2 + 1, 3 + 1) {
        panic!("the base mock should not have been called: {e}");
    }
    let want_collected = CollectedSourceData {
        frontmatters: vec![fm("skill1", "desc1_updated")],
        instructions: [("skill1".to_string(), "instructions1_updated".to_string())].into(),
        resources: ordered(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data-2")]),
        )]),
    };
    assert_eq!(collected, want_collected, "collectedSourceData mismatch");
}

#[tokio::test]
async fn test_with_complete_preload_source_reload_failure_does_not_invalidate_cache() {
    let mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data")]),
        )]),
        ..Default::default()
    });

    let (source, reload) = with_complete_preload(mock.clone()).await.unwrap();
    let collected = collect_source_data(source.as_ref()).await.unwrap();
    let want_collected = CollectedSourceData {
        frontmatters: vec![fm("skill1", "")],
        instructions: [("skill1".to_string(), "instructions1".to_string())].into(),
        resources: ordered(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data")]),
        )]),
    };
    assert_eq!(collected, want_collected, "collectedSourceData mismatch");

    {
        let mut s = mock.state();
        s.frontmatters = vec![fm("skill2", "")];
        s.instructions = strings(&[("skill2", "instructions2")]);
        s.resources = resources(vec![(
            "skill2",
            files(&[("assets/image2.png", b"image-data-2")]),
        )]);
        s.list_resources_err = Some("list resources error");
    }

    assert!(reload().await.is_err(), "Reload should have failed");

    let collected = collect_source_data(source.as_ref()).await.unwrap();
    // should be unchanged.
    assert_eq!(collected, want_collected, "collectedSourceData mismatch");
}

#[tokio::test]
async fn test_with_complete_preload_source_list_resources_filtering() {
    let mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            files(&[
                ("assets/image.png", b"image-data"),
                ("references/doc.txt", b"doc-data"),
                ("scripts/script.py.js", b"js-script-data"),
                ("scripts/script.py", b"py-script-data"),
                ("scripts/script", b"script-data"),
                ("scripts/script/another_script", b"another-script-data"),
            ]),
        )]),
        ..Default::default()
    });

    let (source, _reload) = with_complete_preload(mock).await.unwrap();

    let tests: Vec<(&str, &str, Vec<&str>)> = vec![
        ("Folder match", "assets", vec!["assets/image.png"]),
        (
            "Folder match with trailing slash",
            "assets/",
            vec!["assets/image.png"],
        ),
        (
            "Another folder match",
            "references",
            vec!["references/doc.txt"],
        ),
        (
            "Specific resource request",
            "scripts/script.py",
            vec!["scripts/script.py"],
        ),
        // Whether or not it is possible to have both a file and a directory with the same
        // name is dependent on the base source implementation.
        (
            "Match both directory and file with the same name",
            "scripts/script",
            vec!["scripts/script/another_script", "scripts/script"],
        ),
        ("No match", "nonexistent", vec![]),
        (
            "Root match",
            ".",
            vec![
                "assets/image.png",
                "references/doc.txt",
                "scripts/script",
                "scripts/script.py",
                "scripts/script/another_script",
                "scripts/script.py.js",
            ],
        ),
    ];
    for (name, subpath, want_paths) in tests {
        let mut paths = source
            .list_resources("skill1", subpath)
            .await
            .unwrap_or_else(|e| panic!("{name}: ListResources failed: {e}"));
        // Go compares with cmpopts.SortSlices.
        paths.sort();
        let mut want: Vec<String> = want_paths.into_iter().map(String::from).collect();
        want.sort();
        assert_eq!(paths, want, "{name}: ListResources mismatch");
    }
}

#[tokio::test]
async fn test_with_complete_preload_source_load_resource() {
    let mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            files(&[
                ("assets/image1.png", b"image-data-1"),
                ("assets/dir/.././image2.png", b"image-data-2"),
            ]),
        )]),
        ..Default::default()
    });
    let (source, _reload) = with_complete_preload(mock).await.unwrap();

    let tests: Vec<(&str, &str, &str, Option<SkillErrorKind>)> = vec![
        ("Direct path", "assets/image1.png", "image-data-1", None),
        (
            "Complex path",
            "assets/../././assets/./image2.png",
            "image-data-2",
            None,
        ),
        (
            "Not existing resource",
            "assets/nonexistent.png",
            "",
            Some(SkillErrorKind::ResourceNotFound),
        ),
    ];
    for (name, path, want, want_err) in tests {
        let res = source.load_resource("skill1", path).await;
        let got_err = res.as_ref().err().map(SkillError::kind);
        assert_eq!(
            got_err, want_err,
            "{name}: LoadResource(\"skill1\", {path:?}) error"
        );
        let Ok(mut resource) = res else { continue };
        let mut data = String::new();
        resource.read_to_string(&mut data).await.unwrap();
        assert_eq!(data, want, "{name}: LoadResource(\"skill1\", {path:?})");
    }
}

#[tokio::test]
async fn test_with_complete_preload_source_resource_size_limit() {
    let mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            [(
                "assets/justRight.png".to_string(),
                vec![0u8; MAX_RESOURCE_BYTES],
            )]
            .into(),
        )]),
        ..Default::default()
    });
    let exceed_mock = MockCompletePreloadBaseSource::new(MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            [(
                "assets/tooLarge.png".to_string(),
                vec![0u8; MAX_RESOURCE_BYTES + 1],
            )]
            .into(),
        )]),
        ..Default::default()
    });

    let res = with_complete_preload(mock).await;
    let exceed = with_complete_preload(exceed_mock).await;

    if let Err(e) = res {
        panic!("WithCompletePreloadSource() error for maximum size data: {e}");
    }
    let Err(exceed_err) = exceed else {
        panic!("WithCompletePreloadSource() should have failed for exceed maximum size data");
    };
    assert_eq!(
        exceed_err.to_string(),
        "resource \"assets/tooLarge.png\" in skill \"skill1\" exceeds 10485760 bytes limit"
    );
}

#[tokio::test]
async fn test_with_complete_preload_source_errors() {
    let good_source = || MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data")]),
        )]),
        ..Default::default()
    };
    let success_source = good_source();
    let list_frontmatters_failed_source = MockState {
        list_frontmatters_err: Some(TEST_ERR),
        ..good_source()
    };
    let load_instructions_failed_source = MockState {
        load_instructions_err: Some(TEST_ERR),
        ..good_source()
    };
    let list_resources_failed_source = MockState {
        list_resources_err: Some(TEST_ERR),
        ..good_source()
    };
    let load_resource_failed_source = MockState {
        load_resource_err: Some(TEST_ERR),
        ..good_source()
    };
    let mut duplicate_skill_source = good_source();
    let dup = duplicate_skill_source.frontmatters.clone();
    duplicate_skill_source.frontmatters.extend(dup);

    /// Go `errors.Is(err, want)`.
    #[derive(Debug)]
    enum Want {
        Ok,
        TestErr,
        Kind(SkillErrorKind),
    }
    let tests: Vec<(&str, MockState, Want)> = vec![
        ("Succeed", success_source, Want::Ok), // for completeness
        (
            "ListFrontmatters error",
            list_frontmatters_failed_source,
            Want::TestErr,
        ),
        (
            "LoadInstructions error",
            load_instructions_failed_source,
            Want::TestErr,
        ),
        (
            "ListResources error",
            list_resources_failed_source,
            Want::TestErr,
        ),
        (
            "LoadResource error",
            load_resource_failed_source,
            Want::TestErr,
        ),
        (
            "Duplicate skill error",
            duplicate_skill_source,
            Want::Kind(SkillErrorKind::DuplicateSkill),
        ),
    ];
    for (name, state, want) in tests {
        let res = with_complete_preload(MockCompletePreloadBaseSource::new(state)).await;
        let ok = match (&res, &want) {
            (Ok(_), Want::Ok) => true,
            (Err(e), Want::TestErr) => chain_has(e, TEST_ERR),
            (Err(e), Want::Kind(kind)) => e.kind() == *kind,
            _ => false,
        };
        let got = res.as_ref().err().map(ToString::to_string);
        assert!(
            ok,
            "{name}: WithCompletePreloadSource() error = {got:?}, want error {want:?}"
        );
    }
}

/// Go's wrapping texts on each reload step.
#[tokio::test]
async fn complete_preload_error_texts_match_go() {
    let base = || MockState {
        frontmatters: vec![fm("skill1", "")],
        instructions: strings(&[("skill1", "instructions1")]),
        resources: resources(vec![(
            "skill1",
            files(&[("assets/image.png", b"image-data")]),
        )]),
        ..Default::default()
    };
    let cases = [
        (
            MockState {
                list_frontmatters_err: Some(TEST_ERR),
                ..base()
            },
            "list frontmatters: test error",
        ),
        (
            MockState {
                load_instructions_err: Some(TEST_ERR),
                ..base()
            },
            "load instructions for skill \"skill1\": test error",
        ),
        (
            MockState {
                list_resources_err: Some(TEST_ERR),
                ..base()
            },
            "list resources for skill \"skill1\": test error",
        ),
        (
            MockState {
                load_resource_err: Some(TEST_ERR),
                ..base()
            },
            "resource path \"assets/image.png\" in skill \"skill1\": test error",
        ),
    ];
    for (state, want) in cases {
        let err = with_complete_preload(MockCompletePreloadBaseSource::new(state))
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), want);
    }
    let (source, _reload) = with_complete_preload(MockCompletePreloadBaseSource::new(base()))
        .await
        .unwrap();
    assert_eq!(
        source
            .load_instructions("nope")
            .await
            .unwrap_err()
            .to_string(),
        "skill not found"
    );
    let err = source
        .load_resource("skill1", "assets/nope")
        .await
        .err()
        .unwrap();
    assert_eq!(
        (err.kind(), err.to_string()),
        (
            SkillErrorKind::ResourceNotFound,
            "resource not found".to_string()
        )
    );
}
