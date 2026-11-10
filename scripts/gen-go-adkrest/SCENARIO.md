# gen-go-adkrest scenario

`scripts/gen-go-adkrest` runs ADK-Go's real `adkrest.NewServer` (ADK-Go at
`docs/parity/ADK_GO_COMMIT`) behind a real loopback listener (`httptest.NewServer`, Go
`net/http` client), sends each of three server configurations the request script below, and
writes every exchange to `crates/gadk-server/testdata/wire/<config>_<NN>_<name>.json` (plan
Appendix C.7 format). gadk-server's `tests/wire.rs` (plan Task 13) rebuilds the same agents,
services, providers and configurations and replays the script against `Server::router()`.

Regenerate (needs ADK-Go checked out beside this repository, as for the other probes):

```
cd scripts/gen-go-adkrest && go run .
```

It deletes `crates/gadk-server/testdata/wire/*.json` first and prints
`wrote main=67 strict=11 custom=4 fixtures to ../../crates/gadk-server/testdata/wire`.

## Fixture format

```json
{
  "name": "<NN>_<name>",
  "config": "main|strict|custom",
  "request": {"method": "GET", "target": "/path?query", "headers": [["Name", "value"]], "body": ""},
  "response": {"status": 200, "headers": [["Content-Type", "application/json"]], "body": "..."},
  "compare": "exact|json_key_set|json_sorted_array|debug_trace|sse"
}
```

- `request.headers` are the headers the script sets (sorted by name); the client adds only
  `Host: 127.0.0.1:<port>` (unless the fixture sets `Host`), `User-Agent: Go-http-client/1.1`
  and `Content-Length` (`0` on a bodiless POST); no `Accept-Encoding`.
  A `Host` entry is the request's Host header (Go `req.Host`).
- `response.headers` are parsed from the raw bytes read off the socket (Go's client deletes
  `Connection: close` from the headers it returns, so they are not taken from it), in Go canonical
  form, sorted by name then value, without `Date`, `Content-Length` and `Transfer-Encoding`.
- `response.body` is the body after transfer decoding, as a UTF-8 string.
- `<NN>` is the 1-based position in the configuration's script: requests run in file-name order,
  one server per configuration, and later requests see earlier state.
- Targets that name an event ID carry the literal ID the fixed providers produce.

## Compare modes

- `exact`: status, headers and body byte for byte.
- `json_key_set` (`main_03_version`, `strict_02_version_no_credentials`): status and headers
  exact; the body's top-level JSON object key sets are equal (`/version` values are gadk's).
