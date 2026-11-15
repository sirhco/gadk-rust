# gen-go-configurable scenario

`scripts/gen-go-configurable` records how ADK-Go's conformance record and replay plugins
(`internal/configurable/conformance/{recordplugin,replayplugin}`) write and read recordings
files, as fixtures for `gadk-configurable` (sub-project 6c-2, spec R8, R10, R15): a recording
corpus, the reflection-derived type-path fixup table and Go struct shapes (with a self-check that
rebuilds every corpus case from the shapes alone), jsonschema-go v0.4.3's `Schema` field list,
and a `canonicalizeJSONSubstrings` table.

Regenerate (from the main checkout, so `../../../adk-go` is ADK-Go; elsewhere pass
`-modfile` with an absolute `replace`, as `scripts/check-fixtures.sh` does):

```
cd scripts/gen-go-configurable && go run .
```

It prints
`wrote recordings=44 (replay errors 2) fixups=54 jsonschema_fields=65 shapes=120 shape_paths=218 (self-check 44/44 go numbers, 43/44 json numbers) canonicalize=40 to ../../crates/gadk-configurable/tests/fixtures`.
Flags: `-fixtures DIR` (default `../../crates/gadk-configurable/tests/fixtures`; deletes and
rewrites `DIR/recordings/`, rewrites `DIR/fixups.json` and `DIR/canonicalize.json`);
`-vendored adk|probe` prints the vendored declarations (below) from ADK-Go (`$ADK_GO_DIR`, else
`../../../adk-go`) or from the probe's own copies, and exits. The run sets `TZ=UTC` and uses no
clock, network or host state (temp dirs are created and removed, their paths never reach an
output), so every output is byte-identical run to run; `scripts/check-fixtures.sh` reruns it into
a temp dir and requires an empty `diff -r`, and diffs `-vendored adk` against `-vendored probe`.
The outputs are identical under the go1.26.10 and go1.27.2 toolchains (module line
`go 1.26.10`, like the sibling probes); they depend on `encoding/json`, `gopkg.in/yaml.v3` v3.0.1, `google.golang.org/genai` v1.77.0
and `github.com/google/jsonschema-go` v0.4.3 (all pinned by `go.sum`).

## Vendored code

Go forbids importing another module's `internal/` packages, so the probe copies these ADK-Go
declarations verbatim (text from the doc comment to the end of the declaration, byte for byte);
`vendorcheck.go`'s `vendoredDecls` is the manifest and `-vendored` the extractor (`go/parser`):

| ADK-Go file (under `internal/configurable/conformance/`) | Probe file | Declarations |
|---|---|---|
| `replayplugin/recording/recording.go` | `recording/recording.go` | `Recordings`, `Recording`, `LLMRecording`, `ToolRecording` |
| `recordplugin/record_plugin.go` | `vendor_record.go` | `recordPlugin`, `(*recordPlugin).saveRecordings`, `shouldPruneYAMLField`, `snakeCaseFieldMap`, `init`, `findKeyNode`, `cleanYAMLNode`, `injectSchemaTitles`, `toCamelCaseTitle`, `sanitizeLLMRequest` |
| `recordplugin/invocation_record_state.go` | `vendor_record.go` | `invocationRecordState`, `newInvocationRecordState` |
| `replayplugin/yaml_utils.go` | `vendor_replay.go` | `toIgnore`, `toIgnoreButRecurse`, `normalizeYAMLNode` |
| `replayplugin/replay_plugin.go` | `vendor_replay.go` | the `var (…)` block of `dataBlockRegex`, `jsonCandidateRegex`, `quoteRegex`, `nullRegex`, `boolRegex`; `modifyString`, `canonicalizeJSONSubstrings` |
| `functions.go` | `vendor_functions.go` | the 6 arg structs (`ValidateEmailArgs` … `askForApprovalArgs`, plus `FlightPreferences`, `TripDetails`), `emailRegex`, the 7 tool functions, and `RegisterFunctions`' leading statements (the 7 `functiontool.New` calls and their `if err != nil` checks, up to the first statement mentioning `configurable.`) as the body of the probe's `newConformanceTools` (up to its first statement mentioning `conformanceTools`) |

## Outputs

