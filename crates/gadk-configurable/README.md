# gadk-configurable

Internal crate (`publish = false`): YAML-configured agents (the `configurable` loader), the
conformance support (the record and replay plugins, callbacks, function tools, node function and
agent loader) and the `adkcli` binary. It exists so gadk can run ADK's cross-language
conformance cases: record a run into `generated-recordings.yaml` byte-identical to ADK's, and
replay a recording against a live agent. No other crate depends on it and the `gadk` façade never
re-exports it.

- `Registry` holds the shared registries (agent classes, tool and toolset factories, callbacks,
  node functions, and the agent and node caches); `Registry::new()` registers the built-in set.
  `Registry::from_config(path)` loads an agent YAML file (`LlmAgent`, `LoopAgent`,
  `ParallelAgent`, `SequentialAgent`, `Workflow`) with ADK's defaults, validation and error texts.
- Modules: `lib.rs` (the registry), `agents.rs` and `config.rs` (agent configs and factories),
  `builtin_tools.rs`, `workflow.rs`, `conformance/{loader,callbacks,functions,nodes}.rs`,
  `conformance/record/`, `conformance/replay/` and `conformance/recording.rs`.
- `conformance/gonode/` builds the yaml.v3 node tree ADK's recorder builds when it encodes its
  recordings, from gadk's serde types (through `gadk-goyaml`), with the type-path fixups and
  struct shapes generated from a fixture (`gonode/shapes.rs` is generated, see below), and
  decodes it back.
- Behavior, file bytes, error texts and stdout match ADK. The few places where the Rust
  implementation necessarily differs are noted in the source comments next to the code.

## adkcli

```
cargo run -p gadk-configurable --bin adkcli -- [console|web …] [flags]
```

Run it from a directory tree of conformance cases: it registers the conformance content, loads
every `root_agent.yaml` below the working directory (the folder name is the agent's key; failing
and duplicate agents are skipped with ADK's status lines) and runs the full launcher (as
`gadk_launcher::full`) with the replay and then the record plugin. Session state
`_adk_replay_config` / `_adk_recordings_config` (`dir`, `user_message_index`, and for recording
`streaming_mode`) switches a run into replay or record mode; `dir` must lie inside the working
directory.

## Tests and goldens

- Unit tests per module and the loader's own test files (19 tests) in `src/parity_tests/`.
- `tests/recordings_differential.rs` replays the recorded corpus in `tests/fixtures/`
  (44 recording cases: record bytes, replay decoding or the decode error, round trip; the fixup
  table and shapes against `fixups.json`; `canonicalize.json` for replay matching).
- `tests/plugins_integration.rs` runs both plugins under a runner against goldens written by ADK
  in `tests/fixtures/integration/`; `tests/adkcli.rs` checks the binary's stdout, stderr and exit
  codes against texts captured from ADK's `adkcli`, and a REST-mode replay.
- The fixtures were recorded from ADK itself; they are committed and not regenerated.
  `gonode/shapes.rs` is generated from `tests/fixtures/fixups.json`: regenerate
  it with `GADK_GONODE_REGEN=1 cargo test -p gadk-configurable --test recordings_differential
  shapes_rs_is_generated_from_fixture`.

## License

Apache-2.0.
