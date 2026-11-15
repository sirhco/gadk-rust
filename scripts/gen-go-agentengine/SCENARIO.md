# gen-go-agentengine scenario

`scripts/gen-go-agentengine` records ADK-Go's Agent Engine server and launcher (ADK-Go at
`docs/parity/ADK_GO_COMMIT`) as gadk fixtures, and generates gadk-server's `ConvertSnake` shape
table:

- `crates/gadk-server/testdata/agentengine/` (29 files): `server/agentengine.NewHandler` served by
  `httptest.NewServer`, one file per exchange, plus its startup log and `ListClassMethods()`;
- `crates/gadk-launcher/testdata/agentengine/` (13 files): `cmd/launcher/agentengine.NewLauncher`
  as a subprocess: command lines (`cli_*`) and web servers (`wire_*`);
- `--shapes`: `crates/gadk-server/src/agentengine/encode_shapes.rs`.

gadk-server's agentengine replay (plan 5c Task 10) replays the server fixtures exactly;
gadk-launcher's agentengine tests (Tasks 8 and 10) replay the launcher fixtures after the
normalization below. Declared deltas (`<name>.deltas.json`) are written by the replay's task,
never by this probe.

Regenerate (ADK-Go checked out beside this repository):

```
cd scripts/gen-go-agentengine && go run .            # fixtures
cd scripts/gen-go-agentengine && go run . --shapes   # encode_shapes.rs (--out PATH to write elsewhere)
```

With ADK-Go elsewhere (or from a worktree, where `../../../adk-go` does not resolve), point a
temporary modfile at it (`go.sum` must sit next to it) and set `ADK_GO_DIR` (the `--shapes`
generator runs inside it):

```
cd scripts/gen-go-agentengine && tmp="$(mktemp -d)" && \
  sed "s#=> ../../../adk-go#=> $ADK_GO_DIR#" go.mod > "$tmp/go.mod" && cp go.sum "$tmp/" && \
  go run -modfile="$tmp/go.mod" .   # or: … . --shapes
```

The probe records every fixture twice and compares the two runs byte for byte; on any difference
it prints both and exits 1 without writing. Otherwise it deletes the `*.json` files (except
`*.deltas.json`) of both directories, writes the fixtures and prints
`wrote server=29 launcher=13 fixtures`. A run takes a few seconds.

## Agent, model and providers

As `scripts/gen-go-adkrest/SCENARIO.md` § Agents and § Scripted model, `assistant` only:
`llmagent` `assistant` (Description `Answers greetings and saves notes.`, Instruction `Answer
briefly. Use save_note to save notes.`) over `scripted-assistant` with the `save_note` tool
(`hello` → `Hello! How can I help?`; `fail` → error `scripted model failure`; `save: <x>` →
`save_note {"text": "<x>"}`, then `Saved your note.`; anything else → `I can only say hello.`),
loaded with `agent.NewSingleLoader(assistant)`. Every model response carries usage metadata
(10/5/15) and finish reason `STOP`.

