# gen-go-launcher scenario

`scripts/gen-go-launcher` runs ADK-Go's launcher (`cmd/launcher/full.NewLauncher()` and
`cmd/launcher/prod.NewLauncher()`, ADK-Go at `docs/parity/ADK_GO_COMMIT`) as a subprocess around a
scripted agent and records three kinds of fixtures under `crates/gadk-launcher/testdata/`:

- `wire/`: web servers launched on an ephemeral loopback port, each driven with raw HTTP/1.1
  requests: one file for the startup log, one per exchange, one for the shutdown;
- `console/`: `console` runs, stdin script → stdout bytes and stderr;
- `cli/`: command lines that fail or ask for help → stdout, stderr, exit status and the returned
  error text.

gadk-launcher's `tests/wire_replay.rs` (plan Task 12) replays every fixture against
gadk-launcher's `full::new()` / `prod::new()` and compares after applying the normalization below
and the fixture's declared deltas (`<name>.deltas.json`, written by Task 12, never by this probe).

Regenerate (needs ADK-Go checked out beside this repository, as for the other probes):

```
cd scripts/gen-go-launcher && go run .
```

With ADK-Go elsewhere (or from a worktree, where `../../../adk-go` does not resolve), point a
temporary modfile at it (`go.sum` must sit next to it):

```
cd scripts/gen-go-launcher && tmp="$(mktemp -d)" && \
  sed "s#=> ../../../adk-go#=> $ADK_GO_DIR#" go.mod > "$tmp/go.mod" && cp go.sum "$tmp/" && \
  go run -modfile="$tmp/go.mod" .
```

The probe records every fixture, then replays every fixture from its own JSON alone (section
"Replay") in fresh subprocesses and compares the replayed bytes with the recorded bytes; on any
difference it prints both and exits 1 without writing. Otherwise it deletes
`testdata/{wire,console,cli}/*.json` except `*.deltas.json`, writes the fixtures and prints
`wrote wire=70 console=15 cli=34 fixtures to ../../crates/gadk-launcher/testdata`. Run to run,
every fixture is byte-identical. A run takes about 25 s.

## The launched program

The probe binary is also the launched program: with `GEN_GO_LAUNCHER_CHILD` set it runs
`childMain`, which is ADK-Go's example `main` (`examples/quickstart/main.go`) with the scripted
agent:

```go
config := &launcher.Config{AgentLoader: agent.NewSingleLoader(assistant)}
l := full.NewLauncher() // or prod.NewLauncher()
if err = l.Execute(ctx, config, os.Args[2:]); err != nil {
	log.Fatalf("Run failed: %v\n\n%s", err, l.CommandLineSyntax())
}
```

`os.Args[1]` is the preset (`full` or `prod`), the rest are the launcher arguments. The child
differs from the example in three ways, each reproduced by the replay:

1. `ctx` carries fixed providers (`platform.WithTimeProvider`, `platform.WithUUIDProvider`): clock
   `2026-01-02T03:04:05Z` + n s for the n-th read (n = 0, 1, …); UUIDs
   `00000000-0000-4000-8000-%012d` (n = 1, 2, …). Only the console's runner sees them (the web
   server's request contexts do not derive from `ctx`); no recorded output contains them.
2. `ctx` is cancelled on SIGTERM (`signal.NotifyContext(ctx, syscall.SIGTERM)`); the probe stops a
   web server that way. The replay cancels the `CancellationToken` it passed to `execute`. (The
   web launcher itself installs no signal handler.)
3. Before `log.Fatalf`, the error text `err.Error()` is written to the file named by
   `GEN_GO_LAUNCHER_ERROR_FILE`; the fixture's `error` is that text (`null` when `Execute`
   returned nil).

`log.Fatalf` exits with status 1. The child's environment is only `PATH`, `HOME`, `TZ=UTC` and the
two probe variables (no `OTEL_*`, no `GOOGLE_*`). Its stdin is the fixture's `stdin` (CLI and web
fixtures: empty), and stdout is a pipe, so the console's unset `-streaming_mode` resolves to
`none` (stdout is not a character device; the replay passes `stdout_is_char_device = false`).

## Agent and model

