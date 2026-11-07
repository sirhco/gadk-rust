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

//! Port of ADK-Go tool/skilltoolset/skill/filesystem_source_test.go.
//!
//! Go runs every case on `fstest.MapFS` behind `plainFS` (Open only); here every case runs on
//! [`MemFs`], whose three [`SkillFs`](crate::skills::skill::SkillFs) methods are all
//! `FileSystemSource` uses. Go's `errors.Is(err, want)` with `want == nil` is `err == nil`, so
//! a case's expected error is an `Option<SkillErrorKind>`.

use std::sync::Arc;

use tokio::io::AsyncReadExt;

use crate::skills::skill::{
    FileSystemSource, Frontmatter, MemFs, ResourceReader, SkillError, SkillErrorKind, Source,
};

/// Go's `wantErr`: `None` is Go's `nil`.
type WantErr = Option<SkillErrorKind>;

const VALID_SKILL: &str = "---\nname: test-skill\ndescription: test\n---\n";

fn source(files: &[(&str, &str)]) -> FileSystemSource {
    FileSystemSource::new(Arc::new(MemFs::new(files.iter().map(|(p, d)| (*p, *d)))))
}

fn kind<T>(res: &Result<T, SkillError>) -> Option<SkillErrorKind> {
    res.as_ref().err().map(SkillError::kind)
}

fn fm(name: &str, description: &str) -> Frontmatter {
    Frontmatter {
        name: name.into(),
        description: description.into(),
        ..Default::default()
    }
}

async fn read_all(mut reader: ResourceReader) -> Vec<u8> {
    let mut data = Vec::new();
    reader.read_to_end(&mut data).await.unwrap();
    data
}

// parity: adapted: Go's fstest.MapFS behind plainFS (Open only) becomes MemFs; errors.Is on a sentinel is SkillError::kind.
#[tokio::test]
async fn test_file_system_source_list_frontmatters() {
    let tools = || {
        vec![
            "Bash(git:*)".to_string(),
            "Bash(jq:*)".to_string(),
            "Read".to_string(),
        ]
    };
    let tests: Vec<(&str, FileSystemSource, Vec<Frontmatter>, WantErr)> = vec![
        (
            "Success",
            source(&[
                (
                    "math-skill/SKILL.md",
                    "---\nname: math-skill\ndescription: test\n---\n",
                ),
                (
                    "weather-skill/SKILL.md",
                    "---\nname: weather-skill\ndescription: test\n---\n",
                ),
                ("random-file.txt", "should be ignored"),
                ("SKILL.md", "should be ignored"),
                ("dir/not-skill.txt", "should be ignored"),
                ("sub/dir/SKILL.md", "should be ignored"),
            ]),
            vec![fm("math-skill", "test"), fm("weather-skill", "test")],
            None,
        ),
        (
            "Allowed tools formats",
            source(&[
                (
                    "spec-skill/SKILL.md",
                    "---\nname: spec-skill\ndescription: test\nallowed-tools: Bash(git:*) Bash(jq:*) Read\n---\nInstructions.",
                ),
                (
                    "list-skill/SKILL.md",
                    "---\nname: list-skill\ndescription: test\nallowed-tools:\n  - Bash(git:*)\n  - Bash(jq:*)\n  - Read\n---\nInstructions.",
                ),
                (
                    "other-skill/SKILL.md",
                    "---\nname: other-skill\ndescription: test\n---\nInstructions.",
                ),
            ]),
            vec![
                Frontmatter {
                    allowed_tools: tools(),
                    ..fm("spec-skill", "test")
                },
                Frontmatter {
                    allowed_tools: tools(),
                    ..fm("list-skill", "test")
                },
                fm("other-skill", "test"),
            ],
            None,
        ),
        (
            "Name mismatch",
            source(&[(
                "test-skill/SKILL.md",
                "---\nname: wrong-skill\ndescription: test\n---\n",
            )]),
            vec![],
            Some(SkillErrorKind::InvalidSkillName),
        ),
        (
            "Invalid frontmatter",
            source(&[("test-skill/SKILL.md", "---[INVALID_YAML")]),
            vec![],
            Some(SkillErrorKind::InvalidFrontmatter),
        ),
    ];
    for (name, source, mut want, want_err) in tests {
        let got = source.list_frontmatters().await;
        assert_eq!(
            kind(&got),
            want_err,
            "{name}: ListFrontmatters() error: {got:?}"
        );
        let mut got = got.unwrap_or_default();
        // Go compares with cmpopts.SortSlices by name.
        got.sort_by(|a, b| a.name.cmp(&b.name));
        want.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got, want, "{name}: ListFrontmatters() mismatch");
    }
}

