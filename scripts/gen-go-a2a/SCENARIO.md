# gen-go-a2a scenario

`scripts/gen-go-a2a` runs ADK-Go's A2A v2 executor (`server/adka2a/v2`, ADK-Go at
`docs/parity/ADK_GO_COMMIT`) behind a2a-go v2.7.0's `a2asrv` (JSON-RPC handler, REST handler and a
static agent card) on loopback listeners (`httptest.NewServer`). It drives the server with raw HTTP
requests (server direction) and runs ADK-Go's v2 remote agent (`agent/remoteagent/v2`) against it
(client direction). Every scenario below is written, normalized, to
`crates/gadk-a2a/testdata/wire/<scenario>.json`. gadk-a2a's `tests/wire_replay.rs` (plan Task 9)
replays both directions against gadk-a2a and compares after applying the same normalization and the
scenario's declared deltas (`<scenario>.deltas.json`, written by Task 9, never by this probe).

Before it writes a fixture the probe replays it the way Task 9 will (section "Replay"): server units
against a fresh ADK-Go server, client units against a fake server serving the fixture's responses to
a fresh ADK-Go remote agent. The normalized replay must equal the fixture, or the probe exits
non-zero. Run to run, every fixture is byte-identical.

Regenerate (needs ADK-Go checked out beside this repository, as for the other probes):

```
cd scripts/gen-go-a2a && go run .
```

With ADK-Go elsewhere, point a temporary modfile at it (`go.sum` must sit next to it):

```
cd scripts/gen-go-a2a && tmp="$(mktemp -d)" && \
  sed "s#=> ../../../adk-go#=> $ADK_GO_DIR#" go.mod > "$tmp/go.mod" && cp go.sum "$tmp/" && \
  go run -modfile="$tmp/go.mod" .
```

It deletes `crates/gadk-a2a/testdata/wire/*.json` except `*.deltas.json` first and prints
`wrote 7 fixtures (25 units) to ../../crates/gadk-a2a/testdata/wire`.

## Server layout

One `http.ServeMux` per server, every request wrapped (outermost first) by the recorder and by the
fixed server-side providers:

| path | handler |
|---|---|
| `/.well-known/agent-card.json` | `a2asrv.NewStaticAgentCardHandler(card)` |
| `/jsonrpc` | `a2asrv.NewJSONRPCHandler(a2asrv.NewHandler(executor))` |
| `/rest/…` | `http.StripPrefix("/rest", a2asrv.NewRESTHandler(<same handler>))` (so `/rest/message:send`, `/rest/message:stream`, `/rest/tasks/{id}`, `/rest/tasks/{id}:cancel`) |

`a2asrv.NewHandler` runs with its defaults (in-memory task store, no authenticator, no push). The
executor is `adka2a.NewExecutor(ExecutorConfig{RunnerConfig: runner.Config{AppName: <agent name>,
Agent: <agent>, SessionService: session.InMemoryService()}, RunConfig: agent.RunConfig{StreamingMode:
<per scenario>}, OutputMode: <per scenario>})` and nothing else (no callbacks, no converters, no
cleanup callback). The A2A user is therefore `A2A_USER_<contextId>` and the session ID the context
ID.

The card (`newCard`):

```json
{
  "name": "<agent name>", "description": "<agent description>", "version": "1.0.0",
  "supportedInterfaces": [
    {"url": "<base>/jsonrpc", "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
    {"url": "<base>/rest", "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"}
  ],
  "capabilities": {"streaming": true},
  "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
  "skills": "<adka2a.BuildAgentSkills(agent)>"
}
```

(`a2a.NewAgentInterface` fills `protocolVersion` `1.0`; a2a-go marshals `capabilities` with
`"pushNotifications": false`.) A card "naming only `jsonrpc`" (or `rest`) has just that interface.

## Fixture format

One file per scenario, `<scenario>.json`:

```json
{
  "scenario": "<name>",
  "server": {"jsonrpc": <server unit>, "rest": <server unit>},
  "client": {"<unit name>": <client unit>, ...}
}
```

