// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Command gen-go-adkrest runs ADK-Go's REST server (adkrest.NewServer) on a
// real loopback listener in three configurations (main, strict, custom), sends
// each configuration the fixed request script of SCENARIO.md and writes every
// exchange as a gadk-server wire fixture.
//
// Run from this directory: go run .
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"iter"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/log/global"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/agent/workflowagents/sequentialagent"
	"google.golang.org/adk/v2/artifact"
	"google.golang.org/adk/v2/memory"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/server/adkrest"
	"google.golang.org/adk/v2/server/authn"
	"google.golang.org/adk/v2/server/authz"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
)

const outDir = "../../crates/gadk-server/testdata/wire"

// t0 is the first value the fixed clock returns; every read advances it 1 s.
var t0 = time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)

// mainMaxPayload is ServerConfig.MaxPayloadSize of the main configuration.
const mainMaxPayload = 1024

// ---------------------------------------------------------------------------
// Fixed providers.

// fixedPlatform is one configuration's clock and UUID sequence.
type fixedPlatform struct {
	mu    sync.Mutex
	reads int
	ids   int
}

func (p *fixedPlatform) now() time.Time {
	p.mu.Lock()
	defer p.mu.Unlock()
	t := t0.Add(time.Duration(p.reads) * time.Second)
	p.reads++
	return t
}

func (p *fixedPlatform) uuid() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.ids++
	return fmt.Sprintf("00000000-0000-4000-8000-%012d", p.ids)
}