// parity: adapted: Go's fstest.MapFS behind plainFS (Open only) becomes MemFs; errors.Is on a sentinel is SkillError::kind.
#[tokio::test]
async fn test_file_system_source_load_frontmatter() {
    let tests: Vec<(&str, FileSystemSource, Option<Frontmatter>, WantErr)> = vec![
        (
            "Success",
            source(&[("test-skill/SKILL.md", VALID_SKILL)]),
            Some(fm("test-skill", "test")),
            None,
        ),
        (
            "Name mismatch",
            source(&[(
                "test-skill/SKILL.md",
                "---\nname: wrong-skill\ndescription: test\n---\n",
            )]),
            None,
            Some(SkillErrorKind::InvalidSkillName),
        ),
        (
            "Invalid frontmatter",
            source(&[("test-skill/SKILL.md", "---[INVALID_YAML")]),
            None,
            Some(SkillErrorKind::InvalidFrontmatter),
        ),
        (
            "Skill not found",
            source(&[]),
            None,
            Some(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, source, want, want_err) in tests {
        let got = source.load_frontmatter("test-skill").await;
        assert_eq!(
            kind(&got),
            want_err,
            "{name}: LoadFrontmatter(\"test-skill\") error: {got:?}"
        );
        assert_eq!(
            got.ok(),
            want,
            "{name}: LoadFrontmatter(\"test-skill\") mismatch"
        );
    }
}

// parity: adapted: Go's fstest.MapFS behind plainFS (Open only) becomes MemFs; errors.Is on a sentinel is SkillError::kind.
#[tokio::test]
async fn test_file_system_source_load_instructions() {
    let tests: Vec<(&str, FileSystemSource, &str, WantErr)> = vec![
        (
            "Success",
            source(&[(
                "test-skill/SKILL.md",
                "---\nname: test-skill\ndescription: test\n---\nMath instructions.",
            )]),
            "Math instructions.",
            None,
        ),
        (
            "Name mismatch",
            source(&[(
                "test-skill/SKILL.md",
                "---\nname: wrong-name\ndescription: test\n---\n",
            )]),
            "",
            Some(SkillErrorKind::InvalidSkillName),
        ),
        (
            "Invalid YAML",
            source(&[("test-skill/SKILL.md", "---[INVALID_YAML")]),
            "",
            Some(SkillErrorKind::InvalidFrontmatter),
        ),
        (
            "Skill not found",
            source(&[]),
            "",
            Some(SkillErrorKind::SkillNotFound),
        ),
    ];
    for (name, source, want, want_err) in tests {
        let got = source.load_instructions("test-skill").await;
        assert_eq!(
            kind(&got),
            want_err,
            "{name}: LoadInstructions(\"test-skill\") error: {got:?}"
        );
        assert_eq!(
            got.unwrap_or_default(),
            want,
            "{name}: LoadInstructions(\"test-skill\")"
        );
    }
}

// parity: adapted: Go's fstest.MapFS behind plainFS (Open only) becomes MemFs; errors.Is on a sentinel is SkillError::kind.
#[tokio::test]
async fn test_file_system_source_load_resource() {
    let tests: Vec<(&str, &str, FileSystemSource, WantErr, &[u8])> = vec![
        (
            "Success Asset",
            "assets/image.png",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/assets/image.png", "image-data"),
            ]),
            None,
            b"image-data",
        ),
        (
            "Success Reference",
            "references/doc.txt",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/references/doc.txt", "doc-data"),
            ]),
            None,
            b"doc-data",
        ),
        (
            "Success Script",
            "scripts/script.py",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/scripts/script.py", "python-code"),
            ]),
            None,
            b"python-code",
        ),
        (
            "Success Clean Path resolves traversal safely",
            "assets/../assets/images/../images/./image.png",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/assets/images/image.png", "image-data"),
            ]),
            None,
            b"image-data",
        ),
        (
            "Error Skill Not Found",
            "assets/image.png",
            source(&[]),
            Some(SkillErrorKind::SkillNotFound),
            b"",
        ),
        (
            "Error Not a Skill",
            "assets/image.png",
            // No SKILL.md file - not a skill.
            source(&[("test-skill/assets/image.png", "image-data")]),
            Some(SkillErrorKind::SkillNotFound),
            b"",
        ),
        (
            "Error Invalid Skill Name",
            "assets/image.png",
            source(&[
                (
                    "test-skill/SKILL.md",
                    "---\nname: wrong-name\ndescription: test\n---\n",
                ),
                ("test-skill/assets/image.png", "image-data"),
            ]),
            Some(SkillErrorKind::InvalidSkillName),
            b"",
        ),
        (
            "Error Invalid Frontmatter",
            "assets/image.png",
            source(&[
                ("test-skill/SKILL.md", "Invalid YAML"),
                ("test-skill/assets/image.png", "image-data"),
            ]),
            Some(SkillErrorKind::InvalidFrontmatter),
            b"",
        ),
        (
            "Error Traversal Attempt",
            "../../etc/passwd",
            source(&[("test-skill/SKILL.md", VALID_SKILL)]),
            Some(SkillErrorKind::InvalidResourcePath),
            b"",
        ),
        (
            "Error Unauthorized Directory",
            "unauthorized/file.txt",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/unauthorized/file.txt", "secret"),
            ]),
            Some(SkillErrorKind::InvalidResourcePath),
            b"",
        ),
        (
            "Error File Not Found",
            "assets/missing.png",
            source(&[("test-skill/SKILL.md", VALID_SKILL)]),
            Some(SkillErrorKind::ResourceNotFound),
            b"",
        ),
    ];
    for (name, resource_path, source, want_err, want) in tests {
        let res = source.load_resource("test-skill", resource_path).await;
        let got_err = kind(&res);
        assert_eq!(
            got_err, want_err,
            "{name}: LoadResource(\"test-skill\", {resource_path:?}) error"
        );
        let Ok(resource) = res else { continue };
        assert_eq!(
            read_all(resource).await,
            want,
            "{name}: LoadResource(\"test-skill\", {resource_path:?})"
        );
    }
}

