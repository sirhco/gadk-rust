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

//! Ports of ADK-Go tests that need crate-internal access, one module per Go test file (plan
//! Appendix A). Each module's owning task fills it.

mod internal_ranksearch_bm25;
mod internal_ranksearch_ranksearch;
mod tool_exampletool_tool;
mod tool_skilltoolset_instruction;
mod tool_skilltoolset_internal_skilltool_tools;
mod tool_skilltoolset_skill_complete_preload;
mod tool_skilltoolset_skill_filesystem_source;
mod tool_skilltoolset_skill_frontmatter;
mod tool_skilltoolset_skill_frontmatter_preload;
mod tool_skilltoolset_skill_merged_source;
mod tool_toolsearch_toolsearch;