// withPlatform puts the fixed providers on every request's context.
func withPlatform(p *fixedPlatform, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := platform.WithTimeProvider(r.Context(), p.now)
		ctx = platform.WithUUIDProvider(ctx, p.uuid)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// ---------------------------------------------------------------------------
// Scripted model.

// scriptedModel answers from a fixed function of the request's last content.
type scriptedModel struct {
	name  string
	reply func(last *genai.Content) (*genai.Content, error)
}

func (m *scriptedModel) Name() string { return m.name }

func (m *scriptedModel) GenerateContent(_ context.Context, req *model.LLMRequest, _ bool) iter.Seq2[*model.LLMResponse, error] {
	return func(yield func(*model.LLMResponse, error) bool) {
		var last *genai.Content
		if n := len(req.Contents); n > 0 {
			last = req.Contents[n-1]
		}
		content, err := m.reply(last)
		if err != nil {
			yield(nil, err)
			return
		}
		yield(&model.LLMResponse{
			Content: content,
			UsageMetadata: &genai.GenerateContentResponseUsageMetadata{
				PromptTokenCount:     10,
				CandidatesTokenCount: 5,
				TotalTokenCount:      15,
			},
			FinishReason: genai.FinishReasonStop,
		}, nil)
	}
}

func modelText(text string) *genai.Content {
	return genai.NewContentFromText(text, genai.RoleModel)
}

// lastText is the text of the last content's first text part.
func lastText(c *genai.Content) string {
	if c == nil {
		return ""
	}
	for _, p := range c.Parts {
		if p.Text != "" {
			return p.Text
		}
	}
	return ""
}

func hasFunctionResponse(c *genai.Content) bool {
	if c == nil {
		return false
	}
	for _, p := range c.Parts {
		if p.FunctionResponse != nil {
			return true
		}
	}
	return false
}

// assistantReply is the script of the assistant agent's model.
func assistantReply(last *genai.Content) (*genai.Content, error) {
	if hasFunctionResponse(last) {
		return modelText("Saved your note."), nil
	}
	text := lastText(last)
	switch {
	case text == "hello":
		return modelText("Hello! How can I help?"), nil
	case text == "fail":
		return nil, errors.New("scripted model failure")
	case strings.HasPrefix(text, "save: "):
		return &genai.Content{Role: genai.RoleModel, Parts: []*genai.Part{{
			FunctionCall: &genai.FunctionCall{
				Name: "save_note",
				Args: map[string]any{"text": strings.TrimPrefix(text, "save: ")},
			},
		}}}, nil
	default:
		return modelText("I can only say hello."), nil
	}
}

// ---------------------------------------------------------------------------
// Sample agents.

type saveNoteArgs struct {
	Text string `json:"text"`
}

type saveNoteResult struct {
	Status  string `json:"status"`
	Version int64  `json:"version"`
}

func saveNote(ctx agent.Context, args saveNoteArgs) (saveNoteResult, error) {
	resp, err := ctx.Artifacts().Save(ctx, "note.txt", genai.NewPartFromText(args.Text))
	if err != nil {
		return saveNoteResult{}, err
	}
	return saveNoteResult{Status: "saved", Version: resp.Version}, nil
}

func newAgents() (agent.Loader, error) {
	saveNoteTool, err := functiontool.New(functiontool.Config{
		Name:        "save_note",
		Description: "Saves a note as the artifact note.txt.",
	}, saveNote)
	if err != nil {
		return nil, err
	}
	assistant, err := llmagent.New(llmagent.Config{
		Name:        "assistant",
		Description: "Answers greetings and saves notes.",
		Model:       &scriptedModel{name: "scripted-assistant", reply: assistantReply},
		Instruction: "Answer briefly. Use save_note to save notes.",
		Tools:       []tool.Tool{saveNoteTool},
	})
	if err != nil {
		return nil, err
	}
	drafter, err := llmagent.New(llmagent.Config{
		Name:        "drafter",
		Description: "Writes a draft.",
		Model: &scriptedModel{name: "scripted-drafter", reply: func(*genai.Content) (*genai.Content, error) {
			return modelText("Draft ready."), nil
		}},
		Instruction: "Write a draft.",
	})
	if err != nil {
		return nil, err
	}
	reviewer, err := llmagent.New(llmagent.Config{
		Name:        "reviewer",
		Description: "Reviews the draft.",
		Model: &scriptedModel{name: "scripted-reviewer", reply: func(*genai.Content) (*genai.Content, error) {
			return modelText("Review done."), nil
		}},
		Instruction: "Review the draft.",
	})
	if err != nil {
		return nil, err
	}
	pipeline, err := sequentialagent.New(sequentialagent.Config{AgentConfig: agent.Config{
		Name:        "pipeline",
		Description: "Drafts, then reviews.",
		SubAgents:   []agent.Agent{drafter, reviewer},
	}})
	if err != nil {
		return nil, err
	}
	return agent.NewMultiLoader(assistant, pipeline)
}

// ---------------------------------------------------------------------------
// Servers.

func baseConfig(loader agent.Loader) adkrest.ServerConfig {
	return adkrest.ServerConfig{
		SessionService:  session.InMemoryService(),
		MemoryService:   memory.InMemoryService(),
		ArtifactService: artifact.InMemoryService(),
		AgentLoader:     loader,
		BindHost:        "127.0.0.1",
		AllowedOrigins:  []string{"http://localhost:4200"},
	}
}

func customAuth(r *http.Request) (*authn.Caller, error) {
	switch v := r.Header.Get("X-Test-User"); v {
	case "forbid":
		return nil, authn.ErrForbidden
	case "boom":
		return nil, errors.New("boom")
	case "":
		return nil, nil
	default:
		return &authn.Caller{UserID: v}, nil
	}
}

func newServer(name string, loader agent.Loader) (*adkrest.Server, error) {
	cfg := baseConfig(loader)
	switch name {
	case "main":
		cfg.DebugAPIConfig = adkrest.DebugAPIConfig{IncludeDebugAPI: true}
		cfg.MaxPayloadSize = mainMaxPayload
	case "strict":
		cfg.Authenticator = authn.NewHeader("X-Test-User")
		cfg.Authorizer = authz.NewStrict()
	case "custom":
		cfg.Authenticator = authn.NewCustom(customAuth)
	}
	return adkrest.NewServer(cfg)
}

// ---------------------------------------------------------------------------
// Fixtures.

type wireRequest struct {
	Method  string      `json:"method"`
	Target  string      `json:"target"`
	Headers [][2]string `json:"headers"`
	Body    string      `json:"body"`
}

type wireResponse struct {
	Status  int         `json:"status"`
	Headers [][2]string `json:"headers"`
	Body    string      `json:"body"`
}

type fixture struct {
	Name     string       `json:"name"`
	Config   string       `json:"config"`
	Request  wireRequest  `json:"request"`
	Response wireResponse `json:"response"`
	Compare  string       `json:"compare"`
}

// step is one request of the script. target, when set, computes the target
// from earlier responses; after sees the response body.
type step struct {
	name    string
	method  string
	path    string
	target  func() string
	headers [][2]string
	body    string
	compare string
	after   func(body []byte)
}

var excludedHeaders = map[string]bool{"Date": true, "Content-Length": true, "Transfer-Encoding": true}

// wireTap records every byte the client reads from the server, so response
// headers come from the wire: net/http's client deletes "Connection: close"
// from the headers it returns.
type wireTap struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

type tappedConn struct {
	net.Conn
	tap *wireTap
}

func (c tappedConn) Read(b []byte) (int, error) {
	n, err := c.Conn.Read(b)
	c.tap.mu.Lock()
	c.tap.buf.Write(b[:n])
	c.tap.mu.Unlock()
	return n, err
}

func (t *wireTap) dial(ctx context.Context, network, addr string) (net.Conn, error) {
	conn, err := (&net.Dialer{}).DialContext(ctx, network, addr)
	if err != nil {
		return nil, err
	}
	return tappedConn{Conn: conn, tap: t}, nil
}

func (t *wireTap) reset() {
	t.mu.Lock()
	t.buf.Reset()
	t.mu.Unlock()
}

// headers parses the header block of the one response read since reset.
func (t *wireTap) headers() [][2]string {
	t.mu.Lock()
	raw := t.buf.String()
	t.mu.Unlock()
	head, _, ok := strings.Cut(raw, "\r\n\r\n")
	if !ok {
		log.Fatalf("no header block in %q", raw)
	}
	lines := strings.Split(head, "\r\n")[1:] // drop the status line
	out := [][2]string{}
	for _, line := range lines {
		name, value, ok := strings.Cut(line, ":")
		if !ok {
			log.Fatalf("malformed header line %q", line)
		}
		name = http.CanonicalHeaderKey(name)
		if excludedHeaders[name] {
			continue
		}
		out = append(out, [2]string{name, strings.TrimSpace(value)})
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i][0] != out[j][0] {
			return out[i][0] < out[j][0]
		}
		return out[i][1] < out[j][1]
	})
	return out
}