`client` is absent for `cancel_input_required_with_child` (its client direction is the child
exchanges inside the server units). Each unit is normalized on its own (section "Normalization"):
placeholders are numbered per unit, so `<task-1>` in `server.jsonrpc` and in `server.rest` are
unrelated values.

### Exchange

```json
{
  "request": {"method": "POST", "path": "/jsonrpc", "headers": {"Name": "value"}, "body": <JSON|string|null>},
  "response": {"status": 200, "headers": {"Name": "value"}, "body": <JSON|string|null>, "sse": [<JSON>, ...]},
  "child": [<exchange>, ...]
}
```

- Exchanges are recorded at the server, by a handler wrapper, in completion order.
- `request.path` is `URL.RequestURI()` (path and query as received; REST paths include `/rest`).
- `request.headers`: every request header except `Host`, `Content-Length`, `Accept-Encoding`,
  `User-Agent` and `Connection`, names in Go canonical form (`A2a-Version`, `Content-Type`), multiple
  values joined with `", "`. Compare names case-insensitively.
- `request.body`: the body parsed as JSON; `null` when empty; the raw string when it is not JSON.
- `response.headers`: the headers the handler had set when it wrote the status (a2a-go's handlers
  and the static card handler), not the ones `net/http` adds afterwards (`Date`, `Content-Length`,
  `Transfer-Encoding`).
- `response.body`: as `request.body`. For a `text/event-stream` response it is `null` and `sse`
  holds the stream instead: the body split on `"\n\n"`; per frame the `data:` lines (one optional
  space after the colon stripped) joined with `"\n"` and parsed as JSON; frames without `data:` (the
  `id:`-only remainder, keep-alives) dropped. a2a-go frames each payload as
  `id: <uuid>\ndata: <json>\n\n`; the `id:` values are not recorded. `sse` is omitted for
  non-stream responses.
- `child` (only in `cancel_input_required_with_child`): the exchanges the child server recorded
  from the start of this exchange until the probe's wait for it ended (section "Scenarios"); omitted
  when empty.

### Server unit

```json
{"exchanges": [<exchange>, ...], "session_events": [<session.Event JSON>, ...]}
```

`exchanges` are the requests the probe sent, in order, one fresh server (and child) per unit. A
request built from an earlier response (a task or context ID, a function-call ID) carries that value,
so in the fixture it is a placeholder (section "Replay"). JSON-RPC request IDs are the 1-based step
number. `session_events` (only in `cancel_input_required_with_child`): the root agent's stored session
(`AppName "root"`, user `A2A_USER_<contextId>`, session `<contextId>` of the first task) after the
last step, each event `json.Marshal(*session.Event)` (the same Go JSON as
`scripts/gen-go-events`).

The probe sends exactly the headers a2a-go's own client (`a2aclient` v2.7.0) sends:

| operation | JSON-RPC (`POST /jsonrpc`, method) | REST | headers |
|---|---|---|---|
| send | `SendMessage` | `POST /rest/message:send` | `A2A-Version: 1.0`, `Content-Type: application/json`; REST also `Accept: application/a2a+json, application/json` |
| stream | `SendStreamingMessage` | `POST /rest/message:stream` | `A2A-Version: 1.0`, `Content-Type: application/json`, `Accept: text/event-stream, application/json` |
| get | `GetTask` (`params: {"id": <task>}`) | `GET /rest/tasks/<task>` (no body) | JSON-RPC: `A2A-Version`, `Content-Type`; REST: `A2A-Version: 1.0`, `Accept: application/a2a+json, application/json` |
| cancel | `CancelTask` (`params: {"id": <task>}`) | `POST /rest/tasks/<task>:cancel` (no body) | as get |
| card | — | `GET /.well-known/agent-card.json` (no headers) | — |

JSON-RPC bodies are `{"jsonrpc": "2.0", "id": <n>, "method": <method>, "params": <params>}`; REST
send/stream bodies are the params (`{"message": <message>}`). Messages the probe writes are
`{"messageId": "msg-<k>", "role": "ROLE_USER", "parts": [...]}` (literal, non-UUID IDs, so they
survive normalization), plus `taskId`/`contextId` when the step says so.

