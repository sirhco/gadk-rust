# gen-go-8b

`scripts/gen-go-8b` records what ADK-Go (at `docs/parity/ADK_GO_COMMIT`) sends and returns for the
two 8b wire shapes gadk talks over REST (spec Design E, R10):

- the Vertex AI Example Store provider (`tool/exampletool/vertexai`): the `SearchExamplesRequest`
  Go builds and the examples Go maps from each `TestProvider_Examples` response, as goldens for
  `gadk-vertexai`;
- the Agent Engine deploy environment (`cmd/adkgo/internal/deploy/agentengine`): the
  `CreateReasoningEngineRequest` / `UpdateReasoningEngineRequest` Go sends for each
  `TestEnvironmentDeploymentRequests` scenario, as goldens for `gadk-cli`.

It is two overlay tests, not a Go module: `run.sh` maps `overlay/vertexai_zz_gen8b_test.go.txt`
and `overlay/agentengine_zz_gen8b_test.go.txt` into those two ADK-Go packages as
`zz_gen8b_test.go` with `go test -overlay` (in-package, so unexported functions and the packages'
own test helpers are callable), using ADK-Go's own `go.mod` / `go.sum` with `-mod=readonly`, and
runs `go test -run '^TestZZGen8b(ExampleStore|Environment)$'` with `GEN_GO_8B_OUT` set. ADK-Go's
tree is never written.

Regenerate (from the repo root):

```
scripts/gen-go-8b/run.sh            # replaces both golden directories below
scripts/gen-go-8b/run.sh OUT_DIR    # writes OUT_DIR/example_store and OUT_DIR/environment instead
```

It prints `wrote 16 example_store and 14 environment goldens`. ADK-Go is `$ADK_GO_DIR`, else
`adk-go` beside the main checkout (the parent of git's common dir, so it works from a git
worktree), else `../adk-go`. The Go toolchain is `$GADK_PROBE_GOTOOLCHAIN` (default `go1.26.10`,
fetched by the go command's `GOTOOLCHAIN` support on first use); the output is byte-identical under
go1.26.10 and go1.27.2. No clock reading, randomness or Go map order reaches a golden, so
`scripts/check-fixtures.sh` reruns the probe into a temp dir and requires an empty `diff -r`, and
asserts the counts (example_store 16, environment 14).

## Where the inputs come from

- **Example Store.** The 16 cases are the 16 `t.Run` subtests of `TestProvider_Examples` in
  `vertexai_test.go`, with their query and fake-client response (or search error) transcribed
  verbatim into the overlay, built with the test file's own `makeResult` / `makeExpectedContent`.
  The probe parses `vertexai_test.go` (`go/ast`) and fails unless its case names equal the
  subtest names, in order. Each case runs Go's `newProvider` with the package's
  `fakeExampleStoreClient`, which captures the request.
- **Environment.** The 14 scenarios (`name`, `args`, `update`) are read from the `tests` table of
  `TestEnvironmentDeploymentRequests` in `environment_test.go` with `go/ast`, not transcribed. Each
  runs exactly as that test does: `resetFlags(t, "not-a-go-file")`, project `test-project`, region
  `test-region`, the real cobra command (`environmentCommand()`, which sets `updateEnv` from
  `Changed`) up to its bounded entry-point error, `computeFlags()` with entry point `main.go`, the
  source archive `test archive`, and `gcloudDeployToAgentEngine` or (agent engine ID
  `test-engine`) `gcloudUpdateAgentEngine` against the package's loopback gRPC server
  (`environmentDeploymentServer` / `environmentClientOptions`), which captures the request.

## Golden formats

Every golden is one JSON document, written by Go `encoding/json` (2-space indent, no HTML
escaping, trailing newline). Embedded protobuf messages are Go `protojson` output with its
whitespace normalized (`json.Compact`, then re-indented); their key order is protojson's (proto
field order), the other keys are in struct order. The file stem is the subtest/scenario name in
lower case with every run of other characters replaced by one `_` (e.g.
`converts file data, video metadata, …` → `converts_file_data_video_metadata_…`).

### `crates/gadk-vertexai/testdata/example_store/<case>.json` (16)

| key | presence | content |
| --- | --- | --- |
| `name` | always | the `TestProvider_Examples` subtest name |
| `query` | always | the query passed to `Examples` |
| `request` | always | `protojson.Marshal` of the `SearchExamplesRequest` Go sent (default options: lowerCamelCase names; `topK` is an int64, so it is the string `"10"`) |
| `response` | unless `searchError` | `protojson.MarshalOptions{UseEnumNumbers: true}` of the fake response, as the REST wire (`$alt=json;enum-encoding=int`) carries it: `language` / `outcome` are integers, bytes are base64, durations are `"5s"`; a `nil` element of a repeated message field is `{}` |
| `responseIsNil` | only `tolerates_unset_results` | `true`: Go's fake returned a nil response; `response` is then `{}`, the body a REST server would send |
| `searchError` | only `propagates_search_error` | the fake client's error text (`rpc failed`) |
| `examples` | unless `error` | `json.Marshal` of the `[]*exampletool.Example` Go returned (`{"input": Content, "output": [Content]}`, genai JSON: enum names such as `"PYTHON"` / `"OUTCOME_OK"`, base64 bytes, `"5s"` durations, empty part/content fields omitted, `output` always present and `[]` when empty); `[]` when there are no examples |
| `error` | only `propagates_search_error` | Go's error text (`vertexai: search examples: rpc failed`) |

### `crates/gadk-cli/testdata/environment/<scenario>.json` (14)

| key | presence | content |
| --- | --- | --- |
| `name` | always | the `TestEnvironmentDeploymentRequests` scenario name |
| `args` | always | the command-line arguments (`[]` when none) |
| `update` | always | `true` for an update (`--agent_engine_id test-engine`), `false` for a create |
| `projectName`, `region` | always | `test-project`, `test-region` |
| `agentEngineId` | update only | `test-engine` |
| `request` | always | `protojson.Marshal` of the captured `CreateReasoningEngineRequest` (`parent`, `reasoningEngine`) or `UpdateReasoningEngineRequest` (`reasoningEngine` with `name`, and `updateMask` in protojson form: one comma-joined lowerCamelCase string such as `"spec.sourceCodeSpec,spec.classMethods,spec.deploymentSpec.env"`) |

`request.reasoningEngine.spec` holds `sourceCodeSpec` (`inlineSource.sourceArchive` is base64 of
`test archive`, plus `imageSpec: {}`), `classMethods` (Go's `agentengine.ListClassMethods()`),
`agentFramework` (create only) and `deploymentSpec`. An empty environment is
`"deploymentSpec": {}` (protojson omits an empty `env`); an update without `--env` /
`--default_env` has no `deploymentSpec` and no `spec.deploymentSpec.env` mask path. The create
request's default display name is Go's clock (`ADK Agent: <RFC3339 now>`), recorded as
`ADK Agent: {{RFC3339}}`; nothing else is rewritten.

## After `ADK_GO_COMMIT` moves

Rerun `run.sh`. If a `TestProvider_Examples` subtest was added or renamed, the probe fails with the
name difference; a subtest whose input changed under the same name is not detected, so review the
Go diff of `vertexai_test.go` and update the transcribed case in
`overlay/vertexai_zz_gen8b_test.go.txt`.
Scenario changes in `environment_test.go` are picked up automatically. Update the counts in this
README and in `scripts/check-fixtures.sh`.