`assistant`: `llmagent.New` with Name `assistant`, Description `Answers greetings and transfers
funds.`, Instruction `Answer briefly. Use transfer_funds to transfer funds.`, model
`scripted-assistant`, tool `transfer_funds` (`functiontool.New`, Description `Transfers funds.`,
`RequireConfirmation: true`, args `{"amount": int}` (Go struct field `Amount int` tagged
`json:"amount"`), result `{"status": string}` = `{"status": "transferred <amount>"}`). No other
options (no callbacks, output key, schemas, `GenerateContentConfig`).

Scripted model: `Name()` `scripted-assistant`; `GenerateContent(ctx, req, stream)` yields, in
order, the responses below, computed from `last`, the last element of `req.Contents`.
`final(parts…)` = `LLMResponse{Content: {role: "model", parts}, UsageMetadata:
{promptTokenCount: 10, candidatesTokenCount: 5, totalTokenCount: 15}, FinishReason: STOP}`;
`chunk(text)` = `LLMResponse{Content: {role: "model", parts: [{text}]}, Partial: true}`;
`reply(c₁, c₂)` = `stream ? [chunk c₁, chunk c₂, final(text c₁+c₂)] : [final(text c₁+c₂)]`.

1. `last` has a part with a `FunctionResponse`: if its `response` has an `error` key →
   `reply("Transfer ", "cancelled.")`, else `reply("Transfer ", "done.")`;
2. otherwise by the text of `last`'s first part with non-empty text:
   - `hello` → `reply("Hello! ", "How can I help?")`;
   - `revise` → `stream ? [chunk "Draft ", chunk "answer.", final(text "Final answer.")] :
     [final(text "Final answer.")]` (the final differs from the partials);
   - `fail` → one `(nil, errors.New("scripted model failure"))`;
   - `transfer` → `[final(functionCall transfer_funds {"amount": 100})]` (no ID; ADK assigns
     `adk-<uuid>`);
   - anything else → `reply("I can only ", "say hello.")`.

## Process fixtures (`console/`, `cli/`)

```json
{
  "name": "<case>",
  "launcher": "full|prod",
  "args": ["console", "-streaming_mode", "none"],
  "stdin": "hello\n",
  "stdout": "<bytes the child wrote to stdout>",
  "stderr": "<bytes the child wrote to stderr, normalized (N1)>",
  "exit_status": 0,
  "error": null
}
```