All JSON: 2-space indent, no HTML escaping, trailing newline (`json.Encoder` with
`SetEscapeHTML(false)`, `SetIndent("", "  ")`); maps with sorted keys (Go `encoding/json`).

- `crates/gadk-configurable/tests/fixtures/recordings/<case>/` — 44 case dirs (below), each
  holding `input.json`, the Go record bytes as `generated-recordings.yaml` (or
  `generated-recordings-sse.yaml` when `streaming_mode` is `sse`: 1 case), `replay.json`, and,
  for the 6 merge cases, `existing.yaml`.
- `crates/gadk-configurable/tests/fixtures/fixups.json` — 54 fixups, 65 jsonschema fields, 120
  shapes, 218 shape paths, the shapes self-check result.
- `crates/gadk-configurable/tests/fixtures/canonicalize.json` — 40 rows.

### input.json

```
{"case": "<name>", "streaming_mode": "none"|"sse", "user_message_index": <int>,
 "existing": null|"existing.yaml", "recordings": [<recording>…]}
```

`user_message_index` is the invocation's (the record state's) index, `existing` names the file
in the same dir that already holds the recordings file when `saveRecordings` runs. A
`<recording>` is

```
{"user_message_index": <int>, "agent_name": "<s>",
 "llm_recording": {"llm_request": {"model": "<s>", "contents": [Content…], "config": GenerateContentConfig},
                   "llm_responses": [LLMResponse…]},
 "tool_recording": {"tool_call": FunctionCall, "tool_response": FunctionResponse}}
```

with absent keys meaning Go nil (`llm_recording`/`tool_recording`/`llm_request`/`tool_call`/
`tool_response` pointers, `contents`/`config`/`llm_responses`). `Content`,
`GenerateContentConfig`, `FunctionCall`, `FunctionResponse` are genai JSON and `LLMResponse` is
`model.LLMResponse`'s JSON — the shapes gadk-genai's `Content`, `GenerateContentConfig`,
`FunctionCall`, `FunctionResponse` and gadk-model's `LlmResponse` deserialize (every input and
every replay recording deserializes into them; nothing but the zero `startOffset: "0s"` and
`config.httpOptions` — which gadk keeps on `LlmRequest.http_options`, not in the config, and
which `sanitizeLLMRequest` drops anyway — is lost). `llm_request` is `model.LLMRequest`'s
`Model`, `Contents`, `Config` (the type has no JSON tags; `Tools` is `json:"-"`).

The probe builds the Go recordings from `recordings` with `encoding/json`
(`DisallowUnknownFields` at the top level), then converts every
`config.tools[].functionDeclarations[].parametersJsonSchema`/`responseJsonSchema` into a
`*jsonschema.Schema` (marshal, `Schema.UnmarshalJSON`: `true`/`{}` → the empty schema, `false` →
`{"not": {}}`, `"type": [..]` → `Types`, `"items"` → `Items`), as Go function tools store them,
and passes every `llm_request` through the vendored `sanitizeLLMRequest` (as the record plugin's
`beforeModel` does: `Tools` nil, `Config.HTTPOptions` nil). `GenerateContentConfig`'s own
`responseJsonSchema` stays the `map[string]any` JSON decodes.

### generated-recordings[-sse].yaml

The bytes the vendored `(*recordPlugin).saveRecordings` writes for an
`invocationRecordState{caseDir: <temp dir>, userMessageIndex, streamingMode, recordings}` (the
file name is chosen by `streaming_mode`), with `existing.yaml` copied to that file name first
when present: read and `yaml.Unmarshal` the existing file into `recording.Recordings` (errors
ignored, the partial value kept), drop entries with the invocation's `user_message_index`, append
the case's recordings, `node.Encode`, `cleanYAMLNode(&node, false)`, `yaml.Marshal`. The probe
checks the temp dir then holds exactly that one file.

### replay.json

```
{"error": null|"<text>", "recordings": [<recording>…]}
```

