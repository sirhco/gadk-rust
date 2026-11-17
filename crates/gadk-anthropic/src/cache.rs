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

//! Prompt-cache breakpoints.

use crate::config::PromptCacheConfig;
use crate::wire::{
    CacheControlEphemeral, ContentBlockParam, MessageParam, TextBlockParam, ToolParam,
};

/// The cache-control param: ephemeral with the configured TTL (5m unless 1h).
pub(crate) fn to_cache_control_param(cfg: &PromptCacheConfig) -> CacheControlEphemeral {
    CacheControlEphemeral::new(cfg.ttl)
}

/// Applies the cache breakpoints, in place: with caching on, `cache_control` goes on the last
/// tool, the last cacheable message block and the last system block; without, nothing changes.
pub(crate) fn apply_cache_breakpoints(
    cache: Option<&PromptCacheConfig>,
    system: &mut [TextBlockParam],
    messages: &mut [MessageParam],
    tools: &mut [ToolParam],
) {
    let Some(cache) = cache else {
        return;
    };
    let cc = to_cache_control_param(cache);

    if let Some(last) = tools.last_mut() {
        last.cache_control = Some(cc);
    }

    mark_last_cacheable_message_block(messages, cc);

    if let Some(last) = system.last_mut() {
        last.cache_control = Some(cc);
    }
}

/// Marks the last block, scanning messages and blocks from the
/// end, that is not thinking or redacted thinking.
pub(crate) fn mark_last_cacheable_message_block(
    messages: &mut [MessageParam],
    cc: CacheControlEphemeral,
) {
    for message in messages.iter_mut().rev() {
        for block in message.content.iter_mut().rev() {
            // Claude rejects cache_control on thinking and redacted_thinking blocks.
            if matches!(
                block,
                ContentBlockParam::Thinking(_) | ContentBlockParam::RedactedThinking(_)
            ) {
                continue;
            }
            if set_block_cache_control(block, cc) {
                return;
            }
        }
    }
}

