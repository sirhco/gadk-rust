# gadk-prompts

Versioned, labelled prompts for gadk agents. A `PromptStore` keeps immutable prompt versions
(template, optional JSON config, content hash, message and author) and movable labels such as
`production` and `staging`; `FileStore` keeps each prompt in a YAML file
(`<root>/<name>.yaml`, a `/` in the name being a subdirectory). A `PromptRegistry` serves
prompts from an in-memory snapshot that it refreshes by polling the store, and a
`PromptInstruction` renders one as an agent's instruction provider with session-state
templating (`{var}`, `{var?}`, `{artifact.x}`). Each render records a `render_prompt <name>`
span, and the registry exports refresh metrics. Re-exported by the `gadk` façade as
`gadk::prompts` (feature `prompts`, not in `default`).

```rust,ignore
use std::sync::Arc;
use gadk_prompts::{FileStore, Label, PromptRegistry, Selector};

let registry = PromptRegistry::builder(Arc::new(FileStore::new("prompts"))).load().await?;
let provider = registry
    .instruction("support-triage", Selector::Label(Label::production()))
    .with_fallback("You are a helpful support agent.")
    .into_provider();
// Use `provider` as the agent's instruction provider.
```

- `Selector` picks a version by label (`Selector::Label`), number (`Selector::Version`) or
  `Selector::Latest`.
- `PromptRegistryBuilder::refresh_interval` sets the polling interval
  (`DEFAULT_REFRESH_INTERVAL` by default, `Duration::ZERO` to disable background refresh);
  `meter_provider` sets where the refresh metrics go.
- Feature `testsuite`: `gadk_prompts::testsuite::run`, the `PromptStore` contract tests, for
  other store implementations.

Other stores: `gadk-prompts-firestore` (Google Cloud Firestore). Command-line management:
`gadk-prompts-cli`.
