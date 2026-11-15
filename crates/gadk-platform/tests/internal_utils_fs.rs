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

//! Port of ADK-Go `internal/utils/fs_test.go` (`SafeSubpath` lexical cases, `isLinkLike`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use gadk_platform::{go_filepath_rel, is_link_like, safe_subpath};

#[derive(Clone, Copy)]
struct NormalizeCase {
    name: &'static str,
    rel: &'static str,
    want_err: bool,
    want_suffix: &'static str,
}

const fn ok(name: &'static str, rel: &'static str, want_suffix: &'static str) -> NormalizeCase {
    NormalizeCase {
        name,
        rel,
        want_err: false,
        want_suffix,
    }
}

const fn err(name: &'static str, rel: &'static str) -> NormalizeCase {
    NormalizeCase {
        name,
        rel,
        want_err: true,
        want_suffix: "",
    }
}

/// Go `runTests`: every case against `SafeSubpath` for the given base.
fn run_tests(base: &str, cases: &[NormalizeCase]) {
    for tc in cases {
        let got = safe_subpath(Path::new(base), tc.rel);
        if tc.want_err {
            assert!(
                got.is_err(),
                "{}: Normalize({base:?}, {:?}) = {got:?}; want error (escape/invalid)",
                tc.name,
                tc.rel
            );
            continue;
        }
        let (got, got_rel) = got.unwrap_or_else(|e| {
            panic!(
                "{}: Normalize({base:?}, {:?}) returned error {e}; want success",
                tc.name, tc.rel
            )
        });
        // Defense-in-depth: a success must never land outside base.
        assert!(
            within_base(base, &got),
            "{}: SafeSubpath({base:?}, {:?}) = ({got:?}, {got_rel:?}); result escapes base",
            tc.name,
            tc.rel
        );
        assert_eq!(
            got_rel, tc.want_suffix,
            "{}: SafeSubpath({base:?}, {:?}) = ({got:?}, {got_rel:?})",
            tc.name, tc.rel
        );
    }
}

/// Go `withinBase`: `p` is `base` or a descendant of it.
fn within_base(base: &str, p: &Path) -> bool {
    let Ok(rel) = go_filepath_rel(base, &p.to_string_lossy()) else {
        return false;
    };
    if rel == ".." || rel == "." {
        return rel == ".";
    }
    !rel.starts_with("../")
}

/// Go `commonSubpathCases`: identical on all platforms.
const COMMON_SUBPATH_CASES: &[NormalizeCase] = &[
    // Accepted
    ok("simple_file", "file.txt", "file.txt"),
    ok("leading_dot_slash", "./file.txt", "file.txt"),
    ok(
        "interior_dotdot_stays_inside",
        "sub/../file.txt",
        "file.txt",
    ),
    ok("trailing_slash", "sub/", "sub"),
    // Rejected
    // We don't allow that, it allows to test for the physical path (brute-force)
    err("dotdot_nets_back_into_base", "../data/inside.txt"),
    // Empty input
    err("empty_resolves_to_base", ""),
    err("dot_resolves_to_base", "."),
    // Escapes
    err("parent_escape", "../x"),
    err("deep_escape", "../../../../etc/secret"),
    err("interior_escape", "sub/../../x"),
    err("mixed_valid_then_escape", "foo/../../../bar"),
    err("bare_dotdot", ".."),
    err("sibling_prefix_dash", "../data-evil/x"),
    err("sibling_prefix_word", "../database/x"),
    // NUL rune
    err("embedded_null_byte", "a\x00.txt"),
];

// parity: skipped TestSafeSubpath_Windows: Windows-only (Go skips it elsewhere); gadk's SafeSubpath port has Unix path semantics only

#[test]
fn test_safe_subpath_non_windows() {
    const BASE: &str = "/srv/app/data";
    let mut cases: Vec<NormalizeCase> = COMMON_SUBPATH_CASES.to_vec();
    cases.extend([
        ok("nested_descendant", "sub/dir/file.txt", "sub/dir/file.txt"),
        ok(
            "interior_single_dot",
            "sub/./nested/file.txt",
            "sub/nested/file.txt",
        ),
        ok(
            "collapsed_double_slash",
            "sub//double.txt",
            "sub/double.txt",
        ),
        // ---- Allowed: characters that are LITERAL on POSIX filesystems ----
        // Backslash is a normal filename byte on POSIX, NOT a separator: this must be a single
        // file inside base, not treated as traversal.
        ok(
            "backslash_is_literal_not_separator",
            r"weird\name.txt",
            r"weird\name.txt",
        ),
        ok(
            "colon_is_literal",
            "file:with:colons.txt",
            "file:with:colons.txt",
        ),
        ok("dotfile", ".hiddenfile", ".hiddenfile"),
        ok("unicode_name", "naïve-名前.txt", "naïve-名前.txt"),
        // ---- Rejected: absolute override ----
        // An absolute rel must not silently replace the base.
        err("absolute_override", "/etc/passwd"),
        err("leading_double_slash_absolute", "//etc/passwd"),
        // ---- Rejected: traversal ----
        err("classic_traversal", "../../etc/passwd"),
        err("traversal_into_proc", "../../../proc/self/environ"),
        err("null_byte_with_traversal", "foo\x00/../../etc/passwd"),
    ]);
    run_tests(BASE, &cases);
}

/// Go `TestIsLinkLike`: the mode predicate `followLinks` uses to decide which components to
/// ask `os.Readlink` about.
// parity: adapted: Go builds fs.FileModes by hand; Rust's FileType comes only from the filesystem, so the regular file, directory and symlink rows read real entries, and the "junction or other reparse point" row (ModeIrregular, Windows-only) is left out
#[test]
fn test_is_link_like() {
    let dir = std::env::temp_dir().join(format!("gadk-islinklike-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("f.txt");
    std::fs::write(&file, b"x").unwrap();
    let sub = dir.join("d");
    std::fs::create_dir(&sub).unwrap();
    let link = dir.join("l");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    for (name, p, want) in [
        ("regular file", &file, false),
        ("directory", &sub, false),
        ("symlink", &link, true),
    ] {
        let ft = std::fs::symlink_metadata(p).unwrap().file_type();
        assert_eq!(is_link_like(&ft), want, "{name}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
