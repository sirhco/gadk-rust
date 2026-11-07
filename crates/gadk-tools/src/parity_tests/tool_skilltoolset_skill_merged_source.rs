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

//! Port of ADK-Go tool/skilltoolset/skill/merged_source_test.go.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::go_fmt::path_clean;
use crate::skills::skill::{
    Frontmatter, MergedSource, ResourceReader, SkillError, SkillErrorKind, Source,
};

/// The sources of one case, queried in order.
type Sources = Vec<Arc<dyn Source>>;

/// Go's `testErr := errors.New("test error")`.
const TEST_ERR: &str = "test error";

/// An error a mock returns: built fresh on every call, as `SkillError` is not `Clone`.
#[derive(Clone, Copy)]
struct ErrSpec(SkillErrorKind, &'static str);

impl ErrSpec {
    fn build(self) -> SkillError {
        SkillError::new(self.0, self.1)
    }
}

/// Go `testErr`.
const TEST_ERROR: ErrSpec = ErrSpec(SkillErrorKind::Other, TEST_ERR);
/// Go `ErrResourceNotFound`.
const RESOURCE_NOT_FOUND: ErrSpec = ErrSpec(SkillErrorKind::ResourceNotFound, "resource not found");

/// What a case expects (Go `errors.Is(err, want)`).
#[derive(Clone, Copy, Debug)]
enum Want {
    /// `want == nil`.
    Ok,
    /// A sentinel: the error has this kind.
    Kind(SkillErrorKind),
    /// `testErr`: the error, or an error it wraps, is the mock's `test error`.
    TestErr,
}

fn check<T>(res: &Result<T, SkillError>, want: Want) -> bool {
    match (res, want) {
        (Ok(_), Want::Ok) => true,
        (Err(e), Want::Kind(kind)) => e.kind() == kind,
        (Err(e), Want::TestErr) => e.kind() == SkillErrorKind::Other && e.to_string() == TEST_ERR,
        _ => false,
    }
}

/// Go `mockMergedBaseSource`.
#[derive(Default)]
struct MockMergedBaseSource {
    frontmatters: Vec<Frontmatter>,
    instructions: HashMap<String, String>,
    resources: HashMap<String, BTreeMap<String, String>>,
    list_frontmatters_err: Option<ErrSpec>,
    load_frontmatter_err: Option<ErrSpec>,
    load_instructions_err: Option<ErrSpec>,
    list_resources_err: Option<ErrSpec>,
    load_resource_err: Option<ErrSpec>,
}

fn names(names: &[&str]) -> Vec<Frontmatter> {
    names
        .iter()
        .map(|n| Frontmatter {
            name: (*n).into(),
            ..Default::default()
        })
        .collect()
}

fn with_frontmatters(fms: Vec<Frontmatter>) -> Arc<dyn Source> {
    Arc::new(MockMergedBaseSource {
        frontmatters: fms,
        ..Default::default()
    })
}

fn with_instructions(name: &str, instructions: &str) -> Arc<dyn Source> {
    Arc::new(MockMergedBaseSource {
        instructions: HashMap::from([(name.to_string(), instructions.to_string())]),
        ..Default::default()
    })
}

fn resource_map(name: &str, entries: &[(&str, &str)]) -> HashMap<String, BTreeMap<String, String>> {
    HashMap::from([(
        name.to_string(),
        entries
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect(),
    )])
}

fn with_resources(name: &str, entries: &[(&str, &str)]) -> Arc<dyn Source> {
    Arc::new(MockMergedBaseSource {
        resources: resource_map(name, entries),
        ..Default::default()
    })
}

fn empty() -> Arc<dyn Source> {
    Arc::new(MockMergedBaseSource::default())
}

fn failing(set: impl FnOnce(&mut MockMergedBaseSource)) -> Arc<dyn Source> {
    let mut mock = MockMergedBaseSource::default();
    set(&mut mock);
    Arc::new(mock)
}

#[async_trait]
impl Source for MockMergedBaseSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        if let Some(e) = self.list_frontmatters_err {
            return Err(e.build());
        }
        Ok(self.frontmatters.clone())
    }

    async fn list_resources(&self, name: &str, _subpath: &str) -> Result<Vec<String>, SkillError> {
        if let Some(e) = self.list_resources_err {
            return Err(e.build());
        }
        let Some(skill_resource) = self.resources.get(name) else {
            return Err(SkillError::new(
                SkillErrorKind::SkillNotFound,
                "skill not found",
            ));
        };
        Ok(skill_resource.keys().cloned().collect())
    }

    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        if let Some(e) = self.load_frontmatter_err {
            return Err(e.build());
        }
        self.frontmatters
            .iter()
            .find(|fm| fm.name == name)
            .cloned()
            .ok_or_else(|| SkillError::new(SkillErrorKind::SkillNotFound, "skill not found"))
    }

    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        if let Some(e) = self.load_instructions_err {
            return Err(e.build());
        }
        self.instructions
            .get(name)
            .cloned()
            .ok_or_else(|| SkillError::new(SkillErrorKind::SkillNotFound, "skill not found"))
    }

    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        if let Some(e) = self.load_resource_err {
            return Err(e.build());
        }
        let Some(skill_resource) = self.resources.get(name) else {
            return Err(SkillError::new(
                SkillErrorKind::SkillNotFound,
                "skill not found",
            ));
        };
        let Some(content) = skill_resource.get(&path_clean(resource_path)) else {
            return Err(SkillError::new(
                SkillErrorKind::ResourceNotFound,
                "resource not found",
            ));
        };
        Ok(Box::pin(std::io::Cursor::new(content.clone().into_bytes())))
    }
}