The record bytes decoded as `replayplugin.loadInvocationState` does: `yaml.Unmarshal` into a
`yaml.Node`, the vendored `normalizeYAMLNode`, `root.Decode(&recording.Recordings{})`.
`recordings` is what `Decode` filled in (also when it fails), in the `<recording>` shape above
(`json.Marshal` of the Go values, so genai's custom marshalers apply: durations as whole seconds
`"%.0fs"` with zero omitted and `VideoMetadata` forcing `"startOffset": "0s"` when only the end
offset is set; `Citation.publicationDate` as `{"year","month","day"}` with the zero date
omitted; `[]byte` as base64). `error` is the bare `Unmarshal`/`Decode` error text; the plugin
wraps it as `failed to parse recordings from <path>: <err>` / `failed to decode recordings:
<err>`. The replay plugin only ever reads `generated-recordings.yaml`; the SSE case is decoded
the same way. Compare typed values (deserialize into the gadk types), not `serde_json::Value`s:
Go writes float32 fields with 32-bit precision (`0.7`), which gadk's `f32` serializes widened
(`0.699999988079071`), and `2` vs `2.0`.

Replay errors (2): `citation-dates` (`parsing time "0000-00-00": month out of range`, the zero
`civil.Date`) and `citation-partial-date` (`parsing time "2024-00-00": month out of range`).

### fixups.json

```
{"roots": [{"name", "go_type"}…],
 "fixups": [{"path", "owner", "key", "class", "go_type"}…],
 "recursion": [{"path", "owner", "key", "go_type"}…],
 "any_fields": […same…], "unencodable": […same…],
 "key_mismatches": [{"path", "owner", "go_name", "yaml_key", "json_tag", "kind"}…],
 "custom_json_types": ["<go type>"…],
 "jsonschema_fields": [{"order", "go_name", "yaml_key", "json_tag", "json_keyword", "go_type", "kind", "class"}…],
 "shape_paths": [{"path", "type"}…],
 "shapes": {"<go type>": {"go_type", "fields": [{"key", "go_name", "json", "category", "go_type", "elem", "zero"}…]}…},
 "shape_selfcheck": {"cases", "go_numbers": {"pass", "failures"}, "json_numbers": {"pass", "failures"}}}
```

A depth-first walk, in Go field order, of the reflect types reachable from the roots
`LLMRequest` (`model.LLMRequest`), `LLMResponse` (`model.LLMResponse`), `FunctionCall`
(`genai.FunctionCall`), `FunctionResponse` (`genai.FunctionResponse`) and `JsonSchema`
(`jsonschema.Schema`, what the `JsonSchema` class positions hold; walked last), following what
yaml.v3's encoder visits: exported struct fields (unexported skipped), keyed by
`strings.ToLower(GoFieldName)` (no genai/model type has yaml tags).

- **Path format.** `<root>` then, per step: `.<yaml key>` for a struct field (the key Go emits
  before `cleanYAMLNode` renames it), `[]` for a slice/array element, `{}` for a map value;
  pointers are transparent. Example:
  `LLMRequest.contents[].parts[].inlinedata.data`,
  `LLMRequest.config.tools[].functiondeclarations[].parameters.properties{}`. `owner` is the Go
  struct type holding the field (`genai.Blob`) and `key` its yaml key (`data`); `go_type` is the
  type as declared at that position (`*time.Duration`, `[]uint8`, `civil.Date`, `*genai.Schema`).
- **Classes** (`fixupClass`, in yaml.v3 `encoder.marshal`'s check order): `Bytes` (slice/array
  of `uint8`: an int sequence), `Time` (`time.Time`/`*time.Time`: RFC3339Nano timestamp, zero
  `0001-01-01T00:00:00Z`), `Duration` (`time.Duration`/`*time.Duration`: `Duration.String()`,
  zero `0s`), `YAMLMarshaler` (none reachable), `TextMarshaler` (`MarshalText`: `civil.Date`,
  zero `0000-00-00`), `SchemaTitle` (`genai.Schema`/`*genai.Schema`: walked further), and
  `JsonSchema` for `genai.FunctionDeclaration`'s `any` fields `ParametersJsonSchema` and
  `ResponseJsonSchema` (what Go function tools store there: `*jsonschema.Schema`; not walked).
  A pointer to a classed type is reported once, with the pointer as `go_type`. Classes stop the
  walk except `SchemaTitle`. Counts: Bytes 17 (16 + `JsonSchema.default`), Duration 20, Time 2,
  TextMarshaler 1, SchemaTitle 12, JsonSchema 2 (54).
