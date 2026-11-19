# gadk-tools

Built-in gadk tools that need no authentication, ready to put in an LLM agent's `tools` or
`toolsets`:

| Module | What it gives the agent |
|---|---|
| `gemini` | Gemini's native tools: `GoogleSearch`, `GoogleMaps`, `UrlContext`, `EnterpriseWebSearch`, and `gemini::new` for any other `genai::Tool` (a retrieval tool, for example); each adds itself to the request config after checking the model |
| `load_artifacts` | Tells the model which artifacts the session has; when it calls `load_artifacts`, the next request receives their content |
| `load_memory` | A `load_memory` function the model calls to search the memory service |
| `preload_memory` | Searches memory with the user's query on every model request and puts the matches into the system instruction (the model never calls it) |
| `example` | Few-shot examples added to the system instruction, from a static list or a `Provider` queried per user message |
| `toolsearch` | A gating toolset: only core tools are visible until the model calls `search_tools` (BM25, regex or exact `select:`) to reveal others |
| `skills` | `SkillToolset`: `list_skills`, `load_skill` and `load_skill_resource` over skill folders (`SKILL.md` plus resources) from a directory, memory or merged sources |

The auth wire model lives in `gadk-session` and is re-exported here as `toolauth`. The function,
agent and exit-loop tools are in `gadk-engine` (`gadk_engine::tool`). Most applications use
this crate through the `gadk` façade (`gadk::tools`).

## Example

```rust
use std::sync::Arc;
use gadk_agent::Tool;
use gadk_tools::skills::{SkillToolset, SkillToolsetConfig, skill};
use gadk_tools::{gemini::GoogleSearch, load_memory};

// Inside an async fn returning Result.
let search: Arc<dyn Tool> = Arc::new(GoogleSearch);
let memory = load_memory::new();

let fs = Arc::new(skill::DirFs::new("./skills"));
let source: Arc<dyn skill::Source> = Arc::new(skill::FileSystemSource::new(fs));
let (source, _reload) = skill::with_complete_preload(source).await?;
let skills = SkillToolset::new(SkillToolsetConfig { source, name: None, system_instruction: None });
// LlmAgentConfig { tools: vec![memory], toolsets: vec![skills], .. }
```

Gemini does not accept a native tool and function tools in one request: give `GoogleSearch` an
agent of its own and call that agent through an `AgentTool`, as the `gadk` crate's
`multiple_tools` example does.

## Notes

- `toolsearch` decides which tools the model sees, not which a caller may use: pass a base
  toolset already filtered for the caller. An agent can use at most one gating toolset, and its
  base tools should reach the agent only through it.