Fixed providers (5a's `fixedPlatform`): clock `2026-01-02T03:04:05Z` + n s for the n-th read
(n = 0, 1, …), UUIDs `00000000-0000-4000-8000-%012d` (n = 1, 2, …), one sequence per server
configuration, put on every request's context by `withPlatform`.

The agent engine ID (the runner's app name) is `test-engine`.

## Server fixtures

Two configurations, each with fresh `session.InMemoryService()`, `memory.InMemoryService()` and
`artifact.InMemoryService()`:

- `main`: `agentengine.NewHandler(cfg, 0, 4096, "test-engine")`;
- `small`: `agentengine.NewHandler(cfg, 0, 32, "test-engine")`.

`cfg` is `&launcher.Config{SessionService, MemoryService, ArtifactService, AgentLoader}`. The
handler is wrapped in `withPlatform` and served by `httptest.NewServer` (a real server, so
`SetWriteDeadline` succeeds and nothing logs `SetWriteDeadline failed`). Go's standard logger is
captured with `log.SetFlags(0)` and `log.SetOutput(buf)`. Requests run in file-name order on one
`http.Client` (`DisableCompression`, redirects not followed); later requests see earlier state.

File `<config>_<NN>_<case>.json`, written by `json.Encoder` with `SetEscapeHTML(false)` and
`SetIndent("", "  ")`:

```json
{
  "name": "main_02_create_session",
  "config": "main",
  "kind": "exchange",
  "request": {"method": "POST", "target": "/reasoning_engine", "headers": [["Content-Type", "application/json"]], "body": "…"},
  "response": {"status": 200, "headers": [["Content-Type", "text/plain; charset=utf-8"]], "body": "…"},
  "log": "",
  "methods": null,
  "compare": "exact"
}
```

- `kind`: `exchange` (one request); `startup` (`request`/`response` null, `log` = what `NewHandler`
  logged); `class_methods` (`request`/`response` null, `methods` = `[m.AsMap() for m in
  ListClassMethods()]`, `log` empty).
- `request.headers`: the headers the step sets, sorted by name. The client adds only `Host`,
  `User-Agent: Go-http-client/1.1` and `Content-Length` (also `Content-Length: 0` for a `POST`
  without a body). `target` is relative to the handler root and sent as is (no cleaning).
- `response.headers`: read off the socket, canonical names, sorted by name then value, without
  `Date`, `Content-Length` and `Transfer-Encoding`. `response.body`: the transfer-decoded body.
- `log`: the bytes Go's logger wrote during the exchange (flags 0: no timestamps) with every line
  starting `Processing event: ` removed (the stream methods' `%+v` event dump, which prints
  pointers).
- `compare`: `exact` for every case (status, headers, body and log byte for byte).

| File | Request (`POST`, `Content-Type: application/json` unless noted) |
|---|---|
| `main_00_startup` | `kind: startup` |
| `main_01_class_methods` | `kind: class_methods` |
| `main_02_create_session` | `/reasoning_engine` `{"class_method":"async_create_session","input":{"user_id":"u1"}}` |
| `main_03_create_session_with_state` | `/reasoning_engine` create, input `{"user_id":"u1","state":{"k":"v","n":1}}` |
| `main_04_get_session` | `/reasoning_engine` `async_get_session` of `main_02`'s session (`u1`) |
| `main_05_get_session_missing` | `/reasoning_engine` `async_get_session` `u1` / `nope` |
| `main_06_list_sessions` | `/reasoning_engine` `{"class_method":"async_list_sessions","input":{"user_id":"u1"}}` |
| `main_07_delete_session` | `/reasoning_engine` `async_delete_session` of `main_03`'s session |
| `main_08_delete_session_missing` | `/reasoning_engine` `async_delete_session` `u1` / `nope` |
| `main_09_stream_query_text` | `/stream_reasoning_engine` `async_stream_query` `{"user_id":"u2","message":"hello"}` (no session: the runner creates one) |
| `main_10_stream_query_content` | `/stream_reasoning_engine` `async_stream_query`, `u2`, `main_09`'s session, `message` `{"role":"user","parts":[{"text":"hello"}]}` |
| `main_11_stream_query_tool_call` | same session, `message` `"save: milk"` |
| `main_12_stream_query_model_error` | same session, `message` `"fail"` |
| `main_13_stream_query_bad_input` | `/stream_reasoning_engine` `async_stream_query` `{"user_id":"u2","message":5}` |
| `main_14_run_with_events_new_session` | `/stream_reasoning_engine` `streaming_agent_run_with_events`, `request_json` = `{"user_id":"u3","session_id":"projects/p/locations/global/collections/default_collection/engines/e/sessions/s","message":{"role":"user","parts":[{"text":"hello"}]}}` |
| `main_15_run_with_events_reuse_session` | same method, `request_json` with `main_14`'s `session_id` |
| `main_16_run_with_events_bad_request_json` | same method, `request_json` `"{"` |
| `main_17_session_method_on_stream_route` | `/stream_reasoning_engine` `async_list_sessions` `u1` |
| `main_18_stream_method_on_session_route` | `/reasoning_engine` `async_stream_query` `{"user_id":"u1","message":"hello"}` |
| `main_19_unknown_method` | `/reasoning_engine` `{"class_method":"nope","input":{}}` |
| `main_20_bad_body` | `/reasoning_engine` `{"class_method":5}` |
| `main_21_empty_body` | `/reasoning_engine`, no body, no `Content-Type` |
| `main_22_uppercase_keys` | `/reasoning_engine` `{"CLASS_METHOD":"async_list_sessions","Input":{"USER_ID":"u1"}}` |
| `main_23_unknown_path` | `/nope`, the list body |
| `main_24_wrong_http_method` | `GET /reasoning_engine`, no body, no `Content-Type` |
| `main_25_trailing_slash` | `/reasoning_engine/`, the list body (strict-slash redirect) |
| `main_26_unclean_path` | `/x/../reasoning_engine`, the list body (clean-path redirect) |
| `main_27_reasoning_engine_query` | `/reasoning_engine?x=1`, the list body |
| `small_01_over_limit_payload` | `/reasoning_engine`, the list body (63 bytes, read up to 32) |

Session IDs come from earlier responses (`output.id`; `main_14`'s first NDJSON line's
`session_id`), except `main_09`'s, which the stream does not name: the probe reads it from the
session service (`List` of `u2`, which reads no clock or UUID). Every request body is in its
fixture, so a replay needs nothing else.

## Launcher fixtures

The probe binary is also the launched program: with `GEN_GO_AGENTENGINE_CHILD` set it runs
`childMain`, ADK-Go's example `main` with the assistant agent:

```go
config := &launcher.Config{AgentLoader: agent.NewSingleLoader(assistant)}
l := agentengine.NewLauncher("test-engine") // cmd/launcher/agentengine
if err = l.Execute(ctx, config, os.Args[2:]); err != nil {
	log.Fatalf("Run failed: %v\n\n%s", err, l.CommandLineSyntax())
}
```

`os.Args[1]` names the launcher: `agentengine` (as above) or `agentengine_bad_compaction` (the
same with `config.Compaction = &compaction.Config{}`, Go `TestNewHandlerRejectsUnusableCompaction`'s
"no strategy at all"). As in `scripts/gen-go-launcher`: `ctx` carries the fixed providers (only
the launcher's own context; the web server's request contexts do not derive from it) and is
cancelled on SIGTERM; before `log.Fatalf` the error text is written to the file named by
`GEN_GO_AGENTENGINE_ERROR_FILE`; the child's environment is only `PATH`, `HOME`, `TZ=UTC` and the
two probe variables; stdin is empty.

`cli_<case>.json` uses the `testdata/cli` schema of `scripts/gen-go-launcher/SCENARIO.md`
§ Process fixtures (`name` = the file stem, `launcher` = the launcher name above); `wire_<server>_<NN>_<step>.json`
uses its `testdata/wire` schema (§ Web fixtures, § Request, § Response) with `name` = the file
stem, `server` = `agentengine` or `prefix`, `launcher` = `agentengine`, kinds `startup` and
`exchange` only (no shutdown file: the probe stops the server with SIGTERM and records nothing).
Requests are raw HTTP/1.1 on a fresh connection: `Host: 127.0.0.1:<port>` first, the step's
headers, `Content-Length` (body or `POST`), `Connection: close` last.

| File | Launcher, args / request |
|---|---|
| `cli_help_top` | `-h` |
| `cli_help_web` | `web -h` |
| `cli_help_web_agentengine` | `web agentengine -h` |
| `cli_bad_flag` | `web agentengine -nope` |
| `cli_bad_max_payload_size` | `web agentengine -max_payload_size x` |
| `cli_bad_sse_write_timeout` | `web agentengine -sse-write-timeout x` |
| `cli_newhandler_error` | `agentengine_bad_compaction`: `web -port 0 agentengine` (the web launcher's own `Config.Validate` reports `invalid Compaction: …` first) |
| `wire_agentengine_00_startup` | `web -port <port> agentengine` |
| `wire_agentengine_01_create_session` | `POST /api/reasoning_engine` `{"class_method":"async_create_session","input":{"user_id":"u1"}}` |
| `wire_agentengine_02_stream_query` | `POST /api/stream_reasoning_engine` `{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hello"}}` |
| `wire_agentengine_03_get_not_allowed` | `GET /api/reasoning_engine` (the outer `Methods("POST")` route: 405) |
| `wire_prefix_00_startup` | `web -port <port> agentengine -path_prefix custom/` |
| `wire_prefix_01_create_session` | `POST /custom/reasoning_engine`, the create body |

### Normalization

`scripts/gen-go-launcher/SCENARIO.md` § Normalization N1–N3 and N5–N7, with two additions (N4 is
moot: one sub-launcher):

- N5 also matches the key `last_update_time` (the agentengine session's float seconds): regex
  `"(lastUpdateTime|last_update_time|timestamp)":(-?[0-9][0-9.eE+-]*|"[^"]*")`.
- After N1, every log line starting `<ts> Processing event: ` is removed (as in the server
  fixtures).

## `--shapes`

`go run . --shapes [--out PATH]` writes `encode_shapes.rs` (default
`../../crates/gadk-server/src/agentengine/encode_shapes.rs`). `server/agentengine/internal/models`
is importable only from inside ADK-Go's `server/agentengine/` tree, so the probe writes an
overlay file

```json
{"Replace":{"$ADK_GO_DIR/server/agentengine/zzgadkshapes/main.go":"<abs>/shapes/main.go.in"}}
```

and runs, in `$ADK_GO_DIR` (default `../../../adk-go`) with `GOFLAGS=-mod=mod`,
`go run -overlay <overlay> ./server/agentengine/zzgadkshapes <first 8 hex of docs/parity/ADK_GO_COMMIT>`;
its stdout is the file. ADK-Go's working tree is not modified, and `shapes/main.go.in` is not a
`.go` file, so this module's `go build ./...` and `go vet ./...` never compile it.

The generator walks `session.Event` and the 14 `internal/models` request/response types by
reflection and emits every struct type reachable through fields, pointers, slices, arrays and map
values (`time.Time` and `json.RawMessage` are kinds, not structs), sorted by
`reflect.Type.String()`: one `const <PKG>_<CONVERTNAME>: GoStruct` each (Go's `convertName`
algorithm, upper-cased; a collision fails) and `ALL`. Field kinds: `json.RawMessage` (by type
identity) → `RawMessage`; `time.Time` → `Time`; pointer → `Pointer`; `[]byte` → `Bytes`; slice or
array → `Slice`; map (string keys, else fail) → `Map`; struct → `Struct("<type>")`; interface →
`Interface`; `string` → `String`, another string type → `NamedString`; `bool` → `Bool`; ints →
`Int`; uints → `Uint`; `float32`/`float64` → `Float32`/`Float64`; anything else fails. At
`0990f9e2`: 88 structs, 435 fields. `scripts/check-fixtures.sh` regenerates the file into a
temporary directory and `cmp`s it with the checked-in one.

## Files

- `main.go`: the probe, the launched program and the `--shapes` driver.
- `shapes/main.go.in`: the shape generator, compiled only through the overlay.
- `go.mod`, `go.sum`: module `gadk/gen-go-agentengine`; `google.golang.org/adk/v2 =>
  ../../../adk-go`.