func send(client *http.Client, tap *wireTap, base string, s step) (wireRequest, wireResponse, []byte) {
	target := s.path
	if s.target != nil {
		target = s.target()
	}
	headers := s.headers
	if headers == nil {
		headers = [][2]string{}
	}
	var body io.Reader = http.NoBody
	if s.body != "" {
		body = strings.NewReader(s.body)
	}
	req, err := http.NewRequest(s.method, base+target, body)
	check(err)
	for _, h := range headers {
		if h[0] == "Host" {
			req.Host = h[1]
			continue
		}
		req.Header.Add(h[0], h[1])
	}
	tap.reset()
	resp, err := client.Do(req)
	check(err)
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	check(err)
	sort.Slice(headers, func(i, j int) bool { return headers[i][0] < headers[j][0] })
	return wireRequest{Method: s.method, Target: target, Headers: headers, Body: s.body},
		wireResponse{Status: resp.StatusCode, Headers: tap.headers(), Body: string(data)},
		data
}

func write(f fixture) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	check(enc.Encode(f))
	check(os.WriteFile(filepath.Join(outDir, f.Config+"_"+f.Name+".json"), buf.Bytes(), 0o644))
}

func run(config string, loader agent.Loader, steps []step) int {
	srv, err := newServer(config, loader)
	check(err)
	if config == "main" {
		otel.SetTracerProvider(sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(srv.SpanProcessor())))
		global.SetLoggerProvider(sdklog.NewLoggerProvider(sdklog.WithProcessor(srv.LogProcessor())))
	}
	ts := httptest.NewServer(withPlatform(&fixedPlatform{}, srv))
	defer ts.Close()
	tap := &wireTap{}
	client := &http.Client{
		Transport:     &http.Transport{DialContext: tap.dial, DisableCompression: true},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	for i, s := range steps {
		req, resp, data := send(client, tap, ts.URL, s)
		compare := s.compare
		if compare == "" {
			compare = "exact"
		}
		write(fixture{
			Name:     fmt.Sprintf("%02d_%s", i+1, s.name),
			Config:   config,
			Request:  req,
			Response: resp,
			Compare:  compare,
		})
		if s.after != nil {
			s.after(data)
		}
	}
	return len(steps)
}