- `recursion`: positions where a struct type already on the current path recurs (the walk stops
  there): 32 — the 9 `genai.Schema` self-references (`anyof[]`, `items`, `properties{}` under
  `config.responseschema` and each function declaration's `parameters`/`response`) and the 23
  `jsonschema.Schema` ones under `JsonSchema`.
- `any_fields`: interface-typed positions (33: `args{}`, `response{}`, `partmetadata{}`,
  `custommetadata{}`, `Schema.default`/`example`, `config.responsejsonschema`, `LLMRequest.tools{}`,
  `httpoptions.extrabody{}`, `customconfigs{}`; `JsonSchema.examples[]`, `enum[]`, `const`,
  `extra{}`).
- `unencodable`: func/chan/complex positions (1: `LLMRequest.config.httpoptions.extrasrequestprovider`,
  which would panic yaml.v3 — why `sanitizeLLMRequest` drops `HTTPOptions`).
- `key_mismatches`: fields whose JSON name, lowercased with `_` removed, is not their yaml key
  (`renamed`: 9, all `$`-keywords of `JsonSchema`: `id`, `schema`, `ref`, `comment`, `defs`,
  `anchor`, `dynamicanchor`, `dynamicref`, `vocabulary`), `json:"-"` fields yaml.v3 still emits
  (`json_ignored`: 10, `httpoptions.extrasrequestprovider`, `LLMRequest.tools` and 8 `JsonSchema`
  fields), and anonymous fields (`embedded`: none). Nothing under the genai/model roots is
  renamed.
- `custom_json_types`: reachable named types with a `MarshalJSON` method (their genai JSON is
  not their reflection shape): `genai.Citation`, `genai.GroundingChunkMapsRoute`,
  `genai.Interval`, `genai.StreamableHTTPTransport`, `genai.Transcription`,
  `genai.VideoMetadata`, `time.Time`.
- `jsonschema_fields`: every exported field of jsonschema-go v0.4.3's `Schema` in declaration
  order (65): `yaml_key` = lowercased Go name (what yaml.v3 emits for a `*jsonschema.Schema`),
  `json_tag` = the raw tag, `json_keyword` = the tag's name, or for the `json:"-"` fields what
  `Schema.MarshalJSON` writes them as (`Type`/`Types` → `type`, `Items`/`ItemsArray` → `items`,
  `DependencySchemas`/`DependencyStrings` → `dependencies`, `Extra` → `*` (its keys are written
  inline), `PropertyOrder` → `""` (orders `properties`, no keyword)); `go_type` (`json.RawMessage`
  for `Default`), `kind` (reflect kind) and `class` (`fixupClass` of the field type: `Bytes` for
  `Default`, else `""`).

### Shapes (`shape_paths`, `shapes`, `shape_selfcheck`)

- `shape_paths`: every position the walk visits whose type is a struct, recursion cut points
  included, as `{"path", "type"}` (217); the path format is the one above, `type` is the Go type
  name keying `shapes`.
- `shapes`: one entry per struct type reachable from the five roots (119, keyed by Go type name,
  e.g. `genai.Part`, `model.LLMRequest`, `jsonschema.Schema`; `civil.Date` and `time.Time` are
  leaves, not shapes). `fields` lists every field yaml.v3 encodes, in Go order (512 in all):
  - `key`: `strings.ToLower(GoName)`; `go_name`; `json`: the JSON tag name (`""` untagged,
    `"-"` ignored) — the serde name gadk uses;
  - `category`: `bytes`, `duration`, `time`, `textmarshaler` (non-pointer special leaves), else
    `ptr`, `iface`, `slice`, `map`, `string`, `int`, `uint`, `float`, `bool`, `struct`, `func`;
  - `go_type`: declared type (`*float32`, `[]*genai.Part`, `genai.FinishReason`);
  - `elem`: the struct type reached through pointers/slices/maps (`genai.Part` for
    `[]*genai.Part`), or `""`;
  - `zero`: the node Go's `node.Encode` emits for the field's zero value — the field's zero value
    encoded on its own with yaml.v3 (so after its text round trip): `{"kind": "scalar", "tag",
    "style", "value"}` (`style` lists the set flags as in gen-go-goyaml: `tagged`, `double`,
    `single`, `literal`, `folded`, `flow`; omitted when none), `{"kind": "sequence"|"mapping",
    "tag", "style"}` (empty, `flow`), `{"kind": "struct", "ref": "<type>"}` (a non-pointer
    struct: the zero mapping of that shape; none occurs) or `{"kind": "unencodable"}` (yaml.v3
    panics: `genai.HTTPOptions.extrasrequestprovider`). Observed zeros: nil pointer/interface →
    `!!null` `null`; string kinds → `!!str` `""` double-quoted; ints, uints **and floats** →
    `!!int` `0` (float `0` re-parses as an int); bool → `!!bool` `false`; slices and `[]byte` →
    empty flow `!!seq`; maps → empty flow `!!map`; `time.Duration` → `!!str` `0s`; `time.Time` →
    `!!timestamp` `0001-01-01T00:00:00Z`; `civil.Date` → `!!str` `0000-00-00`.
- `shape_selfcheck`: the probe rebuilds every case's recordings tree the way gadk will — struct
  mappings only from `shapes` (field order and keys from the table; a field a zero-skipping
  encoding omits — nil pointer/interface, empty slice/map, zero scalar or zero struct — gets its
  `zero` node; present fields are built recursively, `*jsonschema.Schema` in an `any` through the
  `jsonschema.Schema` shape, leaves and mapping keys encoded on their own with yaml.v3), the
  recording roots by their yaml tags (`omitempty` as Go), the merge read as Go's plain
  `yaml.Unmarshal` — then `cleanYAMLNode` and `yaml.Marshal`, and compares with the Go bytes.
  `go_numbers`: `any` values as Go holds them (JSON numbers are `float64`): 44/44.
  `json_numbers`: integral `float64`s in `any` positions turned into `int64` (what a
  `serde_json::Value` holds for a JSON integer): 43/44, failing `function-call-args`
  (Go: `big: 1.2345678901e+10`, shapes: `big: 12345678901`). yaml.v3 writes a `float64` with
  `strconv.FormatFloat(f, 'g', -1, 64)`, which uses exponent form once the decimal exponent
  reaches 6 (`1000000` → `1e+06`), so gadk must hold numbers in `any` positions as Go's
  `float64` (`GoValue::Float`), not as JSON integers. `failures` entries are
  `"<case>: line N: go …, shapes …"` (first differing line). With
  `GEN_GO_CONFIGURABLE_SELFCHECK_DIR=<dir>` the probe also writes each failing rebuild there.

### canonicalize.json

`[{"input", "modify", "output"}…]`: for each string in `canonicalize.go`'s
`canonicalizeInputs`, the vendored `modifyString(input)` and `canonicalizeJSONSubstrings(input)`
(40 rows: empty and non-JSON text, key reordering, whitespace, nested, greedy multi-candidate
and multi-line inputs, invalid JSON, duplicate keys, number re-formatting (`1.0` → `1`, `-0.0` →
`-0`, `12345678901234567890` → `12345678901234567000`, `1e-7`), HTML and U+2028 escaping, unicode,
Python literals (`'…'`, `None`, `True`, `False`) after `parameters:`/`result:` labels in any case
and spacing and standalone, apostrophes, ASCII-only `\b` next to non-ASCII letters, NBSP vs tab
after a label, the ADK "For context:" texts).

## Recording corpus (44)

Authored in `corpus.go` (`authoredCases`, 35, in this order; `streaming_mode` `none` and index
0 unless noted), then captured from ADK-Go (7 + 2):

| Case | Covers |
|---|---|
| `text` | user/model text, `finishReason` |
| `system-instruction` | multi-line (literal block), single-line (quoted) and text-less first part system instructions |
| `function-call-args` | function-call part with non-empty args of every JSON type, zeros, nulls, empty collections |
| `function-call-empty-args` | function calls with absent and `{}` args |
| `function-response` | function-response parts: non-empty/empty response, `willContinue`, `scheduling`, response parts with inline data |
| `thought-signature` | thought text and function-call parts with thought signatures |
| `inline-data` | inline data (set and empty), file data |
| `config-function-schema` | `genai.Schema` declarations: nested properties, items, enum, min/max, explicit titles, `propertyOrdering`, `response`, a declaration without parameters (title injection) |
| `config-json-schema` | `parametersJsonSchema` as `*jsonschema.Schema`: `$schema`, `$defs`/`$ref`, `default`, `enum`, `anyOf`/`const`, type arrays, `additionalProperties: false`, `true`/`{}`/`false` schemas, `responseJsonSchema` (pruned), config `responseJsonSchema` |
| `config-fields` | float32 sampling params, ints, stop sequences, logprobs, seed, response schema, safety settings, tool config, `labels` (incl. empty value), modalities, thinking config, `*bool` false |
| `google-search` | `googleSearch: {}` (kept as `google_search: {}`), `codeExecution`/`urlContext` (pruned: their keys are not in `snakeCaseFieldMap`), `retrieval: {}` (kept) |
| `google-search-time-range` | `time.Time` (set with nanoseconds, zero), exclude domains |
| `sanitize-http-options` | `config.httpOptions` dropped by `sanitizeLLMRequest` |
| `usage-metadata` | token counts (incl. zero), modality details, traffic type, model version |
| `grounding-metadata` | chunks, supports (float32 confidence scores), retrieval metadata, search entry point `sdkBlob` bytes, queries, widget token |
| `citation-dates` | citations with a set and a zero publication date (replay error) |
| `citation-set-date` | set date only (replays) |
| `citation-partial-date` | date with month/day 0 (`2024-00-00`; replay error) |
| `sse-partial` | `sse` mode (`generated-recordings-sse.yaml`), partial responses, `turnComplete` |
| `finish-reason-error` | response without content: finish reason, error code/message, `interrupted`, resumption handle |
| `logprobs` | float32 logprobs, `logProbabilitySum`, float64 `avgLogprobs` |
| `custom-metadata` | `customMetadata` zero values (pruned outside tool context) |
| `code-execution` | executable code, code execution result, part metadata |
| `transcription-durations` | `Transcription` durations zero, `1.5s`, `90s` (`1m30s`), `3600s` (`1h0m0s`) |
| `video-metadata` | `VideoMetadata` offsets (`65.5s` → `1m5.5s`), zero offsets, end offset only |
| `empty-request` | empty model/agent name, no responses (`llm_responses` omitted) |
| `tool-empty-id-name` | tool recordings with empty id/name, nil and `{}` args/response |
| `tool-args` | tool recordings: args/response zero values kept, `willContinue`, `scheduling`, `partialArgs`, response parts |
| `mixed-sequence` | LLM call → tool → LLM, a second agent, system instruction and declarations |
| `merge-second-message` | index 1, existing = `mixed-sequence`'s output (msg 0 kept, decoded lossily) |
| `merge-replace-message` | index 0, existing = `merge-second-message`'s output (msg 0 replaced, msg 1 first) |
| `merge-corrupt-existing` | unparsable existing file |
| `merge-go-key-existing` | index 2, hand-written existing file with Go-default (lowercased) keys: decodes fully; the existing index-2 entry is dropped |
| `merge-type-error-existing` | index 1, existing file with type errors (partial decode kept) |
| `merge-self-written` | index 1, no new recordings, existing = `function-call-args`' output: shows what Go loses re-reading its own snake_case keys |
| `conformance-<tool>` (7) | `validate_email`, `get_user_id`, `create_booking`, `search_flights`, `calculate_trip_cost`, `reimburse`, `ask_for_approval`: the vendored `functiontool.New` result's `Declaration()` in a request's `config.tools` |
| `transfer-to-agent` | the first request of an `llmagent` (`root_agent`, sub-agents `billing_agent`, `support_agent`) under the public runner, captured by a `BeforeModelCallback` that answers itself (the stub model is never called): instruction + transfer system instruction, `transfer_to_agent` declaration (`genai.Schema`) |
| `runner-tools-agent` | the same for an `llmagent` with the 7 conformance tools |

Captured cases' `input.json` is the JSON of the captured (sanitized) Go request; the probe records
both the JSON-rebuilt value and the captured value and requires identical bytes.

## Integration goldens (overlay probe)

`crates/gadk-configurable/tests/fixtures/integration/record-tool-call/*.yaml` are not written by
this program: `overlay/zz_t12probe_test.go.txt` is mapped into ADK-Go's
`internal/configurable/conformance` package with `go test -overlay` (ADK-Go's tree is not
modified) and runs the record plugin under the public runner; its header has the one-line
regeneration command. `scripts/check-fixtures.sh` only counts those two files.