#[tokio::test]
async fn test_merged_source_list_frontmatters() {
    let tests: Vec<(&str, Sources, Vec<Frontmatter>, Want)> = vec![
        (
            "duplicates in a single source",
            vec![with_frontmatters(names(&["skill1", "skill1"]))],
            vec![],
            Want::Kind(SkillErrorKind::DuplicateSkill),
        ),
        (
            "duplicates among sources",
            vec![
                with_frontmatters(names(&["skill1"])),
                with_frontmatters(names(&["skill1"])),
            ],
            vec![],
            Want::Kind(SkillErrorKind::DuplicateSkill),
        ),
        (
            "error in source is returned",
            vec![failing(|m| m.list_frontmatters_err = Some(TEST_ERROR))],
            vec![],
            Want::TestErr,
        ),
        (
            "frontmatters are in the order of sources",
            vec![
                with_frontmatters(names(&["D", "B"])),
                with_frontmatters(names(&["A", "C"])),
            ],
            names(&["D", "B", "A", "C"]),
            Want::Ok,
        ),
    ];
    for (name, sources, want, want_err) in tests {
        let merged = MergedSource::new(sources);
        let got = merged.list_frontmatters().await;
        assert!(
            check(&got, want_err),
            "{name}: ListFrontmatters() error = {got:?}, want {want_err:?}"
        );
        let Ok(got) = got else { continue };
        assert_eq!(got, want, "{name}: ListFrontmatters mismatch");
    }
}

