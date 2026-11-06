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

//! Branch derivation helpers (port of Go workflow/branch.go).

/// Port of Go deriveSubBranch (branch.go:37): appends `segment` to `parent` with a `'.'`. An empty
/// parent yields the bare segment; an empty segment returns `parent` unchanged.
pub(crate) fn derive_sub_branch(parent: &str, segment: &str) -> String {
    if segment.is_empty() {
        return parent.to_owned();
    }
    if parent.is_empty() {
        return segment.to_owned();
    }
    format!("{parent}.{segment}")
}

/// Port of Go deriveChildBranch (branch.go:59): the branch for a dynamically-scheduled child.
/// base = `override_branch` if non-empty, else `parent`; `use_sub_branch` → `base.<name>@<run_id>`,
/// otherwise base unchanged.
pub(crate) fn derive_child_branch(
    parent: &str,
    name: &str,
    run_id: &str,
    use_sub_branch: bool,
    override_branch: &str,
) -> String {
    let base = if override_branch.is_empty() {
        parent
    } else {
        override_branch
    };
    if use_sub_branch {
        return derive_sub_branch(base, &format!("{name}@{run_id}"));
    }
    base.to_owned()
}

/// Port of Go commonBranchPrefix (branch.go:83): the longest dot-delimited prefix shared by all
/// input branches. Empty input or any empty branch → `""`. Segment-aware: `"a"` and `"ab"` share
/// no prefix.
pub(crate) fn common_branch_prefix(branches: &[String]) -> String {
    if branches.is_empty() {
        return String::new();
    }
    let mut splits: Vec<Vec<&str>> = Vec::with_capacity(branches.len());
    for b in branches {
        if b.is_empty() {
            return String::new();
        }
        splits.push(b.split('.').collect());
    }
    let min_len = splits.iter().map(Vec::len).min().unwrap_or(0);
    let first = &splits[0];
    let mut common = 0;
    while common < min_len {
        let seg = first[common];
        if splits[1..].iter().any(|s| s[common] != seg) {
            break;
        }
        common += 1;
    }
    first[..common].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_sub_branch_cases() {
        assert_eq!(derive_sub_branch("", "a@1"), "a@1");
        assert_eq!(derive_sub_branch("p", ""), "p");
        assert_eq!(derive_sub_branch("p", "a@1"), "p.a@1");
    }

    #[test]
    fn derive_child_branch_cases() {
        assert_eq!(derive_child_branch("p", "n", "2", true, "o"), "o.n@2");
        assert_eq!(derive_child_branch("p", "n", "2", false, ""), "p");
    }

    #[test]
    fn common_branch_prefix_cases() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        assert_eq!(common_branch_prefix(&[]), "");
        assert_eq!(common_branch_prefix(&s(&["a.b", ""])), "");
        assert_eq!(common_branch_prefix(&s(&["a.b.c", "a.b.d"])), "a.b");
        assert_eq!(common_branch_prefix(&s(&["a", "ab"])), "");
        assert_eq!(common_branch_prefix(&s(&["x.y", "x.y"])), "x.y");
    }
}
