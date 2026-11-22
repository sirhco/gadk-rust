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
  `meter_provider` sets where the refresh metrics go. Without it the instruments bind to the
  global meter provider at `load()` time, so build the registry after installing telemetry
  providers, or pass `meter_provider`.
- Feature `testsuite`: `gadk_prompts::testsuite::run` and `testsuite::run_rollouts`, the
  `PromptStore` contract tests, for other store implementations.

## Prompt metrics

Each render records the version it used in the invocation's context
(`ReadonlyContext::record_prompt`, keyed by the agent's name): a resolved version records
`PromptUse { name, version, label }`, with `label` the selector's label, `latest` for
`Selector::Latest`, or none for `Selector::Version`; a fallback render clears the agent's record.
When an agent renders two `PromptInstruction`s (instruction and global instruction), the last
render before the model call wins. The request sent to the model is unchanged. The
`gadk-agentanalytics` plugin writes the record into its model rows as `attributes.prompt`, and
`gadk-console` shows calls, tokens, cost, latency and eval pass rate per version on
`/prompts/{name}`.

## Rollouts

A rollout splits a label's traffic between the label's version and one other version: with
`production` at v3 and a rollout of v4 at 10%, about 10% of sessions are served v4 and the rest
v3, and each session stays on its side. Watch the two versions side by side in the console's
per-version metrics and feedback, then promote or abort without a redeploy.

```rust,ignore
use gadk_prompts::{Label, Rollout};

store.set_rollout("support-triage", &Label::production(), Some(Rollout::new(4, 10)?)).await?;
// Abort: remove the rollout; every session is served the label's version again.
store.set_rollout("support-triage", &Label::production(), None).await?;
```

- `Rollout::new(version, percent)` needs `version >= 1` and `1 <= percent <= 99`; the version
  must exist and differ from the label's. One rollout per label.
- Bucketing: the key is the session id (`ReadonlyContext::session_id`), and
  `bucket = u64::from_be_bytes(first 8 bytes of SHA-256(name || 0x00 || label || 0x00 || key)) % 100`;
  the rollout version is served when `bucket < percent`. The same session always gets the same
  version, and raising the percent keeps every session already on the rollout version there
  (it only moves more sessions onto it).
- Only label selectors use rollouts: `Selector::Version` and `Selector::Latest` ignore them, and
  so does `PromptRegistry::get`. `PromptRegistry::get_for(name, &selector, key)` applies them,
  and `PromptInstruction` renders with `get_for` and the session id, recording the version it
  served in `PromptUse` (so per-version metrics split the two sides) and `prompt.rollout` on the
  `render_prompt` span. `PromptRegistry::rollouts()` returns the rollouts loaded with the
  current snapshot.
- Promote: move the label to the rollout version (`set_label`); moving or removing a label
  clears its rollout in the same write, while `create_version` keeps rollouts. From the command
  line: `gadk-prompts rollout <name> --label <l> <version> --percent <p>` starts or changes a
  rollout, `--promote` moves the label to the rollout version (compare-and-set on the label
  version it read) and `--abort` removes it. The console's prompt page has the same actions.
- `FileStore` keeps rollouts in the prompt's file (a `rollouts` key, written only when there is
  one) and `gadk-prompts-firestore` in the prompt document. A store that does not support
  rollouts answers `set_rollout` with `Invalid("rollouts are not supported by this store")`.
- A stored rollout outside the bounds (e.g. a hand-edited percent of 0 or 100) makes that prompt
  fail to load with a `Decode` error, and the store's writes to that prompt fail too. To fix it,
  correct or remove the `rollouts` entry by hand in the prompt's YAML file or Firestore document.
- Mixed versions: a writer built with gadk 0.5.2 or older (CLI, console or service) rewrites a
  prompt's file or document without its rollouts, so changing that prompt with it silently
  ends them (all sessions go back to the label's version). Upgrade every writer before starting
  rollouts.

Other stores: `gadk-prompts-firestore` (Google Cloud Firestore). Command-line management:
`gadk-prompts-cli`.
