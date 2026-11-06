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

//! Port of Go workflow/branch_test.go.

use crate::workflow::branch::{common_branch_prefix, derive_sub_branch};

#[rstest::rstest]
#[case::root_and_segment("", "child@1", "child@1")]
#[case::parent_and_segment("wf", "child@1", "wf.child@1")]
#[case::nested_parent("wf.outer@1", "child@2", "wf.outer@1.child@2")]
#[case::empty_segment_noop("wf", "", "wf")]
#[case::empty_both("", "", "")]
fn test_derive_sub_branch(#[case] parent: &str, #[case] segment: &str, #[case] want: &str) {
    assert_eq!(
        derive_sub_branch(parent, segment),
        want,
        "derive_sub_branch({parent:?}, {segment:?})"
    );
}

#[rstest::rstest]
#[case::empty_input(&[], "")]
#[case::single_branch(&["a.b.c"], "a.b.c")]
#[case::single_empty_branch(&[""], "")]
#[case::any_empty_short_circuits_to_root(&["a.b", "", "a.b.c"], "")]
#[case::identical_branches(&["a.b", "a.b", "a.b"], "a.b")]
#[case::shared_prefix_one_segment(&["a.b", "a.c"], "a")]
#[case::shared_prefix_two_segments(&["a.b.c", "a.b.d"], "a.b")]
#[case::no_common_prefix(&["a", "b"], "")]
#[case::string_prefix_but_no_segment_prefix(&["a", "ab"], "")]
#[case::mixed_depth(&["a.b", "a.b.c.d"], "a.b")]
#[case::three_branches_two_common(&["a.b.x", "a.b.y", "a.b.z.w"], "a.b")]
#[case::first_segment_differs(&["a.b.c", "x.b.c"], "")]
fn test_common_branch_prefix(#[case] branches: &[&str], #[case] want: &str) {
    let owned: Vec<String> = branches.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        common_branch_prefix(&owned),
        want,
        "common_branch_prefix({branches:?})"
    );
}