// ---------------------------------------------------------------------------
// The request script.

const (
	jsonCT = "application/json"
	s1     = "/apps/assistant/users/alice/sessions/s1"
)

func ct() [][2]string { return [][2]string{{"Content-Type", jsonCT}} }

func runBody(app, session, text string, streaming bool) string {
	streamingField := ""
	if streaming {
		streamingField = `,"streaming":true`
	}
	return fmt.Sprintf(`{"appName":%q,"userId":"alice","sessionId":%q,"newMessage":{"role":"user","parts":[{"text":%q}]}%s}`,
		app, session, text, streamingField)
}

// eventWith returns the id of the first event in a /run response whose
// content has a part with the given key.
func eventWith(body []byte, partKey string) string {
	var events []struct {
		ID      string `json:"id"`
		Content *struct {
			Parts []map[string]any `json:"parts"`
		} `json:"content"`
	}
	check(json.Unmarshal(body, &events))
	for _, e := range events {
		if e.Content == nil {
			continue
		}
		for _, p := range e.Content.Parts {
			if _, ok := p[partKey]; ok {
				return e.ID
			}
		}
	}
	log.Fatalf("no event with a %s part in %s", partKey, body)
	return ""
}

func mainSteps() []step {
	var callEventID, responseEventID string
	oversized := fmt.Sprintf(`{"state":{"padding":%q}}`, strings.Repeat("a", 2*mainMaxPayload))
	return []step{
		{name: "health_get", method: "GET", path: "/health"},
		{name: "health_head", method: "HEAD", path: "/health"},
		{name: "version", method: "GET", path: "/version", compare: "json_key_set"},
		{name: "list_apps", method: "GET", path: "/list-apps", compare: "json_sorted_array"},
		{name: "session_create_with_id", method: "POST", path: s1, headers: ct(), body: `{"state":{"color":"blue"}}`},
		{name: "session_create_without_id", method: "POST", path: "/apps/assistant/users/alice/sessions"},
		{name: "session_get", method: "GET", path: s1},
		{name: "session_list", method: "GET", path: "/apps/assistant/users/alice/sessions"},
		{name: "session_patch", method: "PATCH", path: s1, headers: ct(), body: `{"stateDelta":{"mood":"happy"}}`},
		{name: "session_patch_scoped", method: "PATCH", path: s1, headers: ct(), body: `{"stateDelta":{"app:theme":"dark","user:lang":"en"}}`},
		{name: "session_get_missing", method: "GET", path: "/apps/assistant/users/alice/sessions/missing"},
		{name: "run_hello", method: "POST", path: "/run", headers: ct(), body: runBody("assistant", "s1", "hello", false)},
		{name: "run_save_note", method: "POST", path: "/run", headers: ct(), body: runBody("assistant", "s1", "save: buy milk", false),
			after: func(b []byte) {
				callEventID = eventWith(b, "functionCall")
				responseEventID = eventWith(b, "functionResponse")
			}},
		{name: "run_save_note_again", method: "POST", path: "/run", headers: ct(), body: runBody("assistant", "s1", "save: buy eggs", false)},
		{name: "run_error", method: "POST", path: "/run", headers: ct(), body: runBody("assistant", "s1", "fail", false)},
		{name: "run_unknown_session", method: "POST", path: "/run", headers: ct(), body: runBody("assistant", "nope", "hello", false)},
		{name: "run_unknown_field", method: "POST", path: "/run", headers: ct(), body: `{"appName":"assistant","bogus":1}`},
		{name: "run_sse_hello", method: "POST", path: "/run_sse", headers: ct(), body: runBody("assistant", "s1", "hello", true), compare: "sse"},
		{name: "run_sse_error", method: "POST", path: "/run_sse", headers: ct(), body: runBody("assistant", "s1", "fail", false), compare: "sse"},
		{name: "run_sse_unknown_session", method: "POST", path: "/run_sse", headers: ct(), body: runBody("assistant", "nope", "hello", false)},
		{name: "run_live_missing_params", method: "GET", path: "/run_live"},
		{name: "pipeline_session_create", method: "POST", path: "/apps/pipeline/users/alice/sessions/p1"},
		{name: "run_pipeline", method: "POST", path: "/run", headers: ct(), body: runBody("pipeline", "p1", "go", false)},
		{name: "artifacts_list", method: "GET", path: s1 + "/artifacts"},
		{name: "artifact_get", method: "GET", path: s1 + "/artifacts/note.txt"},
		{name: "artifact_get_query_version", method: "GET", path: s1 + "/artifacts/note.txt?version=1"},
		{name: "artifact_get_version", method: "GET", path: s1 + "/artifacts/note.txt/versions/1"},
		{name: "artifact_get_version_missing", method: "GET", path: s1 + "/artifacts/note.txt/versions/7"},
		{name: "artifact_get_version_invalid", method: "GET", path: s1 + "/artifacts/note.txt/versions/x"},
		{name: "artifact_delete", method: "DELETE", path: s1 + "/artifacts/note.txt"},
		{name: "artifacts_list_after_delete", method: "GET", path: s1 + "/artifacts"},
		{name: "debug_trace_event", method: "GET", compare: "debug_trace",
			target: func() string { return "/debug/trace/" + callEventID }},
		{name: "debug_trace_event_dev", method: "GET", compare: "debug_trace",
			target: func() string { return "/dev/apps/assistant/debug/trace/" + responseEventID }},
		{name: "debug_trace_event_missing", method: "GET", path: "/debug/trace/nope"},
		{name: "debug_trace_session", method: "GET", path: "/debug/trace/session/p1", compare: "debug_trace"},
		{name: "debug_trace_session_dev", method: "GET", path: "/dev/apps/assistant/debug/trace/session/s1", compare: "debug_trace"},
		{name: "event_graph", method: "GET",
			target: func() string { return s1 + "/events/" + callEventID + "/graph" }},
		{name: "event_graph_dev", method: "GET",
			target: func() string {
				return "/dev/apps/assistant/users/alice/sessions/s1/events/" + responseEventID + "/graph"
			}},
		{name: "event_graph_missing_event", method: "GET", path: s1 + "/events/nope/graph"},
		{name: "build_graph", method: "GET", path: "/dev/apps/pipeline/build_graph"},
		{name: "build_graph_image", method: "GET", path: "/dev/apps/assistant/build_graph_image"},
		{name: "build_graph_image_pipeline_dark", method: "GET", path: "/dev/apps/pipeline/build_graph_image?dark_mode=true"},
		{name: "build_graph_image_node", method: "GET", path: "/dev/apps/pipeline/build_graph_image?node=pipeline/reviewer"},
		{name: "build_graph_image_node_missing", method: "GET", path: "/dev/apps/pipeline/build_graph_image?node=pipeline/nobody"},
		{name: "build_graph_unknown_app", method: "GET", path: "/dev/apps/nobody/build_graph"},
		{name: "builder_get", method: "GET", path: "/dev/apps/assistant/builder"},
		{name: "builder_save", method: "POST", path: "/dev/apps/assistant/builder/save"},
		{name: "tests_list", method: "GET", path: "/dev/apps/assistant/tests"},
		{name: "eval_sets_list", method: "GET", path: "/dev/apps/assistant/eval_sets"},
		{name: "eval_sets_create_dash", method: "POST", path: "/dev/apps/assistant/eval-sets"},
		{name: "eval_legacy_results", method: "GET", path: "/apps/assistant/eval_results"},
		{name: "metrics_info", method: "GET", path: "/dev/apps/assistant/metrics-info"},
		{name: "unknown_path", method: "GET", path: "/no/such/path"},
		{name: "wrong_method", method: "PUT", path: "/list-apps"},
		{name: "wrong_method_run", method: "GET", path: "/run"},
		{name: "options_session", method: "OPTIONS", path: s1},
		{name: "options_unknown_path", method: "OPTIONS", path: "/no/such/path"},
		{name: "head_list_apps", method: "HEAD", path: "/list-apps"},
		{name: "trailing_slash_post", method: "POST", path: "/apps/assistant/users/alice/sessions/s2/", headers: ct(), body: `{"state":{"via":"slash"}}`},
		{name: "trailing_slash_get", method: "GET", path: "/apps/assistant/users/alice/sessions/s2/"},
		{name: "trailing_slash_root", method: "GET", path: "/"},
		{name: "oversized_body", method: "POST", path: "/apps/assistant/users/alice/sessions", headers: ct(), body: oversized},
		{name: "origin_forbidden", method: "GET", path: "/health", headers: [][2]string{{"Origin", "http://evil.example"}}},
		{name: "origin_allowed", method: "GET", path: "/health", headers: [][2]string{{"Origin", "http://localhost:4200"}}},
		{name: "host_forbidden", method: "GET", path: "/health", headers: [][2]string{{"Host", "evil.example"}}},
		{name: "session_delete", method: "DELETE", path: "/apps/assistant/users/alice/sessions/s2"},
		{name: "session_list_after_delete", method: "GET", path: "/apps/assistant/users/alice/sessions"},
	}
}