#[tokio::test]
async fn test_merged_source_load_frontmatter() {
    let skill_b = |d: &str| Frontmatter {
        name: "skill B".into(),
        description: d.into(),
        ..Default::default()
    };
    let tests: Vec<(&str, Sources, &str, Option<Frontmatter>, Want)> = vec![
        (
            "returns first success and stops search",
            vec![
                with_frontmatters(names(&["skill A"])),
                with_frontmatters(vec![skill_b("desc B")]),
                with_frontmatters(vec![skill_b("desc B dup")]),
                failing(|m| m.load_frontmatter_err = Some(TEST_ERROR)),
            ],
            "skill B",
            Some(skill_b("desc B")),
            Want::Ok,
        ),
        (
            "error precedence",
            vec![
                failing(|m| m.load_frontmatter_err = Some(TEST_ERROR)),
                with_frontmatters(names(&["skill A"])),
            ],
            "skill A",
            None,
            Want::TestErr,
        ),
        (
            "skill not found",
            vec![empty(), empty()],
            "skill C",
            None,
            Want::Kind(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, sources, skill_name, want, want_err) in tests {
        let merged = MergedSource::new(sources);
        let got = merged.load_frontmatter(skill_name).await;
        assert!(
            check(&got, want_err),
            "{name}: LoadFrontmatter({skill_name:?}) error = {got:?}, want {want_err:?}"
        );
        let Ok(got) = got else { continue };
        assert_eq!(Some(got), want, "{name}: LoadFrontmatter mismatch");
    }
}

#[tokio::test]
async fn test_merged_source_load_instructions() {
    let tests: Vec<(&str, Sources, &str, &str, Want)> = vec![
        (
            "returns first success and stops search",
            vec![
                with_instructions("skill A", "inst A"),
                with_instructions("skill B", "inst B"),
                with_instructions("skill B", "inst B dup"),
                failing(|m| m.load_instructions_err = Some(TEST_ERROR)),
            ],
            "skill B",
            "inst B",
            Want::Ok,
        ),
        (
            "error precedence",
            vec![
                failing(|m| m.load_instructions_err = Some(TEST_ERROR)),
                with_instructions("skill A", "inst A"),
            ],
            "skill A",
            "",
            Want::TestErr,
        ),
        (
            "skill not found",
            vec![empty(), empty()],
            "skill C",
            "",
            Want::Kind(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, sources, skill_name, want, want_err) in tests {
        let merged = MergedSource::new(sources);
        let got = merged.load_instructions(skill_name).await;
        assert!(
            check(&got, want_err),
            "{name}: LoadInstructions({skill_name:?}) error = {got:?}, want {want_err:?}"
        );
        let Ok(got) = got else { continue };
        assert_eq!(got, want, "{name}: LoadInstructions({skill_name:?})");
    }
}

#[tokio::test]
async fn test_merged_source_list_resources() {
    let tests: Vec<(&str, Sources, &str, Vec<&str>, Want)> = vec![
        (
            "returns first success and stops search",
            vec![
                with_resources("skill A", &[("p1", "c1")]),
                with_resources("skill B", &[("p2", "c2")]),
                with_resources("skill B", &[("p3", "c3")]),
                failing(|m| m.list_resources_err = Some(TEST_ERROR)),
            ],
            "skill B",
            vec!["p2"],
            Want::Ok,
        ),
        (
            "error precedence",
            vec![
                failing(|m| m.list_resources_err = Some(TEST_ERROR)),
                with_resources("skill A", &[("p1", "c1")]),
            ],
            "skill A",
            vec![],
            Want::TestErr,
        ),
        (
            "ResourceNotFound stops query",
            vec![
                failing(|m| {
                    m.resources = resource_map("skill A", &[]);
                    m.list_resources_err = Some(RESOURCE_NOT_FOUND);
                }),
                with_resources("skill A", &[("p1", "c1")]),
            ],
            "skill A",
            vec![],
            Want::Kind(SkillErrorKind::ResourceNotFound),
        ),
        (
            "skill not found",
            vec![empty(), empty()],
            "skill C",
            vec![],
            Want::Kind(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, sources, skill_name, want, want_err) in tests {
        let merged = MergedSource::new(sources);
        let got = merged.list_resources(skill_name, ".").await;
        assert!(
            check(&got, want_err),
            "{name}: ListResources({skill_name:?}, \".\") error = {got:?}, want {want_err:?}"
        );
        let Ok(got) = got else { continue };
        assert_eq!(got, want, "{name}: ListResources mismatch");
    }
}

#[tokio::test]
async fn test_merged_source_load_resource() {
    let tests: Vec<(&str, Sources, &str, &str, &str, Want)> = vec![
        (
            "returns first success and stops search",
            vec![
                with_resources("skill A", &[("p1", "c1")]),
                with_resources("skill B", &[("p2", "c2")]),
                with_resources("skill B", &[("p3", "c3")]),
                failing(|m| m.load_resource_err = Some(TEST_ERROR)),
            ],
            "skill B",
            "p2",
            "c2",
            Want::Ok,
        ),
        (
            "error precedence",
            vec![
                failing(|m| m.load_resource_err = Some(TEST_ERROR)),
                with_resources("skill A", &[("p1", "c1")]),
            ],
            "skill A",
            "p1",
            "",
            Want::TestErr,
        ),
        (
            "ResourceNotFound stops query",
            vec![
                failing(|m| {
                    m.resources = resource_map("skill A", &[]);
                    m.load_resource_err = Some(RESOURCE_NOT_FOUND);
                }),
                with_resources("skill A", &[("p1", "c1")]),
            ],
            "skill A",
            "p1",
            "",
            Want::Kind(SkillErrorKind::ResourceNotFound),
        ),
        (
            "skill not found",
            vec![empty(), empty()],
            "skill C",
            "p1",
            "",
            Want::Kind(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, sources, skill_name, resource_path, want, want_err) in tests {
        let merged = MergedSource::new(sources);
        let got = merged.load_resource(skill_name, resource_path).await;
        let got_err = got.as_ref().err().map(|e| (e.kind(), e.to_string()));
        assert!(
            check(&got, want_err),
            "{name}: LoadResource({skill_name:?}, {resource_path:?}) error = {got_err:?}, want {want_err:?}"
        );
        let Ok(mut reader) = got else { continue };
        let mut data = String::new();
        reader.read_to_string(&mut data).await.unwrap();
        assert_eq!(
            data, want,
            "{name}: LoadResource({skill_name:?}, {resource_path:?})"
        );
    }
}

/// A miss in every source is Go's bare `ErrSkillNotFound`; a duplicate names the skill.
#[tokio::test]
async fn merged_source_error_texts_match_go() {
    let merged = MergedSource::new(vec![empty()]);
    assert_eq!(
        merged.load_instructions("x").await.unwrap_err().to_string(),
        "skill not found"
    );
    let dup = MergedSource::new(vec![
        with_frontmatters(names(&["a"])),
        with_frontmatters(names(&["a"])),
    ]);
    assert_eq!(
        dup.list_frontmatters().await.unwrap_err().to_string(),
        "duplicate skill: \"a\""
    );
}