// parity: adapted: Go's fstest.MapFS behind plainFS (Open only) becomes MemFs; errors.Is on a sentinel is SkillError::kind.
#[tokio::test]
async fn test_file_system_source_list_resources() {
    let tests: Vec<(&str, &str, FileSystemSource, WantErr, Vec<&str>)> = vec![
        (
            "Success Root",
            ".",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/unauthorized/file.txt", ""),
                ("test-skill/references/doc.txt", ""),
                ("test-skill/references/sub/doc.txt", ""),
                ("test-skill/assets/image.png", ""),
                ("test-skill/scripts/script.py", ""),
            ]),
            None,
            vec![
                "references/doc.txt",
                "references/sub/doc.txt",
                "assets/image.png",
                "scripts/script.py",
            ],
        ),
        (
            "Success Root Empty search path",
            ".",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/references/doc.txt", ""),
            ]),
            None,
            vec!["references/doc.txt"],
        ),
        (
            "Success Specific Dir",
            "assets",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/references/doc.txt", ""),
                ("test-skill/assets/image.png", ""),
                ("test-skill/assets/images/image.png", ""),
            ]),
            None,
            vec!["assets/image.png", "assets/images/image.png"],
        ),
        (
            "Error Skill Not Found",
            ".",
            source(&[]),
            Some(SkillErrorKind::SkillNotFound),
            vec![],
        ),
        (
            "Error Skill name mismatch",
            ".",
            source(&[(
                "test-skill/SKILL.md",
                "---\nname: wrong-skill\ndescription: test\n---\n",
            )]),
            Some(SkillErrorKind::InvalidSkillName),
            vec![],
        ),
        (
            "Error Invalid frontmatter",
            ".",
            source(&[("test-skill/SKILL.md", "Invalid YAML")]),
            Some(SkillErrorKind::InvalidFrontmatter),
            vec![],
        ),
        (
            "Error Unauthorized Directory",
            "unauthorized",
            source(&[
                ("test-skill/SKILL.md", VALID_SKILL),
                ("test-skill/unauthorized/file.txt", ""),
            ]),
            Some(SkillErrorKind::InvalidResourcePath),
            vec![],
        ),
        (
            "Error Directory Not Found",
            "references/missing_dir",
            source(&[("test-skill/SKILL.md", VALID_SKILL)]),
            Some(SkillErrorKind::ResourceNotFound),
            vec![],
        ),
    ];
    for (name, search_path, source, want_err, want) in tests {
        let got = source.list_resources("test-skill", search_path).await;
        assert_eq!(
            kind(&got),
            want_err,
            "{name}: ListResources(\"test-skill\", {search_path:?}) error: {got:?}"
        );
        // Go compares with cmpopts.SortSlices.
        let mut got = got.unwrap_or_default();
        got.sort();
        let mut want: Vec<String> = want.into_iter().map(String::from).collect();
        want.sort();
        assert_eq!(
            got, want,
            "{name}: ListResources(\"test-skill\", {search_path:?})"
        );
    }
}