The file is `<dir>/<case>.json`. `stdout` is verbatim (the console's `fmt.Print*` text). `stderr`
holds Go `log` lines (with the `<ts> ` prefix after N1) and Go `flag` usage output (no prefix),
interleaved in write order. The replay runs `execute(token, config, args)` with stdin = `stdin`,
the console's output and the launcher's `golog` sink and every `FlagSet`'s output captured, then:
on `Err(e)` it writes `Run failed: <e>\n\n<command_line_syntax()>` through a `golog` logger to the
same stderr capture (as `log.Fatalf` does) and sets `exit_status` 1; on `Ok` 0. `error` is
`e.to_string()` or `null`. It compares `stdout`, `stderr` (after N1), `exit_status` and `error` for
equality.

### Console cases

| case | args | stdin |
|---|---|---|
| `none_hello` | `console -streaming_mode none` | `hello\n` |
| `none_two_turns` | `console -streaming_mode none` | `hello\nsomething else\n` |
| `sse_hello` | `console -streaming_mode sse` | `hello\n` |
| `sse_two_turns` | `console -streaming_mode sse` | `hello\nsomething else\n` |
| `none_revised_final` | `console -streaming_mode none` | `revise\n` |
| `sse_revised_final` | `console -streaming_mode sse` | `revise\n` |
| `default_mode_no_args` | (none: the universal launcher's first sub-launcher, console) | `hello\n` |
| `crlf_input` | `console -streaming_mode none` | `hello\r\n` |
| `agent_error` | `console -streaming_mode none` | `fail\nhello\n` |
| `sse_agent_error` | `console -streaming_mode sse` | `fail\n` |
| `hitl_accept` | `console -streaming_mode none` | `transfer\nyes\n` |
| `hitl_reject` | `console -streaming_mode none` | `transfer\nno\n` |
| `sse_hitl_accept` | `console -streaming_mode sse` | `transfer\ny\n` |
| `eof_empty` | `console -streaming_mode none` | (empty) |
| `eof_partial_line` | `console -streaming_mode none` | `hello` (no newline) |

All console cases exit 0 with `error` null and empty stderr. Transcripts ADK-Go produced:

- every run starts with `\n\nUser -> ` (`fmt.Println()` then `fmt.Print("\nUser -> ")`) and ends
  with `\nEOF detected, exiting...\n`;
- a turn prints `\nAgent -> ` then the text, then `\nUser -> `;
- SSE: partials are printed as they arrive and the final is skipped when it equals their
  concatenation (`sse_hello` prints `Hello! How can I help?` once); `sse_revised_final` prints
  `Draft answer.Final answer.`;
- `agent_error`: `\nAgent -> \nAGENT_ERROR: scripted model failure\n\nUser -> `;
- HITL: the function-call turn prints `\nAgent -> ` (no text), then `\n` (`fmt.Println()`),
  `Agent -> Confirm transfer_funds?\n`, `  Type 'yes' to confirm, anything else to reject.\n`,
  `User -> `; the answer turn prints `\nAgent -> Transfer done.` (accept: `yes`, `y`) or
  `\nAgent -> Transfer cancelled.` (reject), then `\nUser -> `;
- `eof_partial_line`: a final line without `\n` is dropped (Go's `ReadString('\n')` returns it
  with `io.EOF`, which the console treats as EOF).

### CLI cases

| case | launcher | args |
|---|---|---|
| `help_top` | full | `-h` |
| `help_console` | full | `console -help` |
| `help_web` | full | `web -h` |
| `help_web_api` | full | `web api -h` |
| `help_web_webui` | full | `web webui -h` |
| `help_web_a2a` | full | `web a2a -h` |
| `help_web_pubsub` | full | `web pubsub -h` |
| `help_web_eventarc` | full | `web eventarc -h` |
| `help_prod` | prod | `-h` |
| `unknown_flag` | full | `web -nope` |
| `unknown_flag_console` | full | `-nope` |
| `flag_needs_argument` | full | `web -port` |
| `bad_flag_syntax` | full | `web ---port 1` |
| `bad_int` | full | `web -port abc` |
| `bad_int_out_of_range` | full | `web -port 99999999999999999999` |
| `bad_int64` | full | `web -max_request_body_size 1k` |
| `bad_duration` | full | `web -read-timeout 5x` |
| `bad_bool` | full | `web -h2c=maybe` |
| `bad_streaming_mode` | full | `console -streaming_mode bidi` |
| `web_no_sublaunchers` | full | `web` |
| `duplicate_keyword` | full | `web api api` |
| `unparsed_args_web` | full | `web api extra args` |
| `unparsed_args_console` | full | `console extra` |
| `unparsed_args_prod` | prod | `console` |
| `api_bad_flag` | full | `web api -trace_capacity x` |
| `pubsub_max_retries_zero` | full | `web pubsub -trigger_max_retries 0` |
| `pubsub_negative_base_delay` | full | `web pubsub -trigger_base_delay -1s` |
| `eventarc_max_delay_zero` | full | `web eventarc -trigger_max_delay 0s` |
| `eventarc_max_concurrent_runs_zero` | full | `web eventarc -trigger_max_concurrent_runs 0` |
| `oidc_audience_empty` | full | `web eventarc -oidc_audience=` |
| `oidc_audience_without_accounts` | full | `web pubsub -oidc_audience https://example.com/push` |
| `oidc_accounts_without_audience` | full | `web pubsub -oidc_service_accounts push@example.iam.gserviceaccount.com` |
| `oidc_audience_dash` | full | `web pubsub -oidc_audience -oidc_service_accounts push@example.iam.gserviceaccount.com` |
| `oidc_accounts_empty_entry` | full | `web pubsub -oidc_audience https://example.com/push -oidc_service_accounts push@example.iam.gserviceaccount.com,` |

Every CLI case exits 1 with empty stdout. A flag-parse failure writes the `flag` package's error
line (none for `-h`), `Usage of <FlagSet name>:` and the defaults to stderr first; then every case
ends with the `Run failed: …` log record. `web_no_sublaunchers` logs the three in-memory service
lines before failing (the error comes from `buildRouter`, after `applyServiceDefaults`).

## Web fixtures (`wire/`)

One server per script, started fresh, its steps run in order (later steps see earlier state). The
file is `wire/<server>_<NN>_<step>.json`: `NN` = `00` for the startup, `01`… for the exchanges in
script order, `99` for the shutdown.

```json
{
  "name": "<server>_<NN>_<step>",
  "server": "<server>",
  "launcher": "full|prod",
  "args": ["web", "-port", "<port>", "..."],
  "kind": "startup|exchange|shutdown",
  "request": <request|null>,
  "response": <response|null>,
  "log": ["<ts> GET /health <elapsed>"],
  "exit_status": <int|null>,
  "stdout": <string|null>,
  "error": <string|null>
}
```

- `args` repeats the server's command line in every file of the server; `<port>` stands for the
  port the probe picked (a free loopback port: listen on `127.0.0.1:0`, read it, close).
- `startup` (`request`, `response` null): `log` is every stderr line written before the server
  accepted its first connection.
- `exchange`: `request`, `response`, and `log` = the stderr lines written while the exchange ran
  (after the response, the probe waits until stderr has been quiet for 150 ms).
- `shutdown` (`request`, `response` null): the probe sends SIGTERM (replay: cancels the token) and
  waits for the child to exit; `log` = the remaining stderr lines, `exit_status` the exit status,
  `stdout` the child's whole stdout, `error` as for process fixtures. `exit_status`, `stdout` and
  `error` are `null` in the other kinds.

### Request

```json
{"method": "GET", "target": "/path?query", "headers": [["Host", "127.0.0.1:<port>"], ["Name", "value"]], "body": ""}
```

The probe opens a new TCP connection per exchange and writes exactly
`<method> <target> HTTP/1.1\r\n`, then each header as `<name>: <value>\r\n` in the listed order
(`<port>` replaced), `\r\n`, and `body`; then it reads to EOF. `headers` is the complete list sent:
`Host: 127.0.0.1:<port>` first (unless the step names its own `Host`, `host_forbidden`), the step's
headers, `Content-Length: <len(body)>` when the body is non-empty or the method is POST, PUT or
PATCH, and `Connection: close` last. `target` is sent verbatim (non-clean paths and `%2F` are not
touched by any client).

### Response

```json
{
  "status": 301, "status_text": "Moved Permanently",
  "headers": [["Content-Type", "text/html; charset=utf-8"], ["Date", "<date>"], ["Location", "/api/health?q=1"]],
  "body": "<a href=\"/health?q=1\">Moved Permanently</a>.\n\n",
  "body_length": 46, "body_sha256": null, "base_href": null
}
```

- `status`, `status_text`: the status line's code and reason phrase as sent on the wire.
- `headers`: parsed from the raw bytes (not from Go's client, which drops `Connection: close`),
  names in Go canonical form, sorted by name then value, every header kept (`Connection`,
  `Content-Length`, `Transfer-Encoding` included) after N7. Compare names case-insensitively.
- `body`: the body after transfer decoding, as a string, when it is valid UTF-8 and at most 8192
  bytes, after N2, N5 and N6; otherwise `null`, and `body_sha256` is the lowercase hex SHA-256 of
  the raw body (the web UI bundle's large files, served byte-for-byte). A HEAD response's body is
  `""`.
- `body_length`: the raw (pre-normalization) body length in bytes, or `null` when the raw body
  holds a wall-clock time whose text width varies: an N5 time field (`lastUpdateTime`/`timestamp`)
  whose value has a fractional-seconds part (contains `.`). a2a-go stamps task status with
  `time.Now()` (not the injected time provider), and RFC 3339 drops trailing zero digits, so such a
  body's raw length varies run to run; only the A2A send fixtures (`full_29`–`full_31`) are `null`.
  Whole-second values keep the integer: the child's fixed clock and adkrest's `lastUpdateTime`
  (`time.Time.Unix()`, integer seconds, wall-clock but fixed width; `full_19`–`full_22`).
- `base_href`: for a `text/html` response, the first `<base …>` tag in the raw body
  (`(?is)<base\s[^>]*>`), else `null`.

### Servers and steps

`full` — `full` preset, `web -port <port> -max_request_body_size 1024 -allow_origins
http://allowed.example api webui a2a pubsub eventarc`:

| NN | step | request | Go status |
|---|---|---|---|
| 01 | `health_get` | `GET /health` | 200 |
| 02 | `health_head` | `HEAD /health` | 200 |
| 03 | `health_wrong_method` | `DELETE /health` | 405 (gorilla, empty body, no log) |
| 04 | `health_trailing_slash` | `GET /health/` | 301 → `/health` (StrictSlash, logged) |
| 05 | `non_clean_path` | `GET /api/../health?x=1` | 301 → `/health?x=1` (clean path, no log) |
| 06 | `non_clean_path_post` | `POST //api/list-apps` (empty body) | 301 → `/api/list-apps` (no 308, no log) |
| 07 | `unmatched_path` | `GET /no/such/path` | 404 |
| 08 | `unmatched_path_cross_origin` | `GET /no/such/path`, `Origin: http://evil.example` | 404 (no origin guard) |
| 09 | `origin_forbidden` | `GET /api/list-apps`, `Origin: http://evil.example` | 403 `Forbidden: origin not allowed` |
| 10 | `origin_allowed_flag` | `GET /api/list-apps`, `Origin: http://allowed.example` | 200 |
| 11 | `origin_allowed_webui` | `GET /api/list-apps`, `Origin: http://localhost:8080` | 200 |
| 12 | `host_forbidden` | `GET /health`, `Host: evil.example` | 403 `Forbidden: host not allowed` |
| 13 | `cors_preflight` | `OPTIONS /api/apps/assistant/users/u1/sessions`, `Origin: http://localhost:8080`, `Access-Control-Request-Method: POST`, `Access-Control-Request-Headers: content-type` | 200 |
| 14 | `cors_preflight_flag_origin` | `OPTIONS /api/list-apps`, `Origin: http://allowed.example`, `Access-Control-Request-Method: GET` | 200 |
| 15 | `api_list_apps` | `GET /api/list-apps` | 200 |
| 16 | `api_health` | `GET /api/health` | 200 (the REST server's own `/health`) |
| 17 | `api_bare_prefix` | `GET /api` | 404 (REST server, CORS headers) |
| 18 | `api_prefix_word` | `GET /apifoo` | 404 (not the API mount) |
| 19 | `api_session_create` | `POST /api/apps/assistant/users/u1/sessions/s1`, JSON `{"state":{"color":"blue"}}` | 200 |
| 20 | `api_session_create_generated_id` | `POST /api/apps/assistant/users/u1/sessions` (empty body) | 200 |
| 21 | `api_session_get_trailing_slash` | `GET /api/apps/assistant/users/u1/sessions/s1/` | 200 |
| 22 | `api_session_post_trailing_slash` | `POST /api/apps/assistant/users/u1/sessions/s2/`, JSON `{}` | 200 (creates `s2`) |
| 23 | `api_redirect_get` | `GET /api/health%2F?q=1` | 301 → `/api/health?q=1` (rewritten) |
| 24 | `api_redirect_head` | `HEAD /api/health%2F` | 301 → `/api/health` |
| 25 | `api_redirect_post` | `POST /api/health%2F` (empty body) | 405 `Allow: GET, HEAD` |
| 26 | `api_wrong_method` | `PUT /api/list-apps` (empty body) | 405 `Allow: GET, HEAD` |
| 27 | `api_body_too_large` | `POST /api/apps/assistant/users/u1/sessions`, JSON `{"state":{"padding":"<2048 × a>"}}` (2071 bytes) | 400 |
| 28 | `a2a_card` | `GET /.well-known/agent-card.json` | 200 |
| 29 | `a2a_v1_send` | `POST /a2a/v1/invoke`, `A2A-Version: 1.0`, JSON-RPC `SendMessage` | 200 |
| 30 | `a2a_v1_send_without_version` | as 29 without `A2A-Version` | 200 |
| 31 | `a2a_v03_send` | `POST /a2a/invoke`, JSON-RPC 0.3 `message/send` | 200 |
| 32 | `webui_root_redirect` | `GET /` | 302 → `/ui/` |
| 33 | `webui_bare_redirect` | `GET /ui` | 301 → `/ui/` |
| 34 | `webui_index` | `GET /ui/` | 200, `base_href` `<base href="/ui/">` |
| 35 | `webui_index_head` | `HEAD /ui/` | 200 |
| 36 | `webui_index_file` | `GET /ui/index.html` | 301 → `./` (`http.FileServer`) |
| 37 | `webui_asset_svg` | `GET /ui/adk_favicon.svg` | 200 `image/svg+xml` |
| 38 | `webui_asset_js` | `GET /ui/polyfills-5CFQRCPP.js` | 200 `text/javascript; charset=utf-8` |
| 39 | `webui_asset_css` | `GET /ui/styles-LBC36Z6S.css` | 200 `text/css; charset=utf-8` |
| 40 | `webui_asset_if_modified_since` | as 37 with `If-Modified-Since: Sat, 01 Jan 2000 00:00:00 GMT` | 200 (zero modtime: no 304) |
| 41 | `webui_missing_js` | `GET /ui/missing.js` | 404 |
| 42 | `webui_deep_link` | `GET /ui/apps/assistant/sessions` | 200 (index, SPA fallback) |
| 43 | `webui_dir_no_listing` | `GET /ui/assets/` | 404 |
| 44 | `webui_runtime_config` | `GET /ui/assets/config/runtime-config.json` | 200 `{"backendUrl":"http://localhost:8080/api"}` |
| 45 | `webui_post` | `POST /ui/` (empty body) | 405 (gorilla, no log) |
| 46 | `pubsub_trigger` | `POST /api/apps/assistant/trigger/pubsub`, JSON push body | 200 `{"status":"success"}` |
| 47 | `pubsub_trigger_bad_json` | as 46 with body `{` | 400 |
| 48 | `pubsub_trigger_body_too_large` | as 46 with `{"message":{"data":"<2048 × a>"},"subscription":"s"}` | 400 |
| 49 | `pubsub_trigger_get` | `GET /api/apps/assistant/trigger/pubsub` | 404 from the REST server (falls through to the API mount) |
| 50 | `eventarc_trigger_structured` | `POST /api/apps/assistant/trigger/eventarc`, `Content-Type: application/cloudevents+json`, a storage CloudEvent | 200 |
| 51 | `eventarc_trigger_binary` | `POST /api/apps/assistant/trigger/eventarc`, `Ce-Id`/`Ce-Source`/`Ce-Specversion`/`Ce-Type` (Pub/Sub message published) headers, the push body | 200 |

`oidc` — `full`, `web -port <port> pubsub -oidc_audience https://example.com/push
-oidc_service_accounts push@example.iam.gserviceaccount.com eventarc`:

| NN | step | request | Go status |
|---|---|---|---|
| 01 | `pubsub_no_token` | `POST /api/apps/assistant/trigger/pubsub`, push body | 401 `unauthorized` |
| 02 | `pubsub_malformed_token` | as 01 with `Authorization: Bearer not-a-jwt` | 401 |
| 03 | `eventarc_unauthenticated` | `POST /api/apps/assistant/trigger/eventarc`, structured CloudEvent | 200 |

`noprefix` — `full`, `web -port <port> api -path_prefix=` (catch-all API mount):

| NN | step | request | Go status |
|---|---|---|---|
| 01 | `health` | `GET /health` | 200 (the launcher's route, registered first) |
| 02 | `list_apps` | `GET /list-apps` | 200 |
| 03 | `list_apps_trailing_slash` | `GET /list-apps/` | 200 |
| 04 | `unknown_path` | `GET /no/such/path` | 404 (REST server, CORS headers, logged) |
| 05 | `root` | `GET /` | 404 (REST server) |

`prod` — `prod`, `web -port <port> a2a api`:

| NN | step | request | Go status |
|---|---|---|---|
| 01 | `health` | `GET /health` | 200 |
| 02 | `api_list_apps` | `GET /api/list-apps` | 200 |
| 03 | `webui_absent` | `GET /ui/` | 404 |

Request bodies: the Pub/Sub push body is
`{"message":{"data":"aGVsbG8=","messageId":"m1","attributes":{"k":"v"}},"subscription":"projects/p/subscriptions/s"}`;
the structured CloudEvent
`{"specversion":"1.0","type":"google.cloud.storage.object.v1.finalized","source":"//storage.googleapis.com/projects/_/buckets/b","id":"e1","data":{"name":"file.txt"}}`;
A2A v1 `{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"msg-1","role":"ROLE_USER","parts":[{"text":"hello"}]}}}`;
A2A 0.3 `{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"kind":"message","messageId":"msg-2","role":"user","parts":[{"kind":"text","text":"hello"}]}}}`.
JSON requests carry `Content-Type: application/json` unless stated.

## Normalization

Applied by the probe before writing and by the replay to its own output before comparing.

- **N1 log timestamps.** In stderr and log text, every line-start `YYYY/MM/DD HH:MM:SS ` (regex
  `(?m)^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} `) becomes `<ts> `. A `log.Println()` line is `<ts> `
  (with the trailing space).
- **N2 port.** `127.0.0.1:<P>`, `localhost:<P>` and `port:<P>` (regex
  `(127\.0\.0\.1|localhost|port):<P>\b`, `<P>` the live port) become `…:<port>`, in log lines,
  response header values and inline bodies.
- **N3 request log elapsed.** A log line `<ts> <METHOD> <RequestURI> <duration>` (the web
  launcher's request logger, `%s %s %s`; regex
  `^(<ts> [A-Z]+ \S+ )(?:\d+(?:\.\d+)?(?:ns|µs|ms|s|m|h))+$`) ends in `<elapsed>` instead of the
  Go `time.Duration` text.
- **N4 user-message order.** In a `startup` log, the lines between `<ts> Web servers starts on …`
  and the next `<ts> ` line (the active sub-launchers' `UserMessage` output, which Go prints in
  map order) are stably sorted by the sub-launcher keyword after `<ts> ` and spaces
  (`^<ts> +([a-z0-9]+):`), in the preset's declaration order: full `webui, a2a, pubsub, eventarc,
  api`; prod `api, a2a`. A sub-launcher's own lines keep their order. (gadk prints them in
  declaration order: ledgered.)
- **N5 times in bodies.** In an inline body, `"lastUpdateTime":<value>` and `"timestamp":<value>`
  (value a JSON number or string; regex `"(lastUpdateTime|timestamp)":(-?[0-9][0-9.eE+-]*|"[^"]*")`)
  become `"<key>":"<t>"`. A value with fractional seconds also makes `body_length` `null` (section "Response").
- **N6 UUIDs in bodies.** In an inline body, every UUID (regex
  `[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`, compared
  case-insensitively) becomes `<uuid-N>`, N = 1, 2, … in order of first appearance in that body
  (one numbering per fixture).
- **N7 headers.** `Date`'s value becomes `<date>`; `Content-Length`'s value becomes `<n>` when N2,
  N5 or N6 changed the body; other values get N2.

Nothing else varies run to run: bodies are compared as strings (JSON key order and whitespace
included), large bodies by hash.

## Replay

The probe's self-check is the replay Task 12 performs, driven only by the fixture files:

- **Process fixtures**: for each file, run the launcher of `launcher` with `args` and `stdin`,
  capture, normalize (N1), rebuild the fixture JSON and compare.
- **Web fixtures**: group by `server`; per server, sorted by `name`: start the server from the
  `00_startup` file's `launcher` and `args` (fresh port for `<port>`), wait until it accepts a
  connection, take the startup log (N1–N4); for each `exchange` file send its `request` (`<port>`
  replaced) on a new connection and record response and log (N1–N3, N5–N7); then cancel (SIGTERM)
  and take the `shutdown` fields. Compare every rebuilt fixture with the file.

A Rust replay compares the same fields after the same normalization; JSON bodies may be compared
as parsed values where a declared delta says so (for example the A2A card, whose a2a-go field
order and 0.3 union fields gadk does not reproduce).

## Files

- `main.go`: the probe and the launched program.
- `go.mod`, `go.sum`: module `gadk/gen-go-launcher`; `google.golang.org/adk/v2 =>
  ../../../adk-go` (gorilla/mux v1.8.1 and a2a-go v2.6.0 come from ADK-Go's pins).