func strictSteps() []step {
	alice := [][2]string{{"X-Test-User", "alice"}}
	bob := [][2]string{{"X-Test-User", "bob"}}
	aliceJSON := [][2]string{{"Content-Type", jsonCT}, {"X-Test-User", "alice"}}
	bobJSON := [][2]string{{"Content-Type", jsonCT}, {"X-Test-User", "bob"}}
	return []step{
		{name: "health_no_credentials", method: "GET", path: "/health"},
		{name: "version_no_credentials", method: "GET", path: "/version", compare: "json_key_set"},
		{name: "list_apps_no_credentials", method: "GET", path: "/list-apps"},
		{name: "unknown_path_no_credentials", method: "GET", path: "/no/such/path"},
		{name: "wrong_method_no_credentials", method: "PUT", path: "/list-apps"},
		{name: "session_create_authz_forbidden", method: "POST", path: s1, headers: bob},
		{name: "session_create", method: "POST", path: s1, headers: alice},
		{name: "session_get_authz_forbidden", method: "GET", path: s1, headers: bob},
		{name: "run_authz_forbidden", method: "POST", path: "/run", headers: bobJSON, body: runBody("assistant", "s1", "hello", false)},
		{name: "run_sse_authz_forbidden", method: "POST", path: "/run_sse", headers: bobJSON, body: runBody("assistant", "s1", "hello", false)},
		{name: "run", method: "POST", path: "/run", headers: aliceJSON, body: runBody("assistant", "s1", "hello", false)},
	}
}