/// Sets cache control on text, image, document, tool use and tool result
/// blocks; `false` for any other block.
pub(crate) fn set_block_cache_control(
    block: &mut ContentBlockParam,
    cc: CacheControlEphemeral,
) -> bool {
    match block {
        ContentBlockParam::Text(b) => b.cache_control = Some(cc),
        ContentBlockParam::Image(b) => b.cache_control = Some(cc),
        ContentBlockParam::Document(b) => b.cache_control = Some(cc),
        ContentBlockParam::ToolUse(b) => b.cache_control = Some(cc),
        ContentBlockParam::ToolResult(b) => b.cache_control = Some(cc),
        ContentBlockParam::Thinking(_) | ContentBlockParam::RedactedThinking(_) => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PromptCacheTtl;
    use crate::tools::convert_function_declaration;
    use crate::wire::{
        DocumentBlockParam, ImageBlockParam, RedactedThinkingBlockParam, Role, ThinkingBlockParam,
        ToolResultBlockParam, ToolResultTag, ToolUseBlockParam, to_go_json,
    };
    use gadk_genai::FunctionDeclaration;
    use pretty_assertions::assert_eq;
    use serde_json::Map;

    fn json_of<T: serde::Serialize>(v: &T) -> String {
        String::from_utf8(to_go_json(v).unwrap()).unwrap()
    }

    fn tool(name: &str) -> ToolParam {
        convert_function_declaration(&FunctionDeclaration {
            name: name.into(),
            ..Default::default()
        })
        .unwrap()
    }

    fn msg(role: Role, content: Vec<ContentBlockParam>) -> MessageParam {
        MessageParam { content, role }
    }

    fn thinking() -> ContentBlockParam {
        ContentBlockParam::Thinking(ThinkingBlockParam::new("sig_1", "Trailing thinking"))
    }

    fn tool_result() -> ContentBlockParam {
        ContentBlockParam::ToolResult(ToolResultBlockParam {
            tool_use_id: "toolu_1".into(),
            is_error: None,
            cache_control: None,
            content: Vec::new(),
            kind: ToolResultTag,
        })
    }

    #[test]
    fn cache_control_ttls() {
        // Go TestPromptCaching: "" and "5m" send 5m, "1h" sends 1h.
        let cases = [
            (
                PromptCacheConfig::default(),
                r#"{"ttl":"5m","type":"ephemeral"}"#,
            ),
            (
                PromptCacheConfig {
                    ttl: PromptCacheTtl::FiveMinutes,
                },
                r#"{"ttl":"5m","type":"ephemeral"}"#,
            ),
            (
                PromptCacheConfig {
                    ttl: PromptCacheTtl::OneHour,
                },
                r#"{"ttl":"1h","type":"ephemeral"}"#,
            ),
        ];
        for (cfg, want) in cases {
            assert_eq!(json_of(&to_cache_control_param(&cfg)), want);
        }
    }

    #[test]
    fn breakpoints_follow_go_placement() {
        // Go TestPromptCaching: last tool, system block, last non-thinking message block.
        for ttl in [PromptCacheTtl::FiveMinutes, PromptCacheTtl::OneHour] {
            let cfg = PromptCacheConfig { ttl };
            let cc = CacheControlEphemeral::new(ttl);
            let mut system = vec![TextBlockParam::new("System prompt")];
            let mut messages = vec![
                msg(
                    Role::User,
                    vec![ContentBlockParam::text("First user message")],
                ),
                msg(
                    Role::Assistant,
                    vec![
                        thinking(),
                        ContentBlockParam::text("Assistant text before thinking"),
                    ],
                ),
            ];
            let mut tools = vec![tool("tool_a"), tool("tool_b")];

            apply_cache_breakpoints(Some(&cfg), &mut system, &mut messages, &mut tools);

            assert_eq!(tools[0].cache_control, None);
            assert_eq!(tools[1].cache_control, Some(cc));
            assert_eq!(system[0].cache_control, Some(cc));
            let mut want_text = TextBlockParam::new("Assistant text before thinking");
            want_text.cache_control = Some(cc);
            assert_eq!(
                messages[1].content,
                vec![thinking(), ContentBlockParam::Text(want_text)]
            );
            assert_eq!(
                messages[0].content,
                vec![ContentBlockParam::text("First user message")]
            );
        }
    }

    #[test]
    fn breakpoint_on_last_block_only() {
        let cfg = PromptCacheConfig::default();
        let mut system = vec![TextBlockParam::new("a"), TextBlockParam::new("b")];
        let mut messages = vec![msg(
            Role::User,
            vec![ContentBlockParam::text("x"), ContentBlockParam::text("y")],
        )];
        apply_cache_breakpoints(Some(&cfg), &mut system, &mut messages, &mut []);
        assert_eq!(system[0].cache_control, None);
        assert!(system[1].cache_control.is_some());
        assert_eq!(
            json_of(&messages),
            r#"[{"content":[{"text":"x","type":"text"},{"text":"y","cache_control":{"ttl":"5m","type":"ephemeral"},"type":"text"}],"role":"user"}]"#
        );
    }

    #[test]
    fn caching_off_changes_nothing() {
        let mut system = vec![TextBlockParam::new("System prompt")];
        let mut messages = vec![msg(Role::User, vec![ContentBlockParam::text("hi")])];
        let mut tools = vec![tool("tool_a")];
        let (s0, m0, t0) = (system.clone(), messages.clone(), tools.clone());
        apply_cache_breakpoints(None, &mut system, &mut messages, &mut tools);
        assert_eq!((system, messages, tools), (s0, m0, t0));
    }

    #[test]
    fn empty_inputs_are_fine() {
        let mut system: Vec<TextBlockParam> = Vec::new();
        let mut messages: Vec<MessageParam> = Vec::new();
        let mut tools: Vec<ToolParam> = Vec::new();
        apply_cache_breakpoints(
            Some(&PromptCacheConfig::default()),
            &mut system,
            &mut messages,
            &mut tools,
        );
        assert!(system.is_empty() && messages.is_empty() && tools.is_empty());
        // A system prompt alone still gets its breakpoint when the other inputs are empty.
        let mut system = vec![TextBlockParam::new("System prompt")];
        apply_cache_breakpoints(
            Some(&PromptCacheConfig::default()),
            &mut system,
            &mut messages,
            &mut tools,
        );
        assert!(system[0].cache_control.is_some(), "{system:?}");
        assert!(messages.is_empty() && tools.is_empty());
    }

    #[test]
    fn thinking_only_last_message_marks_an_earlier_message() {
        let cc = CacheControlEphemeral::new(PromptCacheTtl::FiveMinutes);
        let mut messages = vec![
            msg(Role::User, vec![tool_result()]),
            msg(
                Role::Assistant,
                vec![
                    thinking(),
                    ContentBlockParam::RedactedThinking(RedactedThinkingBlockParam::new("blob")),
                ],
            ),
        ];
        mark_last_cacheable_message_block(&mut messages, cc);
        match &messages[0].content[0] {
            ContentBlockParam::ToolResult(b) => assert_eq!(b.cache_control, Some(cc)),
            other => panic!("unexpected block {other:?}"),
        }
        assert_eq!(
            messages[1].content,
            vec![
                thinking(),
                ContentBlockParam::RedactedThinking(RedactedThinkingBlockParam::new("blob"))
            ]
        );
    }

    #[test]
    fn set_block_cache_control_per_kind() {
        let cc = CacheControlEphemeral::new(PromptCacheTtl::OneHour);
        let mut cacheable = vec![
            ContentBlockParam::text("t"),
            ContentBlockParam::Image(ImageBlockParam::url("https://example.com/a.png")),
            ContentBlockParam::Document(DocumentBlockParam::plain_text("doc")),
            ContentBlockParam::ToolUse(ToolUseBlockParam::new("toolu_1", Map::new(), "fn")),
            tool_result(),
        ];
        for block in &mut cacheable {
            assert!(set_block_cache_control(block, cc), "{block:?}");
            let json = json_of(block);
            assert!(
                json.contains(r#""cache_control":{"ttl":"1h","type":"ephemeral"}"#),
                "{json}"
            );
        }
        let mut not_cacheable = vec![
            thinking(),
            ContentBlockParam::RedactedThinking(RedactedThinkingBlockParam::new("blob")),
        ];
        for block in &mut not_cacheable {
            let before = block.clone();
            assert!(!set_block_cache_control(block, cc));
            assert_eq!(*block, before);
        }
    }
}
