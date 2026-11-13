# gadk-configurable

Internal crate (`publish = false`): a port of ADK-Go's `internal/configurable` (YAML-configured
agents), its conformance package (`internal/configurable/conformance`: the record and replay
plugins, callbacks, function tools, node function and agent loader) and the `adkcli` binary
(`cmd/internal/adkcli`), sub-project 6c-2. It exists so gadk can run ADK's cross-language
conformance cases: record a run into `generated-recordings.yaml` byte-identical to ADK-Go's, and
replay a recording against a live agent. No other crate depends on it and the `gadk` façade never
re-exports it.

- `Registry` holds Go's global registries (agent classes, tool and toolset factories, callbacks,
  node functions, and the agent and node caches); `Registry::new()` is Go's `init()` set.
  `Registry::from_config(path)` loads an agent YAML file (`LlmAgent`, `LoopAgent`,
  `ParallelAgent`, `SequentialAgent`, `Workflow`) with Go's defaults, validation and error texts.
- Go-faithful modules, Go's function names in snake_case: `lib.rs` (the registry of
  `configurable_utils.go`), `agents.rs` and `config.rs` (`configurable.go`), `builtin_tools.rs`,
  `workflow.rs` (`configurable_workflow.go`), `conformance/{loader,callbacks,functions,nodes}.rs`,
  `conformance/record/`, `conformance/replay/` and `conformance/recording.rs`.
- `conformance/gonode/` builds the yaml.v3 node tree Go's `node.Encode(&recordings)` builds from
  gadk's serde types (through `gadk-goyaml`), with the type-path fixups and Go struct shapes the
  probe generates (`gonode/shapes.rs` is generated, see below), and decodes it back.
- Behavior, file bytes, error texts and stdout match ADK-Go except for the accepted divergences
  in `docs/parity/6c-2.md`, each annotated `// Divergence (ledger 6c-2)` in the source.

## adkcli

```
cargo run -p gadk-configurable --bin adkcli -- [console|web …] [flags]
```

Run it from a directory tree of conformance cases: it registers the conformance content, loads
every `root_agent.yaml` below the working directory (the folder name is the agent's key; failing
and duplicate agents are skipped with Go's status lines) and runs the full launcher (as
`gadk_launcher::full`) with the replay and then the record plugin. Session state
`_adk_replay_config` / `_adk_recordings_config` (`dir`, `user_message_index`, and for recording
`streaming_mode`) switches a run into replay or record mode; `dir` must lie inside the working
directory.

## Tests and goldens

- Unit tests per module and ports of the 7 Go test files (19 tests) in `src/parity_tests/`
  (ledger `docs/parity/6c-2.md`, checked by `python3 scripts/parity-check --ledger 6c-2`).
- `tests/recordings_differential.rs` replays the Go probe's corpus in `tests/fixtures/`
  (44 recording cases: record bytes, replay decoding or Go's decode error, round trip; the fixup
  table and shapes against `fixups.json`; `canonicalize.json` for replay matching).
- `tests/plugins_integration.rs` runs both plugins under a runner against Go-written goldens in
  `tests/fixtures/integration/`; `tests/adkcli.rs` checks the binary's stdout, stderr and exit
  codes against texts captured from Go's `adkcli`, and a REST-mode replay.
- The fixtures are written by the Go probe `scripts/gen-go-configurable` (its `SCENARIO.md`
  documents the corpus and format). Regenerate with `cd scripts/gen-go-configurable && go run .`
  (from the main checkout, so `../../../adk-go` is ADK-Go), then regenerate `gonode/shapes.rs`
  with `GADK_GONODE_REGEN=1 cargo test -p gadk-configurable --test recordings_differential
  shapes_rs_is_generated_from_fixture` and update the counts in `scripts/check-fixtures.sh` if cases were
  added. `ADK_GO_DIR=… scripts/check-fixtures.sh` asserts the counts and, when `go` is on `PATH`,
  reruns the probe (output must be identical) and diffs the probe's vendored ADK-Go declarations
  against ADK-Go's.

## License

Apache-2.0.