/// Beyond Go's sort-insensitive comparison: the walk order is Go's (references, then assets,
/// then scripts; lexical pre-order inside each), and the error texts are Go's.
#[tokio::test]
async fn list_resources_order_and_error_texts_match_go() {
    let src = source(&[
        ("test-skill/SKILL.md", VALID_SKILL),
        ("test-skill/scripts/a.sh", ""),
        ("test-skill/assets/z.png", ""),
        ("test-skill/references/b/x.md", ""),
        ("test-skill/references/a.md", ""),
        ("test-skill/references/c.md", ""),
    ]);
    assert_eq!(
        src.list_resources("test-skill", ".").await.unwrap(),
        [
            "references/a.md",
            "references/b/x.md",
            "references/c.md",
            "assets/z.png",
            "scripts/a.sh"
        ]
    );
    assert_eq!(
        src.list_resources("test-skill", "references/c.md")
            .await
            .unwrap(),
        ["references/c.md"]
    );
    assert_eq!(
        src.list_resources("test-skill", "unauthorized")
            .await
            .unwrap_err()
            .to_string(),
        "invalid resource path: \"unauthorized\" must be empty, root (.), or within 'references/', 'assets/', or 'scripts/'"
    );
    assert_eq!(
        src.load_resource("test-skill", "../../etc/passwd")
            .await
            .err()
            .unwrap()
            .to_string(),
        "invalid resource path: \"../../etc/passwd\" must be within 'references/', 'assets/', or 'scripts/' (relative to skill directory)"
    );
    assert_eq!(
        src.load_resource("test-skill", "assets/missing.png")
            .await
            .err()
            .unwrap()
            .to_string(),
        "resource not found: \"assets/missing.png\""
    );
    assert_eq!(
        src.list_resources("test-skill", "references/missing_dir")
            .await
            .unwrap_err()
            .to_string(),
        "resource not found: \"references/missing_dir\""
    );
    assert_eq!(
        src.load_frontmatter("other").await.unwrap_err().to_string(),
        "skill not found: \"other\""
    );
    let mismatch = source(&[(
        "test-skill/SKILL.md",
        "---\nname: wrong-skill\ndescription: test\n---\n",
    )]);
    assert_eq!(
        mismatch
            .load_frontmatter("test-skill")
            .await
            .unwrap_err()
            .to_string(),
        "invalid skill name: name in SKILL.md (\"wrong-skill\") does not match directory name (\"test-skill\")"
    );
    let invalid = source(&[("test-skill/SKILL.md", "---[INVALID_YAML")]);
    assert_eq!(
        invalid
            .load_frontmatter("test-skill")
            .await
            .unwrap_err()
            .to_string(),
        "invalid frontmatter: parse frontmatter: reading first line: EOF"
    );
}