- `json_sorted_array` (`main_04_list_apps`): status and headers exact; the body JSON arrays are
  equal as sorted lists (Go's `MultiLoader.ListAgents` iterates a map).
- `sse` (`main_18_run_sse_hello`, `main_19_run_sse_error`): status and headers exact; the bodies
  split on `\n\n` give equal frame lists.
- `debug_trace` (`main_32`, `main_33`, `main_35`, `main_36`): status and headers exact; the bodies
  are parsed as JSON and normalised on both sides, then compared as JSON values:
  1. collect, in document order, every string value of a key `trace_id`, `span_id` or
     `parent_span_id` (at any depth, including inside `logs`), numbering each distinct value
     `<id-N>` from 0 in order of first appearance;
  2. replace every string value anywhere in the document that equals a collected value by its
     `<id-N>` (this also covers `gcp.vertex.agent.event_id`, which Go's debug store sets to the
     span ID on a `generate_content` span that has no event ID, e.g. the failed model calls in
     `main_36`);
  3. replace the value of every `start_time`, `end_time` and `observed_timestamp` key by `"<t>"`.

Run to run, only the four `debug_trace` fixtures change bytes (trace/span IDs from the OTel SDK,
wall-clock start/end times, `observed_timestamp` in the local zone); `main_04_list_apps` may
change order. Everything else is byte-identical across runs.

## Fixed providers

One fresh pair per configuration, installed on every request's context by a wrapper around the
server (`platform.WithTimeProvider`, `platform.WithUUIDProvider`), and shared by everything the
request runs (session service, runner, events, function-call IDs):

- clock: the n-th read (n = 0, 1, 2, …) returns `2026-01-02T03:04:05Z` + n seconds
  (`1767323045` + n Unix seconds);
- UUIDs: the n-th call (n = 1, 2, 3, …) returns `fmt.Sprintf("00000000-0000-4000-8000-%012d", n)`,
  e.g. `00000000-0000-4000-8000-000000000001`.

Both providers are guarded by a mutex. The fixtures pin how many reads each request makes (for
example `main_06` takes UUID 1 as the session ID; `main_09`'s state-delta event has invocation ID
UUID 2 and event ID UUID 3; `main_13`'s function call ID is `adk-` + UUID 13).

## Sample agents

`agent.NewMultiLoader(assistant, pipeline)`: root `assistant`, then `pipeline`.

- `assistant`: `llmagent.New` with
  - Name `assistant`, Description `Answers greetings and saves notes.`,
  - Instruction `Answer briefly. Use save_note to save notes.`,
  - Model: scripted model named `scripted-assistant` (below),
  - Tools: `save_note` (`functiontool.New`, Name `save_note`, Description
    `Saves a note as the artifact note.txt.`), arguments `{"text": string}` (Go struct
    `saveNoteArgs` with field `Text string` tagged `json:"text"`), result
    `{"status": string, "version": int64}` (Go struct `saveNoteResult` with fields
    `Status string` tagged `json:"status"` and `Version int64` tagged `json:"version"`).
    The tool saves `genai.NewPartFromText(args.text)` as artifact `note.txt` through the tool
    context's artifacts and returns `{"status":"saved","version":<saved version>}`.
- `pipeline`: `sequentialagent.New` with Name `pipeline`, Description `Drafts, then reviews.`,
  SubAgents `drafter`, `reviewer`:
  - `drafter`: `llmagent.New`, Name `drafter`, Description `Writes a draft.`, Instruction
    `Write a draft.`, model `scripted-drafter` that always answers text `Draft ready.`;
  - `reviewer`: `llmagent.New`, Name `reviewer`, Description `Reviews the draft.`, Instruction
    `Review the draft.`, model `scripted-reviewer` that always answers text `Review done.`.

No other agent options are set (no callbacks, no output key, no schemas, no
`GenerateContentConfig`).

### Scripted model

`Name()` returns the model name above. `GenerateContent(ctx, req, stream)` ignores `stream` and
yields exactly one item computed from `last`, the last element of `req.Contents` (nil when empty):

- a content reply → one `LLMResponse{Content: <reply>, UsageMetadata:
  {PromptTokenCount: 10, CandidatesTokenCount: 5, TotalTokenCount: 15}, FinishReason: STOP}`
  (all other fields zero);
- an error reply → one `(nil, err)`.

`scripted-assistant` replies, first match wins:

1. `last` has a part with a `FunctionResponse` → text `Saved your note.`;
2. the text of `last`'s first part with non-empty text is
   - `hello` → text `Hello! How can I help?`,
   - `fail` → error `scripted model failure`,
   - starts with `save: ` → one part `FunctionCall{Name: "save_note", Args: {"text": <rest after "save: ">}}`
     (no ID; ADK assigns `adk-<uuid>`),
   - anything else → text `I can only say hello.`.

Text replies are `genai.NewContentFromText(<text>, "model")`; the function-call reply is
`{role: "model", parts: [{functionCall: …}]}`.

## Services and configurations

Every configuration gets fresh `session.InMemoryService()`, `memory.InMemoryService()`,
`artifact.InMemoryService()`, a fresh loader built as above, and:

- `BindHost: "127.0.0.1"`, `AllowedOrigins: ["http://localhost:4200"]`;
- everything else zero (Go defaults), except:
- `main`: `DebugAPIConfig{IncludeDebugAPI: true}`, `MaxPayloadSize: 1024`; no authenticator or
  authorizer. Its `SpanProcessor()` and `LogProcessor()` are registered on the global OTel tracer
  provider (`sdktrace.NewTracerProvider(WithSpanProcessor(...))`) and logger provider
  (`sdklog.NewLoggerProvider(WithProcessor(...))`) before its first request; gadk installs
  `Server::debug_layer()` in the test subscriber instead.
- `strict`: `Authenticator: authn.NewHeader("X-Test-User")`, `Authorizer: authz.NewStrict()`;
  debug API off.
- `custom`: `Authenticator: authn.NewCustom(f)` where `f` reads `X-Test-User`: `forbid` →
  `authn.ErrForbidden`; `boom` → plain error `boom`; empty or absent → `nil, nil`; any other value
  `v` → `&Caller{UserID: v}`. Authorizer nil (noop); debug API off.

`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` and `ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN`
are unset (the probe unsets both; gadk uses `MapEnv::default()`).

## Request script

`{call}` = `00000000-0000-4000-8000-000000000012` (the function-call event of `main_13`),
`{resp}` = `00000000-0000-4000-8000-000000000015` (its function-response event). The `status`
column is what ADK-Go answered.

### `main`

| # | name | request | headers | body | status | compare |
|---|---|---|---|---|---|---|
| 01 | health_get | `GET /health` | — | — | 200 | exact |
| 02 | health_head | `HEAD /health` | — | — | 200 | exact |
| 03 | version | `GET /version` | — | — | 200 | json_key_set |
| 04 | list_apps | `GET /list-apps` | — | — | 200 | json_sorted_array |
| 05 | session_create_with_id | `POST /apps/assistant/users/alice/sessions/s1` | `Content-Type: application/json` | `{"state":{"color":"blue"}}` | 200 | exact |
| 06 | session_create_without_id | `POST /apps/assistant/users/alice/sessions` | — | — | 200 | exact |
| 07 | session_get | `GET /apps/assistant/users/alice/sessions/s1` | — | — | 200 | exact |
| 08 | session_list | `GET /apps/assistant/users/alice/sessions` | — | — | 200 | exact |
| 09 | session_patch | `PATCH /apps/assistant/users/alice/sessions/s1` | `Content-Type: application/json` | `{"stateDelta":{"mood":"happy"}}` | 200 | exact |
| 10 | session_patch_scoped | `PATCH /apps/assistant/users/alice/sessions/s1` | `Content-Type: application/json` | `{"stateDelta":{"app:theme":"dark","user:lang":"en"}}` | 400 | exact |
| 11 | session_get_missing | `GET /apps/assistant/users/alice/sessions/missing` | — | — | 404 | exact |
| 12 | run_hello | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 200 | exact |
| 13 | run_save_note | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"save: buy milk"}]}}` | 200 | exact |
| 14 | run_save_note_again | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"save: buy eggs"}]}}` | 200 | exact |
| 15 | run_error | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"fail"}]}}` | 500 | exact |
| 16 | run_unknown_session | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"nope","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 404 | exact |
| 17 | run_unknown_field | `POST /run` | `Content-Type: application/json` | `{"appName":"assistant","bogus":1}` | 400 | exact |
| 18 | run_sse_hello | `POST /run_sse` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]},"streaming":true}` | 200 | sse |
| 19 | run_sse_error | `POST /run_sse` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"fail"}]}}` | 200 | sse |
| 20 | run_sse_unknown_session | `POST /run_sse` | `Content-Type: application/json` | `{"appName":"assistant","userId":"alice","sessionId":"nope","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 404 | exact |
| 21 | run_live_missing_params | `GET /run_live` | — | — | 500 | exact |
| 22 | pipeline_session_create | `POST /apps/pipeline/users/alice/sessions/p1` | — | — | 200 | exact |
| 23 | run_pipeline | `POST /run` | `Content-Type: application/json` | `{"appName":"pipeline","userId":"alice","sessionId":"p1","newMessage":{"role":"user","parts":[{"text":"go"}]}}` | 200 | exact |
| 24 | artifacts_list | `GET /apps/assistant/users/alice/sessions/s1/artifacts` | — | — | 200 | exact |
| 25 | artifact_get | `GET /apps/assistant/users/alice/sessions/s1/artifacts/note.txt` | — | — | 200 | exact |
| 26 | artifact_get_query_version | `GET /apps/assistant/users/alice/sessions/s1/artifacts/note.txt?version=1` | — | — | 200 | exact |
| 27 | artifact_get_version | `GET /apps/assistant/users/alice/sessions/s1/artifacts/note.txt/versions/1` | — | — | 200 | exact |
| 28 | artifact_get_version_missing | `GET /apps/assistant/users/alice/sessions/s1/artifacts/note.txt/versions/7` | — | — | 404 | exact |
| 29 | artifact_get_version_invalid | `GET /apps/assistant/users/alice/sessions/s1/artifacts/note.txt/versions/x` | — | — | 400 | exact |
| 30 | artifact_delete | `DELETE /apps/assistant/users/alice/sessions/s1/artifacts/note.txt` | — | — | 200 | exact |
| 31 | artifacts_list_after_delete | `GET /apps/assistant/users/alice/sessions/s1/artifacts` | — | — | 200 | exact |
| 32 | debug_trace_event | `GET /debug/trace/{call}` | — | — | 200 | debug_trace |
| 33 | debug_trace_event_dev | `GET /dev/apps/assistant/debug/trace/{resp}` | — | — | 200 | debug_trace |
| 34 | debug_trace_event_missing | `GET /debug/trace/nope` | — | — | 404 | exact |
| 35 | debug_trace_session | `GET /debug/trace/session/p1` | — | — | 200 | debug_trace |
| 36 | debug_trace_session_dev | `GET /dev/apps/assistant/debug/trace/session/s1` | — | — | 200 | debug_trace |
| 37 | event_graph | `GET /apps/assistant/users/alice/sessions/s1/events/{call}/graph` | — | — | 200 | exact |
| 38 | event_graph_dev | `GET /dev/apps/assistant/users/alice/sessions/s1/events/{resp}/graph` | — | — | 200 | exact |
| 39 | event_graph_missing_event | `GET /apps/assistant/users/alice/sessions/s1/events/nope/graph` | — | — | 404 | exact |
| 40 | build_graph | `GET /dev/apps/pipeline/build_graph` | — | — | 200 | exact |
| 41 | build_graph_image | `GET /dev/apps/assistant/build_graph_image` | — | — | 200 | exact |
| 42 | build_graph_image_pipeline_dark | `GET /dev/apps/pipeline/build_graph_image?dark_mode=true` | — | — | 200 | exact |
| 43 | build_graph_image_node | `GET /dev/apps/pipeline/build_graph_image?node=pipeline/reviewer` | — | — | 200 | exact |
| 44 | build_graph_image_node_missing | `GET /dev/apps/pipeline/build_graph_image?node=pipeline/nobody` | — | — | 404 | exact |
| 45 | build_graph_unknown_app | `GET /dev/apps/nobody/build_graph` | — | — | 404 | exact |
| 46 | builder_get | `GET /dev/apps/assistant/builder` | — | — | 200 | exact |
| 47 | builder_save | `POST /dev/apps/assistant/builder/save` | — | — | 501 | exact |
| 48 | tests_list | `GET /dev/apps/assistant/tests` | — | — | 501 | exact |
| 49 | eval_sets_list | `GET /dev/apps/assistant/eval_sets` | — | — | 501 | exact |
| 50 | eval_sets_create_dash | `POST /dev/apps/assistant/eval-sets` | — | — | 501 | exact |
| 51 | eval_legacy_results | `GET /apps/assistant/eval_results` | — | — | 501 | exact |
| 52 | metrics_info | `GET /dev/apps/assistant/metrics-info` | — | — | 200 | exact |
| 53 | unknown_path | `GET /no/such/path` | — | — | 404 | exact |
| 54 | wrong_method | `PUT /list-apps` | — | — | 405 | exact |
| 55 | wrong_method_run | `GET /run` | — | — | 405 | exact |
| 56 | options_session | `OPTIONS /apps/assistant/users/alice/sessions/s1` | — | — | 204 | exact |
| 57 | options_unknown_path | `OPTIONS /no/such/path` | — | — | 404 | exact |
| 58 | head_list_apps | `HEAD /list-apps` | — | — | 200 | exact |
| 59 | trailing_slash_post | `POST /apps/assistant/users/alice/sessions/s2/` | `Content-Type: application/json` | `{"state":{"via":"slash"}}` | 200 | exact |
| 60 | trailing_slash_get | `GET /apps/assistant/users/alice/sessions/s2/` | — | — | 200 | exact |
| 61 | trailing_slash_root | `GET /` | — | — | 404 | exact |
| 62 | oversized_body | `POST /apps/assistant/users/alice/sessions` | `Content-Type: application/json` | `{"state":{"padding":"<2048 × a>"}}` (2071 bytes) | 400 | exact |
| 63 | origin_forbidden | `GET /health` | `Origin: http://evil.example` | — | 403 | exact |
| 64 | origin_allowed | `GET /health` | `Origin: http://localhost:4200` | — | 200 | exact |
| 65 | host_forbidden | `GET /health` | `Host: evil.example` | — | 403 | exact |
| 66 | session_delete | `DELETE /apps/assistant/users/alice/sessions/s2` | — | — | 200 | exact |
| 67 | session_list_after_delete | `GET /apps/assistant/users/alice/sessions` | — | — | 200 | exact |

### `strict`

| # | name | request | headers | body | status | compare |
|---|---|---|---|---|---|---|
| 01 | health_no_credentials | `GET /health` | — | — | 200 | exact |
| 02 | version_no_credentials | `GET /version` | — | — | 200 | json_key_set |
| 03 | list_apps_no_credentials | `GET /list-apps` | — | — | 401 | exact |
| 04 | unknown_path_no_credentials | `GET /no/such/path` | — | — | 404 | exact |
| 05 | wrong_method_no_credentials | `PUT /list-apps` | — | — | 405 | exact |
| 06 | session_create_authz_forbidden | `POST /apps/assistant/users/alice/sessions/s1` | `X-Test-User: bob` | — | 403 | exact |
| 07 | session_create | `POST /apps/assistant/users/alice/sessions/s1` | `X-Test-User: alice` | — | 200 | exact |
| 08 | session_get_authz_forbidden | `GET /apps/assistant/users/alice/sessions/s1` | `X-Test-User: bob` | — | 403 | exact |
| 09 | run_authz_forbidden | `POST /run` | `Content-Type: application/json`<br>`X-Test-User: bob` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 403 | exact |
| 10 | run_sse_authz_forbidden | `POST /run_sse` | `Content-Type: application/json`<br>`X-Test-User: bob` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 403 | exact |
| 11 | run | `POST /run` | `Content-Type: application/json`<br>`X-Test-User: alice` | `{"appName":"assistant","userId":"alice","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}` | 200 | exact |

### `custom`

| # | name | request | headers | body | status | compare |
|---|---|---|---|---|---|---|
| 01 | authn_forbidden | `GET /list-apps` | `X-Test-User: forbid` | — | 403 | exact |
| 02 | authn_error | `GET /list-apps` | `X-Test-User: boom` | — | 500 | exact |
| 03 | authn_no_caller | `GET /list-apps` | — | — | 401 | exact |
| 04 | authn_ok | `GET /apps/assistant/users/alice/sessions` | `X-Test-User: alice` | — | 200 | exact |
