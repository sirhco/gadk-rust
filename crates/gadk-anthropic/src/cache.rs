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

//! Prompt-cache breakpoints (port of ADK-Go `cache.go`).
//!
//! Placeholder signatures written by Task 1; Task 3 implements them.

use crate::config::PromptCacheConfig;
use crate::wire::{
    CacheControlEphemeral, ContentBlockParam, MessageParam, TextBlockParam, ToolParam,
};

/// Go `toCacheControlParam`: ephemeral with the configured TTL (5m unless 1h).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn to_cache_control_param(cfg: &PromptCacheConfig) -> CacheControlEphemeral {
    todo!("Task 3")
}

/// Go `applyCacheBreakpoints`, in place: with caching on, `cache_control` goes on the last
/// tool, the last cacheable message block and the last system block; without, nothing changes.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn apply_cache_breakpoints(
    cache: Option<&PromptCacheConfig>,
    system: &mut [TextBlockParam],
    messages: &mut [MessageParam],
    tools: &mut [ToolParam],
) {
    todo!("Task 3")
}

/// Go `markLastCacheableMessageBlock`: the last block, scanning messages and blocks from the
/// end, that is not thinking or redacted thinking.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn mark_last_cacheable_message_block(
    messages: &mut [MessageParam],
    cc: CacheControlEphemeral,
) {
    todo!("Task 3")
}

/// Go `setBlockCacheControl`: sets it on text, image, document, tool use and tool result
/// blocks; `false` for any other block.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn set_block_cache_control(
    block: &mut ContentBlockParam,
    cc: CacheControlEphemeral,
) -> bool {
    todo!("Task 3")
}