### Client unit

```json
{
  "card": <normalized agent card>,
  "runs": [
    {"streaming_mode": "none|sse", "new_message": <genai.Content JSON>,
     "exchanges": [<exchange>, ...], "events": [<session.Event JSON or {"error": "<text>"}>, ...]}
  ]
}
```

- One ADK-Go remote agent per unit: `remoteagent.NewA2A(A2AConfig{Name: "remote", Description:
  "Remote A2A agent.", AgentCard: <card>})`, or with `AgentCardProvider:
  remoteagent.NewAgentCardProvider("<base>")` for the `card_provider` unit, nothing else set. It runs
  as the root of `runner.New(runner.Config{AppName: "client", Agent: remote, SessionService:
  session.InMemoryService()})` in session `client`/`user`/`s1` (created first), one
  `Runner.Run(ctx, "user", "s1", new_message, agent.RunConfig{StreamingMode: <mode>})` per run, in
  order, in the same session.
- `card` (static-card units only): the card the agent was built with; it names only the unit's
  binding (`jsonrpc` or `rest`), so the a2aclient factory uses that binding.
- `exchanges`: what the server recorded during the run (the agent's card fetch, send or stream, and
  any cleanup `CancelTask`), after waiting until no request is in flight.
- `events`: every event `Runner.Run` yielded, `json.Marshal(*session.Event)`; a yielded error is
  `{"error": err.Error()}` (none occur in these fixtures).
- `new_message` is `json.Marshal(*genai.Content)`.

## Normalization

Applied to one unit document (the JSON above, parsed into plain JSON values) with the unit's base
URLs; gadk-a2a must apply exactly this to its own unit before comparing. JSON is compared as parsed
values (plan D5): key order never matters.

1. **Base URLs.** In every string value, replace each live base URL (`http://127.0.0.1:<port>`, no
   trailing slash) by its placeholder: the server under test `<base>`, the child server
   (`cancel_input_required_with_child`) `<child-base>`. Longer URLs are replaced first.
2. **Timestamps.** The value of every object key named exactly `timestamp`, at any depth, becomes
   the string `"<t>"` (a2a `TaskStatus.timestamp`, `session.Event.timestamp`). No other value holds a
   wall-clock time.
3. **IDs.** UUID = the regex `[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`
   (a2a-go and a2a-rs both mint UUIDv7 task/context/message/artifact/JSON-RPC IDs; ADK IDs are
   UUIDs too, sometimes prefixed: `e-<uuid>` invocation IDs, `adk-<uuid>` function-call IDs). Visit
   every string value once, in canonical order: depth-first, object keys sorted bytewise ascending,
   array elements in order. For each string `s` stored under object key `k` of object `o` (for an
   array element: the key of the array and the object holding the array):
   - if all of `s` is a UUID: replace it by the placeholder already assigned to that value, else
     assign `<kind-N>` where `kind` is
     - `task` if `k` is `taskId`, `a2a:task_id` or `referenceTaskIds`, or if `k` is `id` and `o`
       has both a `status` and a `contextId` key (an a2a `Task`);
     - `ctx` if `k` is `contextId` or `a2a:context_id`;
     - `msg` if `k` is `messageId`;
     - `artifact` if `k` is `artifactId`;
     - `id` otherwise (session event IDs, JSON-RPC request IDs, …);
     and `N` is 1 + the number of placeholders of that kind assigned so far in this unit;
   - otherwise replace every UUID inside `s`, left to right, by its assigned placeholder, assigning
     `<id-N>` to a UUID seen for the first time (`A2A_USER_<ctx-1>`, `e-<id-4>`, `adk-<id-1>`,
     `/rest/tasks/<task-1>:cancel`).

   Object keys are never rewritten (the probe fails if a key contains a UUID). Placeholder kinds
   come from the first sighting, so a value first seen inside a longer string stays `<id-N>`
   everywhere.

Numbers stay JSON numbers; strings that are not IDs, URLs or timestamps are compared verbatim.

## Replay

Task 9 replays each unit; the probe performs the same replay as its self-check.