func customSteps() []step {
	return []step{
		{name: "authn_forbidden", method: "GET", path: "/list-apps", headers: [][2]string{{"X-Test-User", "forbid"}}},
		{name: "authn_error", method: "GET", path: "/list-apps", headers: [][2]string{{"X-Test-User", "boom"}}},
		{name: "authn_no_caller", method: "GET", path: "/list-apps"},
		{name: "authn_ok", method: "GET", path: "/apps/assistant/users/alice/sessions", headers: [][2]string{{"X-Test-User", "alice"}}},
	}
}

func main() {
	// R25: both telemetry environment variables unset.
	check(os.Unsetenv("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"))
	check(os.Unsetenv("ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN"))

	check(os.MkdirAll(outDir, 0o755))
	old, err := filepath.Glob(filepath.Join(outDir, "*.json"))
	check(err)
	for _, f := range old {
		check(os.Remove(f))
	}

	counts := map[string]int{}
	for _, c := range []struct {
		name  string
		steps []step
	}{{"main", mainSteps()}, {"strict", strictSteps()}, {"custom", customSteps()}} {
		loader, err := newAgents()
		check(err)
		counts[c.name] = run(c.name, loader, c.steps)
	}
	fmt.Printf("wrote main=%d strict=%d custom=%d fixtures to %s\n",
		counts["main"], counts["strict"], counts["custom"], outDir)
}

func check(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
