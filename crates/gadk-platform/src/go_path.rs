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

//! Go `path.Clean`, `path.Join`, `path.Dir` and `path.Base` (lexical, slash-separated paths;
//! on Unix `path/filepath`'s `Clean`/`Join`/`Dir`/`Base` are the same functions).

/// Go `path.Clean` (lexical, slash-separated): drops empty and `.` elements, resolves `..`
/// against the preceding element, keeps leading `..` elements of a relative path, drops `..`
/// at the root of a rooted path, and returns `.` for an empty result (`/` when rooted).
pub fn go_path_clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_owned();
    }
    let rooted = p.starts_with('/');
    let mut elems: Vec<&str> = Vec::new();
    for elem in p.split('/') {
        match elem {
            "" | "." => {}
            ".." => {
                if elems.last().is_some_and(|last| *last != "..") {
                    elems.pop();
                } else if !rooted {
                    elems.push("..");
                }
            }
            real => elems.push(real),
        }
    }
    let joined = elems.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// Go `path.Join`: the non-empty elements joined with `/` and cleaned; `""` when all are
/// empty.
pub fn go_path_join<S: AsRef<str>>(elems: &[S]) -> String {
    let parts: Vec<&str> = elems
        .iter()
        .map(AsRef::as_ref)
        .filter(|e| !e.is_empty())
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    go_path_clean(&parts.join("/"))
}

/// Go `path.Dir` (and `filepath.Dir` on Unix): everything up to the last `/`, cleaned; `.`
/// without a `/`.
pub fn go_path_dir(p: &str) -> String {
    let end = p.rfind('/').map_or(0, |i| i + 1);
    go_path_clean(&p[..end])
}

/// Go `path.Base` (and `filepath.Base` on Unix): the last element, trailing slashes removed;
/// `.` for an empty path, `/` for a path of slashes.
pub fn go_path_base(p: &str) -> String {
    if p.is_empty() {
        return ".".to_owned();
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_owned(),
        None => trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go path/path_test.go `cleantests`.
    #[test]
    fn path_clean_matches_go() {
        for (input, want) in [
            // Already clean
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            (".", "."),
            ("..", ".."),
            ("../..", "../.."),
            ("../../abc", "../../abc"),
            ("/abc", "/abc"),
            ("/", "/"),
            // Remove trailing slash
            ("abc/", "abc"),
            ("abc/def/", "abc/def"),
            ("a/b/c/", "a/b/c"),
            ("./", "."),
            ("../", ".."),
            ("../../", "../.."),
            ("/abc/", "/abc"),
            // Remove doubled slash
            ("abc//def//ghi", "abc/def/ghi"),
            ("//abc", "/abc"),
            ("///abc", "/abc"),
            ("//abc//", "/abc"),
            ("abc//", "abc"),
            // Remove . elements
            ("abc/./def", "abc/def"),
            ("/./abc/def", "/abc/def"),
            ("abc/.", "abc"),
            // Remove .. elements
            ("abc/def/ghi/../jkl", "abc/def/jkl"),
            ("abc/def/../ghi/../jkl", "abc/jkl"),
            ("abc/def/..", "abc"),
            ("abc/def/../..", "."),
            ("/abc/def/../..", "/"),
            ("abc/def/../../..", ".."),
            ("/abc/def/../../..", "/"),
            ("abc/def/../../../ghi/jkl/../../../mno", "../../mno"),
            // Combinations
            ("abc/./../def", "def"),
            ("abc//./../def", "def"),
            ("abc/../../././../def", "../../def"),
        ] {
            assert_eq!(go_path_clean(input), want, "Clean({input:?})");
        }
    }

    /// Go path/path_test.go `jointests`.
    #[test]
    fn path_join_matches_go() {
        let cases: &[(&[&str], &str)] = &[
            (&[], ""),
            (&[""], ""),
            (&["a"], "a"),
            (&["a", "b"], "a/b"),
            (&["a", ""], "a"),
            (&["", "b"], "b"),
            (&["/", "a"], "/a"),
            (&["/", ""], "/"),
            (&["a/", "b"], "a/b"),
            (&["a/", ""], "a"),
            (&["", ""], ""),
            (&["/a/../b", "./ws"], "/b/ws"),
        ];
        for (elems, want) in cases {
            assert_eq!(go_path_join(elems), *want, "Join({elems:?})");
        }
    }

    /// Go path/path_test.go `dirtests` and `basetests` (merged; `filepath` agrees on Unix),
    /// plus the rows gadk's callers rely on (config paths, the webui's trailing slashes).
    #[test]
    fn path_dir_and_base_match_go() {
        for (p, dir, base) in [
            ("", ".", "."),
            (".", ".", "."),
            ("/.", "/", "."),
            ("/", "/", "/"),
            ("//", "/", "/"),
            ("///", "/", "/"),
            ("////", "/", "/"),
            ("/foo", "/", "foo"),
            ("x/", "x", "x"),
            ("abc", ".", "abc"),
            ("abc/def", "abc", "def"),
            ("abc////def", "abc", "def"),
            ("a/b/.x", "a/b", ".x"),
            ("a/b/c.", "a/b", "c."),
            ("a/b/c.x", "a/b", "c.x"),
            ("/a/b", "/a", "b"),
            ("/a/b/", "/a/b", "b"),
            ("/a/b//", "/a/b", "b"),
            ("/a/b/root_agent.yaml", "/a/b", "root_agent.yaml"),
            ("/root_agent.yaml", "/", "root_agent.yaml"),
            ("root_agent.yaml", ".", "root_agent.yaml"),
            ("a//b/../c/x.yaml", "a/c", "x.yaml"),
        ] {
            assert_eq!(go_path_dir(p), dir, "Dir({p:?})");
            assert_eq!(go_path_base(p), base, "Base({p:?})");
        }
        // adkcli's folder name: Base(Dir(path)).
        assert_eq!(
            go_path_base(&go_path_dir("/w/x/dup/root_agent.yaml")),
            "dup"
        );
        assert_eq!(go_path_base(&go_path_dir("/root_agent.yaml")), "/");
    }
}