**Placeholder binding (`align`).** Walk a normalized fixture value and an actual (not normalized)
value in parallel: objects by key (keys missing on either side are skipped), arrays by index (up to
the shorter length), mismatched shapes skipped. For a fixture string containing placeholders, turn it
into an anchored pattern (literal text escaped, each `<kind-N>` → one UUID capture group); if the
actual string matches, bind each placeholder to its captured value unless already bound.

**Denormalize.** Replace in every string: `"<t>"` (whole string) → `2026-01-02T03:04:05Z`; `<base>`
(and `<child-base>`) → the replay server's base URL; each `<kind-N>` → its bound value, or a fresh
UUID (remembered for the rest of the unit) when unbound.

**Server direction.** Start the scenario's server(s) fresh. For request `i`: bind placeholders by
aligning fixture exchanges `0..i-1` with the actual exchanges `0..i-1` (requests, responses and
children); denormalize fixture request `i` with them; send its method, path, headers and body. After
the last request (and the scenario's child waits), normalize the actual unit and compare it to the
fixture unit.

**Client direction.** Serve the unit's exchanges, all runs concatenated, in order from a fake server:
on request `i`, align fixture request `i` with the actual request (this binds the IDs the client
minted, e.g. its `messageId` and JSON-RPC `id`, which the recorded response echoes); fail unless
method and path equal the denormalized fixture's; answer with the denormalized response: its status,
headers, and either the body (JSON re-serialized) or, for `sse`, one `data: <json>\n\n` frame per
payload, flushed per frame (no `id:` lines). The static card is the fixture `card` with `<base>`
replaced by the fake server's URL; the `card_provider` unit fetches it from the fake server (the
fixture's card response, denormalized, so its interfaces point at the fake server). Each run's
`new_message` is built the same way as the probe builds it (section "Scenarios"), from the events of
the earlier runs. Every recorded response must be consumed. Normalize the actual unit (base = fake
server URL) and compare.

## Fixed providers

`platform.WithTimeProvider` and `platform.WithUUIDProvider` (mutex-guarded):

- server side: one pair per server, installed on every request's context inside the recorder;
  clock: `2026-01-02T03:04:05Z` + n s for the n-th read (n = 0, 1, …); UUIDs:
  `00000000-0000-4000-8000-%012d` (n = 1, 2, …);
- client side: one pair per client unit, on the context of every `Runner.Run`; same clock, UUIDs
  `00000000-0000-4000-9000-%012d`.

The distinct variants keep server- and client-minted ADK IDs apart within a unit. Normalization
removes every value the providers produce, so gadk-a2a does not have to consume them in the same
order; they only make the probe's own runs deterministic where a2a-go does not mint the value.

## Agents and models

Scripted model (`scriptedModel`): `Name()` is the model name below; `GenerateContent(ctx, req,
stream)` yields, in order, the responses its reply function returns for `(req, stream)`, or one
`(nil, err)`. `final(parts…)` = `LLMResponse{Content: {role: "model", parts}, UsageMetadata:
{promptTokenCount: 10, candidatesTokenCount: 5, totalTokenCount: 15}, FinishReason: STOP}`;
`chunk(text)` = `LLMResponse{Content: {role: "model", parts: [{text}]}, Partial: true}`. `last` is the
last element of `req.Contents`.

- `assistant`: `llmagent`, Description `Answers greetings.`, Instruction `Answer briefly.`, model
  `scripted-assistant`: always `[final(text "Hello! How can I help?")]`.
- `weather`: `llmagent`, Description `Reports the weather.`, Instruction `Use get_weather to
  answer.`, tool `get_weather` (`functiontool`, Description `Returns the forecast for a city.`, args
  `{"city": string}`, result `{"forecast": "sunny"}`), model `scripted-weather`:
  - `last` has a function response: `stream` ? `[chunk "It is ", chunk "sunny.", final(text "It is
    sunny.")]` : `[final(text "It is sunny.")]`;
  - else: `stream` ? `[chunk "Let me ", chunk "check.", final(text "Let me check.", functionCall
    get_weather {"city": "Paris"})]` : `[final(...)]` (the same final; no ID, ADK assigns
    `adk-<uuid>`).
- `approver` (ADK-Go `a2a_e2e_test.go` `newInputRequestingAgent`): `llmagent`, Description
  `Requests approval before acting.`, Instruction `Ask for approval first.`, tool `request_approval`
  (`functiontool`, `IsLongRunning: true`, Description `Request approval before proceeding.`, args
  `map[string]any`, result `{"status": "pending", "ticket_id": "ticket-1"}`), model
  `scripted-approver`, by the function responses in `last`:
  none → `[final(text "need to request approval first!", functionCall request_approval {})]`;
  exactly one with `response.status == "pending"` → `[final(text "waiting for user's
  approval...")]`; otherwise → `[final(text "Task complete!")]`.
- `broken`: `llmagent`, Description `Always fails.`, Instruction `Fail.`, model `scripted-broken`
  always errors `scripted model failure`.
- `root` (`newRoot(child)`): `llmagent`, Description `Delegates to the child agent.`, Instruction
  `Transfer to child.`, SubAgents `[child]`, model `scripted-root`: always `[final(text
  "transferring to child", functionCall transfer_to_agent {"agent_name": "child"})]`. `child` is
  `remoteagent.NewA2A(A2AConfig{Name: "child", Description: "Remote approver.", AgentCard: <child
  server card naming only jsonrpc>})`.
- `pipeline` (agent-card tree): `sequentialagent` Name `pipeline`, Description `Drafts, checks,
  polishes and stamps a reply.`, SubAgents:
  1. `drafter`: `llmagent`, Description `Writes a draft.`, Instruction `You write a draft of your
     reply.`, tool `lookup` (`functiontool`, no description, args `{"query": string}`, result
     `{"found": true}`), model `scripted-drafter`: `[final(text "Draft ready.")]`;
  2. `checks`: `parallelagent`, no description, SubAgents `[fact_checker]` (`llmagent`, Description
     `Checks facts.`, Instruction `Check the facts.`, model `scripted-fact-checker`:
     `[final(text "Facts checked.")]`);
  3. `polisher`: `loopagent`, Description `Polishes the draft.`, MaxIterations 1, SubAgents
     `[editor]` (`llmagent`, no description, Instruction `You're the editor; polish your draft.`,
     model `scripted-editor`: `[final(text "Polished.")]`);
  4. `stamper`: `agent.New` custom agent, no description, Run yields one event
     `session.NewEvent(ic, ic.InvocationID())` with Author `stamper`, Branch `ic.Branch()`, Content
     `{role: "model", parts: [{text: "Stamped."}]}`.

No other agent options are set (no callbacks, output keys, schemas, `GenerateContentConfig`).

## Scenarios

Executor column: agent, `OutputMode` (`per-run` = `artifact-per-run`, the default; `per-event` =
`artifact-per-event`), `RunConfig.StreamingMode` (`none` default, `sse`). In the steps, `T(k)` is the
task step `k` created (`result.task` of a send, the `task` payload that opens a stream), `call(T)` the
`data.id` of the part with `metadata.adk_type == "function_call"` in `T.status.message.parts`.

| scenario | executor | server steps (both bindings) | client units / runs |
|---|---|---|---|
| `text_reply` | `assistant`, per-run, none | 1 send `msg-1` [text `hello`]; 2 get `T(1)`; 3 stream `msg-2` [text `hello`]; 4 send `msg-3` [text `hello again`] with `contextId` of `T(1)` | `jsonrpc`, `rest`: run 1 `none` text `hello`; run 2 `sse` text `hello again` |
| `streaming_partials_per_run` | `weather`, per-run, sse | 1 stream `msg-1` [text `weather in Paris?`]; 2 send `msg-2` [same text] | `jsonrpc`, `rest`: run 1 `sse` text `weather in Paris?` |
| `streaming_partials_per_event` | `weather`, per-event, sse | as `streaming_partials_per_run` | as `streaming_partials_per_run` |
| `input_required_resume` | `approver`, per-run, none | 1 send `msg-1` [text `Perform important task!`]; 2 send `msg-2` [text `Is it really necessary?`] with `taskId`/`contextId` of `T(1)`; 3 send `msg-3` [text `LGTM`, `{"data": {"id": call(T(1)), "name": "request_approval", "response": {"status": "approved", "ticket_id": "ticket-1"}}, "metadata": {"adk_type": "function_response"}}`] with `taskId`/`contextId` of `T(1)`; 4 get `T(1)` | `jsonrpc`, `rest`: run 1 `none` text `Perform important task!`; run 2 `none` user content `[{functionResponse: {id: <ID of the request_approval functionCall in run 1's events>, name: "request_approval", response: {"status": "approved", "ticket_id": "ticket-1"}}}]` |
| `failed_task` | `broken`, per-run, none | 1 send `msg-1` [text `hello`]; 2 stream `msg-2` [text `hello`] | `jsonrpc`, `rest`: run 1 `none` text `hello`; run 2 `sse` text `hello again` |
| `cancel_input_required_with_child` | `root` → remote `child`; root: per-run, sse; child server: `approver`, per-event, sse, card naming only `jsonrpc` | 1 send `msg-1` [text `Hello`], then wait until the child recorded 1 exchange; 2 cancel `T(1)`, then wait until the child recorded 1 more exchange (the executor's cleanup cancels the child task asynchronously); 3 get `T(1)` | none (the child exchanges are the remote agent's wire) |
| `agent_card_workflow` | `pipeline`, per-run, none | 1 card; 2 send `msg-1` [text `go`] | `card_provider`: card from `NewAgentCardProvider("<base>")` (both interfaces); run 1 `none` text `go` |

Text runs use `genai.NewContentFromText(<text>, "user")`.

Results ADK-Go produced (states as in the fixtures; artifacts in `server.jsonrpc`):

- `text_reply`: completed task with one artifact; stream: `task` (submitted) → `statusUpdate`
  working → one `artifactUpdate` → `statusUpdate` completed; step 4 is a new task in the same
  context.
- `streaming_partials_per_run`: stream: submitted, working, then artifact updates for two
  artifacts: the partial artifact `<artifact-1>` (artifact and part `metadata.adk_partial: true`;
  every chunk replaces it, no `append`) and the run artifact `<artifact-2>` (first update without
  `append`, later ones `append: true`); last, a reset of `<artifact-1>` to one empty data part
  `{"data": {}}` with `lastChunk: true` and event metadata `{"adk_partial": true}` only; then
  completed. The non-streaming send ends with 2 artifacts.
- `streaming_partials_per_event`: stream: one artifact per non-partial event. `<artifact-1>`: the
  first chunk without `append`, the second with `append: true`, then the final event's full content
  replacing it (no `append`) with `lastChunk: true`; `<artifact-2>` the tool response and
  `<artifact-3>` the answer the same way (single updates carry `lastChunk: true`); completed. The
  send ends with 3 artifacts.
- `input_required_resume`: input-required (status message: long-running `function_call` data part
  `adk_is_long_running: true` and its pending `function_response`); input-required again with the
  extra text part `no input provided for function call ID "adk-<id-1>"` (`metadata.validation_error:
  true`); completed with 2 artifacts; get returns the completed task with full history.
- `failed_task`: failed; status message one text part `agent run failed: scripted model failure`
  with `metadata.adk_is_error_message: true`, no artifacts; stream: submitted, working, failed.
- `cancel_input_required_with_child`: root input-required (child streamed submitted, working, two
  per-event artifacts, input-required); cancel answers the canceled task; the child then receives
  `CancelTask` for its task and answers canceled; get returns the canceled task.
- `agent_card_workflow`: the card (`BuildAgentSkills` over sequential, LLM with tool, parallel,
  loop and custom agents); completed task with one artifact holding the four sub-agents' texts.

## Files

- `main.go`: the probe.
- `go.mod`, `go.sum`: module `gadk/gen-go-a2a`; `google.golang.org/adk/v2 => ../../../adk-go`,
  `github.com/a2aproject/a2a-go/v2 v2.7.0` (ADK-Go's own pin).
