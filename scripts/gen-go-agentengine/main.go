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

// Command gen-go-agentengine records ADK-Go's Agent Engine server
// (server/agentengine.NewHandler) and its launcher
// (cmd/launcher/agentengine.NewLauncher) as gadk fixtures (SCENARIO.md):
//
//   - crates/gadk-server/testdata/agentengine/: the handler on a real loopback
//     listener in two configurations (main, small), one file per exchange,
//     plus its startup log and ListClassMethods();
//   - crates/gadk-launcher/testdata/agentengine/: the launcher as a subprocess,
//     command lines (cli_*) and web servers (wire_*).
//
// With --shapes [--out PATH] it instead writes gadk-server's encode_shapes.rs
// (default ../../crates/gadk-server/src/agentengine/encode_shapes.rs) by
// running shapes/main.go.in inside ADK-Go through `go run -overlay`.
//
// The same binary is the probe and the launched program: with
// GEN_GO_AGENTENGINE_CHILD set it is the child (childMain).
//
// Run from this directory: go run . (or go run . --shapes)
package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
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
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode/utf8"

	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/artifact"
	"google.golang.org/adk/v2/cmd/launcher"
	aelauncher "google.golang.org/adk/v2/cmd/launcher/agentengine"
	"google.golang.org/adk/v2/memory"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/server/agentengine"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/session/compaction"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
)

const (
	serverDir   = "../../crates/gadk-server/testdata/agentengine"
	launcherDir = "../../crates/gadk-launcher/testdata/agentengine"
	shapesOut   = "../../crates/gadk-server/src/agentengine/encode_shapes.rs"
	shapesSrc   = "shapes/main.go.in"
	commitFile  = "../../docs/parity/ADK_GO_COMMIT"

	childEnv   = "GEN_GO_AGENTENGINE_CHILD"
	errFileEnv = "GEN_GO_AGENTENGINE_ERROR_FILE"

	// engineID is the agent engine ID (the runner's app name).
	engineID = "test-engine"

	portPlaceholder = "<port>"
	inlineBodyLimit = 8192
	quietPeriod     = 150 * time.Millisecond

	// processingEvent starts the per-event %+v dump line of the stream
	// methods; such lines are removed from every recorded log.
	processingEvent = "Processing event: "
)

// t0 is the first value the fixed clock returns; every read advances it 1 s.
var t0 = time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)

// ---------------------------------------------------------------------------
// Fixed providers (scripts/gen-go-adkrest).

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
// Scripted model and the assistant agent (scripts/gen-go-adkrest, unchanged).

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

func newAssistant() (agent.Agent, error) {
	saveNoteTool, err := functiontool.New(functiontool.Config{
		Name:        "save_note",
		Description: "Saves a note as the artifact note.txt.",
	}, saveNote)
	if err != nil {
		return nil, err
	}
	return llmagent.New(llmagent.Config{
		Name:        "assistant",
		Description: "Answers greetings and saves notes.",
		Model:       &scriptedModel{name: "scripted-assistant", reply: assistantReply},
		Instruction: "Answer briefly. Use save_note to save notes.",
		Tools:       []tool.Tool{saveNoteTool},
	})
}

// ---------------------------------------------------------------------------
// Shared helpers.

type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func (b *syncBuffer) Len() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Len()
}

// take returns the bytes written so far and empties the buffer.
func (b *syncBuffer) take() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	s := b.buf.String()
	b.buf.Reset()
	return s
}

func encode(v any) []byte {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	check(enc.Encode(v))
	return buf.Bytes()
}

func sortHeaders(headers [][2]string) {
	sort.SliceStable(headers, func(i, j int) bool {
		if headers[i][0] != headers[j][0] {
			return headers[i][0] < headers[j][0]
		}
		return headers[i][1] < headers[j][1]
	})
}

// fileSet maps a path relative to the repository's crates/ to fixture bytes.
type fileSet map[string][]byte

// ---------------------------------------------------------------------------
// Server fixtures.

type wireRequest struct {
	Method  string      `json:"method"`
	Target  string      `json:"target"`
	Headers [][2]string `json:"headers"`
	Body    string      `json:"body"`
}

type serverResponse struct {
	Status  int         `json:"status"`
	Headers [][2]string `json:"headers"`
	Body    string      `json:"body"`
}

type serverFixture struct {
	Name     string           `json:"name"`
	Config   string           `json:"config"`
	Kind     string           `json:"kind"`
	Request  *wireRequest     `json:"request"`
	Response *serverResponse  `json:"response"`
	Log      string           `json:"log"`
	Methods  []map[string]any `json:"methods"`
	Compare  string           `json:"compare"`
}

var excludedHeaders = map[string]bool{"Date": true, "Content-Length": true, "Transfer-Encoding": true}

// wireTap records every byte the client reads, so response headers come from
// the wire (scripts/gen-go-adkrest).
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

func (t *wireTap) headers() [][2]string {
	t.mu.Lock()
	raw := t.buf.String()
	t.mu.Unlock()
	head, _, ok := strings.Cut(raw, "\r\n\r\n")
	if !ok {
		fatalf("no header block in %q", raw)
	}
	out := [][2]string{}
	for _, line := range strings.Split(head, "\r\n")[1:] {
		name, value, ok := strings.Cut(line, ":")
		if !ok {
			fatalf("malformed header line %q", line)
		}
		name = http.CanonicalHeaderKey(name)
		if excludedHeaders[name] {
			continue
		}
		out = append(out, [2]string{name, strings.TrimSpace(value)})
	}
	sortHeaders(out)
	return out
}

// dropProcessingEvents removes every line starting with processingEvent.
func dropProcessingEvents(s string) string {
	if s == "" {
		return s
	}
	lines := strings.SplitAfter(s, "\n")
	var b strings.Builder
	for _, l := range lines {
		if !strings.HasPrefix(l, processingEvent) {
			b.WriteString(l)
		}
	}
	return b.String()
}

// serverStep is one request of a configuration's script; body, when set,
// computes the request body from earlier responses; after sees the response
// body.
type serverStep struct {
	name    string
	kind    string // "" = exchange
	method  string
	target  string
	headers [][2]string
	body    func() string
	after   func(body []byte)
}

type serverConfig struct {
	name           string
	maxPayloadSize int64
	steps          func(svc session.Service) []serverStep
}

const (
	jsonCT = "application/json"
	re     = "/reasoning_engine"
	sre    = "/stream_reasoning_engine"
)

func ct() [][2]string { return [][2]string{{"Content-Type", jsonCT}} }

func lit(s string) func() string { return func() string { return s } }

func mustJSON(v any) string {
	data, err := json.Marshal(v)
	check(err)
	return string(data)
}

func call(method string, input any) string {
	return mustJSON(map[string]any{"class_method": method, "input": input})
}

func listU1() string {
	return `{"class_method":"async_list_sessions","input":{"user_id":"u1"}}`
}

// outputID is output.id of a session method's JSON response.
func outputID(body []byte) string {
	var v struct {
		Output struct {
			ID string `json:"id"`
		} `json:"output"`
	}
	check(json.Unmarshal(body, &v))
	if v.Output.ID == "" {
		fatalf("no output.id in %s", body)
	}
	return v.Output.ID
}

// firstSessionID is session_id of the first NDJSON line.
func firstSessionID(body []byte) string {
	line, _, _ := bytes.Cut(body, []byte("\n"))
	var v struct {
		SessionID string `json:"session_id"`
	}
	check(json.Unmarshal(line, &v))
	if v.SessionID == "" {
		fatalf("no session_id in %s", body)
	}
	return v.SessionID
}

func runRequestJSON(user, sessionID, text string) string {
	return fmt.Sprintf(`{"user_id":%q,"session_id":%q,"message":{"role":"user","parts":[{"text":%q}]}}`, user, sessionID, text)
}

func mainSteps(svc session.Service) []serverStep {
	var s02, s03, s09, s14 string
	return []serverStep{
		{name: "startup", kind: "startup"},
		{name: "class_methods", kind: "class_methods"},
		{name: "create_session", method: "POST", target: re, headers: ct(),
			body:  lit(`{"class_method":"async_create_session","input":{"user_id":"u1"}}`),
			after: func(b []byte) { s02 = outputID(b) }},
		{name: "create_session_with_state", method: "POST", target: re, headers: ct(),
			body:  lit(`{"class_method":"async_create_session","input":{"user_id":"u1","state":{"k":"v","n":1}}}`),
			after: func(b []byte) { s03 = outputID(b) }},
		{name: "get_session", method: "POST", target: re, headers: ct(),
			body: func() string {
				return call("async_get_session", map[string]string{"user_id": "u1", "session_id": s02})
			}},
		{name: "get_session_missing", method: "POST", target: re, headers: ct(),
			body: lit(`{"class_method":"async_get_session","input":{"user_id":"u1","session_id":"nope"}}`)},
		{name: "list_sessions", method: "POST", target: re, headers: ct(), body: lit(listU1())},
		{name: "delete_session", method: "POST", target: re, headers: ct(),
			body: func() string {
				return call("async_delete_session", map[string]string{"user_id": "u1", "session_id": s03})
			}},
		{name: "delete_session_missing", method: "POST", target: re, headers: ct(),
			body: lit(`{"class_method":"async_delete_session","input":{"user_id":"u1","session_id":"nope"}}`)},
		{name: "stream_query_text", method: "POST", target: sre, headers: ct(),
			body: lit(`{"class_method":"async_stream_query","input":{"user_id":"u2","message":"hello"}}`),
			after: func([]byte) {
				// The stream does not name the session it created: read it
				// from the session service (List reads no clock or UUID).
				resp, err := svc.List(context.Background(), &session.ListRequest{AppName: engineID, UserID: "u2"})
				check(err)
				if len(resp.Sessions) != 1 {
					fatalf("want one session of u2, got %d", len(resp.Sessions))
				}
				s09 = resp.Sessions[0].ID()
			}},
		{name: "stream_query_content", method: "POST", target: sre, headers: ct(),
			body: func() string {
				return fmt.Sprintf(`{"class_method":"async_stream_query","input":{"user_id":"u2","session_id":%q,"message":{"role":"user","parts":[{"text":"hello"}]}}}`, s09)
			}},
		{name: "stream_query_tool_call", method: "POST", target: sre, headers: ct(),
			body: func() string {
				return fmt.Sprintf(`{"class_method":"async_stream_query","input":{"user_id":"u2","session_id":%q,"message":"save: milk"}}`, s09)
			}},
		{name: "stream_query_model_error", method: "POST", target: sre, headers: ct(),
			body: func() string {
				return fmt.Sprintf(`{"class_method":"async_stream_query","input":{"user_id":"u2","session_id":%q,"message":"fail"}}`, s09)
			}},
		{name: "stream_query_bad_input", method: "POST", target: sre, headers: ct(),
			body: lit(`{"class_method":"async_stream_query","input":{"user_id":"u2","message":5}}`)},
		{name: "run_with_events_new_session", method: "POST", target: sre, headers: ct(),
			body: lit(call("streaming_agent_run_with_events", map[string]string{
				"request_json": runRequestJSON("u3", "projects/p/locations/global/collections/default_collection/engines/e/sessions/s", "hello"),
			})),
			after: func(b []byte) { s14 = firstSessionID(b) }},
		{name: "run_with_events_reuse_session", method: "POST", target: sre, headers: ct(),
			body: func() string {
				return call("streaming_agent_run_with_events", map[string]string{"request_json": runRequestJSON("u3", s14, "hello")})
			}},
		{name: "run_with_events_bad_request_json", method: "POST", target: sre, headers: ct(),
			body: lit(`{"class_method":"streaming_agent_run_with_events","input":{"request_json":"{"}}`)},
		{name: "session_method_on_stream_route", method: "POST", target: sre, headers: ct(), body: lit(listU1())},
		{name: "stream_method_on_session_route", method: "POST", target: re, headers: ct(),
			body: lit(`{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hello"}}`)},
		{name: "unknown_method", method: "POST", target: re, headers: ct(), body: lit(`{"class_method":"nope","input":{}}`)},
		{name: "bad_body", method: "POST", target: re, headers: ct(), body: lit(`{"class_method":5}`)},
		{name: "empty_body", method: "POST", target: re},
		{name: "uppercase_keys", method: "POST", target: re, headers: ct(),
			body: lit(`{"CLASS_METHOD":"async_list_sessions","Input":{"USER_ID":"u1"}}`)},
		{name: "unknown_path", method: "POST", target: "/nope", headers: ct(), body: lit(listU1())},
		{name: "wrong_http_method", method: "GET", target: re},
		{name: "trailing_slash", method: "POST", target: re + "/", headers: ct(), body: lit(listU1())},
		{name: "unclean_path", method: "POST", target: "/x/.." + re, headers: ct(), body: lit(listU1())},
		{name: "reasoning_engine_query", method: "POST", target: re + "?x=1", headers: ct(), body: lit(listU1())},
	}
}

func smallSteps(session.Service) []serverStep {
	return []serverStep{
		{name: "over_limit_payload", method: "POST", target: re, headers: ct(), body: lit(listU1())},
	}
}

func serverConfigs() []serverConfig {
	return []serverConfig{
		{name: "main", maxPayloadSize: 4096, steps: mainSteps},
		{name: "small", maxPayloadSize: 32, steps: smallSteps},
	}
}

func sendServer(client *http.Client, tap *wireTap, base string, s serverStep) (*wireRequest, *serverResponse, []byte) {
	body := ""
	if s.body != nil {
		body = s.body()
	}
	headers := append([][2]string{}, s.headers...)
	var reader io.Reader = http.NoBody
	if body != "" {
		reader = strings.NewReader(body)
	}
	req, err := http.NewRequest(s.method, base+s.target, reader)
	check(err)
	for _, h := range headers {
		req.Header.Add(h[0], h[1])
	}
	tap.reset()
	resp, err := client.Do(req)
	check(err)
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	check(err)
	sort.SliceStable(headers, func(i, j int) bool { return headers[i][0] < headers[j][0] })
	return &wireRequest{Method: s.method, Target: s.target, Headers: headers, Body: body},
		&serverResponse{Status: resp.StatusCode, Headers: tap.headers(), Body: string(data)},
		data
}

// runServerConfig runs one configuration: a fresh handler over fresh
// in-memory services, served by httptest.NewServer behind withPlatform.
func runServerConfig(c serverConfig, files fileSet, logBuf *syncBuffer) {
	assistant, err := newAssistant()
	check(err)
	svc := session.InMemoryService()
	cfg := &launcher.Config{
		SessionService:  svc,
		MemoryService:   memory.InMemoryService(),
		ArtifactService: artifact.InMemoryService(),
		AgentLoader:     agent.NewSingleLoader(assistant),
	}
	logBuf.take()
	handler, err := agentengine.NewHandler(cfg, 0, c.maxPayloadSize, engineID)
	check(err)
	startupLog := dropProcessingEvents(logBuf.take())

	ts := httptest.NewServer(withPlatform(&fixedPlatform{}, handler))
	defer ts.Close()
	tap := &wireTap{}
	client := &http.Client{
		Transport:     &http.Transport{DialContext: tap.dial, DisableCompression: true},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	steps := c.steps(svc)
	first := 1
	if c.name == "main" {
		first = 0
	}
	for i, s := range steps {
		name := fmt.Sprintf("%s_%02d_%s", c.name, first+i, s.name)
		f := serverFixture{Name: name, Config: c.name, Compare: "exact"}
		switch s.kind {
		case "startup":
			f.Kind = "startup"
			f.Log = startupLog
		case "class_methods":
			f.Kind = "class_methods"
			methods, err := agentengine.ListClassMethods()
			check(err)
			f.Methods = []map[string]any{}
			for _, m := range methods {
				f.Methods = append(f.Methods, m.AsMap())
			}
		default:
			f.Kind = "exchange"
			logBuf.take()
			req, resp, data := sendServer(client, tap, ts.URL, s)
			f.Request, f.Response = req, resp
			f.Log = dropProcessingEvents(logBuf.take())
			if s.after != nil {
				s.after(data)
			}
		}
		files[filepath.Join(serverDir, name+".json")] = encode(f)
	}
}

func generateServer() fileSet {
	files := fileSet{}
	logBuf := &syncBuffer{}
	log.SetFlags(0)
	log.SetOutput(logBuf)
	defer func() {
		log.SetFlags(log.LstdFlags)
		log.SetOutput(os.Stderr)
	}()
	for _, c := range serverConfigs() {
		runServerConfig(c, files, logBuf)
	}
	return files
}

// ---------------------------------------------------------------------------
// Launcher child: the launched program.

// childMain is ADK-Go's example main with the assistant agent and
// agentengine.NewLauncher(engineID) (scripts/gen-go-launcher's child).
func childMain() {
	args := os.Args[1:]
	if len(args) == 0 {
		log.Fatal("child: missing launcher")
	}
	name, args := args[0], args[1:]

	fp := &fixedPlatform{}
	ctx := platform.WithTimeProvider(context.Background(), fp.now)
	ctx = platform.WithUUIDProvider(ctx, fp.uuid)
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGTERM)
	defer stop()

	a, err := newAssistant()
	if err != nil {
		log.Fatalf("Failed to create agent: %v", err)
	}
	config := &launcher.Config{AgentLoader: agent.NewSingleLoader(a)}
	switch name {
	case "agentengine":
	case "agentengine_bad_compaction":
		config.Compaction = &compaction.Config{}
	default:
		log.Fatalf("child: unknown launcher %q", name)
	}
	l := aelauncher.NewLauncher(engineID)
	if err = l.Execute(ctx, config, args); err != nil {
		if f := os.Getenv(errFileEnv); f != "" {
			if werr := os.WriteFile(f, []byte(err.Error()), 0o644); werr != nil {
				log.Printf("child: cannot write the error file: %v", werr)
			}
		}
		log.Fatalf("Run failed: %v\n\n%s", err, l.CommandLineSyntax())
	}
}

// ---------------------------------------------------------------------------
// Launcher fixtures (the testdata/cli and testdata/wire schemas of
// scripts/gen-go-launcher).

type procFixture struct {
	Name       string   `json:"name"`
	Launcher   string   `json:"launcher"`
	Args       []string `json:"args"`
	Stdin      string   `json:"stdin"`
	Stdout     string   `json:"stdout"`
	Stderr     string   `json:"stderr"`
	ExitStatus int      `json:"exit_status"`
	Error      *string  `json:"error"`
}

type webResponse struct {
	Status     int         `json:"status"`
	StatusText string      `json:"status_text"`
	Headers    [][2]string `json:"headers"`
	Body       *string     `json:"body"`
	BodyLength *int        `json:"body_length"`
	BodySHA256 *string     `json:"body_sha256"`
	BaseHref   *string     `json:"base_href"`
}

type webFixture struct {
	Name       string       `json:"name"`
	Server     string       `json:"server"`
	Launcher   string       `json:"launcher"`
	Args       []string     `json:"args"`
	Kind       string       `json:"kind"`
	Request    *wireRequest `json:"request"`
	Response   *webResponse `json:"response"`
	Log        []string     `json:"log"`
	ExitStatus *int         `json:"exit_status"`
	Stdout     *string      `json:"stdout"`
	Error      *string      `json:"error"`
}

var (
	logTimestamp   = regexp.MustCompile(`(?m)^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} `)
	requestLogLine = regexp.MustCompile(`^(<ts> [A-Z]+ \S+ )(?:\d+(?:\.\d+)?(?:ns|µs|ms|s|m|h))+$`)
	uuidPattern    = regexp.MustCompile(`[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`)
	// timeField is gen-go-launcher's N5 plus the agentengine session's
	// last_update_time.
	timeField = regexp.MustCompile(`"(lastUpdateTime|last_update_time|timestamp)":(-?[0-9][0-9.eE+-]*|"[^"]*")`)
	baseHref  = regexp.MustCompile(`(?is)<base\s[^>]*>`)
)

func portPattern(port int) *regexp.Regexp {
	return regexp.MustCompile(`(127\.0\.0\.1|localhost|port):` + strconv.Itoa(port) + `\b`)
}

func normText(s string, port int) string {
	s = logTimestamp.ReplaceAllString(s, "<ts> ")
	if port > 0 {
		s = portPattern(port).ReplaceAllString(s, "${1}:"+portPlaceholder)
	}
	return s
}

// normLog splits normalized stderr into lines (N1–N3), without the
// processingEvent lines.
func normLog(s string, port int) []string {
	s = normText(s, port)
	lines := strings.Split(s, "\n")
	if lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	out := []string{}
	for _, l := range lines {
		if strings.HasPrefix(l, "<ts> "+processingEvent) {
			continue
		}
		out = append(out, requestLogLine.ReplaceAllString(l, "${1}<elapsed>"))
	}
	return out
}

func hasWallClockTime(s string) bool {
	for _, m := range timeField.FindAllStringSubmatch(s, -1) {
		if strings.Contains(m[2], ".") {
			return true
		}
	}
	return false
}

func normBody(s string, port int) string {
	s = portPattern(port).ReplaceAllString(s, "${1}:"+portPlaceholder)
	s = timeField.ReplaceAllString(s, `"$1":"<t>"`)
	ids := map[string]string{}
	return uuidPattern.ReplaceAllStringFunc(s, func(u string) string {
		key := strings.ToLower(u)
		if p, ok := ids[key]; ok {
			return p
		}
		p := fmt.Sprintf("<uuid-%d>", len(ids)+1)
		ids[key] = p
		return p
	})
}

var self string

func childCommand(name string, args []string, errFile string) *exec.Cmd {
	cmd := exec.Command(self, append([]string{name}, args...)...)
	cmd.Env = []string{
		"PATH=" + os.Getenv("PATH"),
		"HOME=" + os.Getenv("HOME"),
		"TZ=UTC",
		childEnv + "=1",
		errFileEnv + "=" + errFile,
	}
	return cmd
}

func errFilePath() string {
	f, err := os.CreateTemp("", "gen-go-agentengine-err-*")
	check(err)
	check(f.Close())
	check(os.Remove(f.Name()))
	return f.Name()
}

func readError(errFile string) *string {
	data, err := os.ReadFile(errFile)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	check(err)
	check(os.Remove(errFile))
	s := string(data)
	return &s
}

func runProc(name, launcherName string, args []string) procFixture {
	errFile := errFilePath()
	cmd := childCommand(launcherName, args, errFile)
	cmd.Stdin = strings.NewReader("")
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	status := 0
	if err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			fatalf("%s: %v", name, err)
		}
		status = exit.ExitCode()
	}
	return procFixture{
		Name:       name,
		Launcher:   launcherName,
		Args:       args,
		Stdin:      "",
		Stdout:     stdout.String(),
		Stderr:     normText(stderr.String(), 0),
		ExitStatus: status,
		Error:      readError(errFile),
	}
}

type webServer struct {
	name    string
	cmd     *exec.Cmd
	port    int
	errFile string
	stderr  syncBuffer
	taken   int
	done    chan error
}

func freePort() int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	check(err)
	port := l.Addr().(*net.TCPAddr).Port
	check(l.Close())
	return port
}

func substitutePort(args []string, port int) []string {
	out := make([]string, len(args))
	for i, a := range args {
		out[i] = strings.ReplaceAll(a, portPlaceholder, strconv.Itoa(port))
	}
	return out
}

func startServer(name, launcherName string, args []string) *webServer {
	s := &webServer{name: name, port: freePort(), errFile: errFilePath(), done: make(chan error, 1)}
	s.cmd = childCommand(launcherName, substitutePort(args, s.port), s.errFile)
	s.cmd.Stdout = io.Discard
	s.cmd.Stderr = &s.stderr
	check(s.cmd.Start())
	go func() { s.done <- s.cmd.Wait() }()
	deadline := time.Now().Add(60 * time.Second)
	for {
		conn, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", s.port), time.Second)
		if err == nil {
			check(conn.Close())
			break
		}
		select {
		case err := <-s.done:
			fatalf("%s: server exited before listening: %v\nstderr:\n%s", name, err, s.stderr.String())
		default:
		}
		if time.Now().After(deadline) {
			_ = s.cmd.Process.Kill()
			fatalf("%s: server did not listen\nstderr:\n%s", name, s.stderr.String())
		}
		time.Sleep(20 * time.Millisecond)
	}
	return s
}

func (s *webServer) newLog() []string {
	last, since := s.stderr.Len(), time.Now()
	for time.Since(since) < quietPeriod {
		time.Sleep(5 * time.Millisecond)
		if n := s.stderr.Len(); n != last {
			last, since = n, time.Now()
		}
	}
	all := s.stderr.String()
	chunk := all[s.taken:]
	s.taken = len(all)
	if chunk == "" {
		return []string{}
	}
	return normLog(chunk, s.port)
}

// exchange sends one raw request on a fresh connection and reads to EOF.
func (s *webServer) exchange(req wireRequest) *webResponse {
	conn, err := net.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", s.port))
	check(err)
	defer conn.Close()
	check(conn.SetDeadline(time.Now().Add(60 * time.Second)))
	var out bytes.Buffer
	fmt.Fprintf(&out, "%s %s HTTP/1.1\r\n", req.Method, req.Target)
	for _, h := range req.Headers {
		fmt.Fprintf(&out, "%s: %s\r\n", h[0], strings.ReplaceAll(h[1], portPlaceholder, strconv.Itoa(s.port)))
	}
	out.WriteString("\r\n")
	out.WriteString(req.Body)
	_, err = conn.Write(out.Bytes())
	check(err)
	raw, err := io.ReadAll(conn)
	check(err)

	resp, err := http.ReadResponse(bufio.NewReader(bytes.NewReader(raw)), &http.Request{Method: req.Method})
	check(err)
	body, err := io.ReadAll(resp.Body)
	check(err)
	check(resp.Body.Close())

	head, _, ok := bytes.Cut(raw, []byte("\r\n\r\n"))
	if !ok {
		fatalf("no header block in %q", raw)
	}
	lines := strings.Split(string(head), "\r\n")
	statusText := ""
	if parts := strings.SplitN(lines[0], " ", 3); len(parts) == 3 {
		statusText = parts[2]
	}
	w := &webResponse{Status: resp.StatusCode, StatusText: statusText}

	normalized := false
	text := string(body)
	if len(body) <= inlineBodyLimit && utf8.Valid(body) {
		n := normBody(text, s.port)
		normalized = n != text
		w.Body = &n
	}
	if !hasWallClockTime(text) {
		n := len(body)
		w.BodyLength = &n
	}
	if w.Body == nil {
		sum := sha256.Sum256(body)
		h := hex.EncodeToString(sum[:])
		w.BodySHA256 = &h
	}
	if strings.HasPrefix(resp.Header.Get("Content-Type"), "text/html") {
		if m := baseHref.Find(body); m != nil {
			b := string(m)
			w.BaseHref = &b
		}
	}

	headers := [][2]string{}
	for _, line := range lines[1:] {
		name, value, ok := strings.Cut(line, ":")
		if !ok {
			fatalf("malformed header line %q", line)
		}
		name = http.CanonicalHeaderKey(name)
		value = strings.TrimSpace(value)
		switch {
		case name == "Date":
			value = "<date>"
		case name == "Content-Length" && normalized:
			value = "<n>"
		default:
			value = portPattern(s.port).ReplaceAllString(value, "${1}:"+portPlaceholder)
		}
		headers = append(headers, [2]string{name, value})
	}
	sortHeaders(headers)
	w.Headers = headers
	return w
}

// stop sends SIGTERM (the child cancels its context) and waits.
func (s *webServer) stop() {
	check(s.cmd.Process.Signal(syscall.SIGTERM))
	select {
	case <-s.done:
	case <-time.After(60 * time.Second):
		_ = s.cmd.Process.Kill()
		fatalf("%s: server did not exit", s.name)
	}
	_ = readError(s.errFile)
}

// webStep is one request; the probe adds Host first, Content-Length (body or
// POST) and Connection: close last (scripts/gen-go-launcher).
type webStep struct {
	name    string
	method  string
	target  string
	headers [][2]string
	body    string
}

func (s webStep) request() wireRequest {
	headers := [][2]string{{"Host", "127.0.0.1:" + portPlaceholder}}
	headers = append(headers, s.headers...)
	if s.body != "" || s.method == "POST" || s.method == "PUT" || s.method == "PATCH" {
		headers = append(headers, [2]string{"Content-Length", strconv.Itoa(len(s.body))})
	}
	headers = append(headers, [2]string{"Connection", "close"})
	return wireRequest{Method: s.method, Target: s.target, Headers: headers, Body: s.body}
}

type webScript struct {
	server string
	args   []string
	steps  []webStep
}

const (
	createU1     = `{"class_method":"async_create_session","input":{"user_id":"u1"}}`
	streamU1Text = `{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hello"}}`
)

func webScripts() []webScript {
	return []webScript{
		{
			server: "agentengine",
			args:   []string{"web", "-port", portPlaceholder, "agentengine"},
			steps: []webStep{
				{name: "create_session", method: "POST", target: "/api/reasoning_engine", headers: ct(), body: createU1},
				{name: "stream_query", method: "POST", target: "/api/stream_reasoning_engine", headers: ct(), body: streamU1Text},
				{name: "get_not_allowed", method: "GET", target: "/api/reasoning_engine"},
			},
		},
		{
			server: "prefix",
			args:   []string{"web", "-port", portPlaceholder, "agentengine", "-path_prefix", "custom/"},
			steps: []webStep{
				{name: "create_session", method: "POST", target: "/custom/reasoning_engine", headers: ct(), body: createU1},
			},
		},
	}
}

type cliCase struct {
	name     string
	launcher string
	args     []string
}

func cliCases() []cliCase {
	return []cliCase{
		{"help_top", "agentengine", []string{"-h"}},
		{"help_web", "agentengine", []string{"web", "-h"}},
		{"help_web_agentengine", "agentengine", []string{"web", "agentengine", "-h"}},
		{"bad_flag", "agentengine", []string{"web", "agentengine", "-nope"}},
		{"bad_max_payload_size", "agentengine", []string{"web", "agentengine", "-max_payload_size", "x"}},
		{"bad_sse_write_timeout", "agentengine", []string{"web", "agentengine", "-sse-write-timeout", "x"}},
		{"newhandler_error", "agentengine_bad_compaction", []string{"web", "-port", "0", "agentengine"}},
	}
}

func runWebScript(sc webScript, files fileSet) {
	prefix := "wire_" + sc.server
	s := startServer(prefix, "agentengine", sc.args)
	defer s.stop()
	fixture := func(name, kind string) webFixture {
		return webFixture{Name: name, Server: sc.server, Launcher: "agentengine", Args: sc.args, Kind: kind}
	}
	put := func(f webFixture) {
		files[filepath.Join(launcherDir, f.Name+".json")] = encode(f)
	}
	start := fixture(prefix+"_00_startup", "startup")
	start.Log = s.newLog()
	put(start)
	for i, st := range sc.steps {
		f := fixture(fmt.Sprintf("%s_%02d_%s", prefix, i+1, st.name), "exchange")
		req := st.request()
		f.Request = &req
		f.Response = s.exchange(req)
		f.Log = s.newLog()
		put(f)
	}
}

func generateLauncher() fileSet {
	files := fileSet{}
	for _, sc := range webScripts() {
		runWebScript(sc, files)
	}
	for _, c := range cliCases() {
		name := "cli_" + c.name
		files[filepath.Join(launcherDir, name+".json")] = encode(runProc(name, c.launcher, c.args))
	}
	return files
}

// ---------------------------------------------------------------------------
// --shapes

// adkGoDir is $ADK_GO_DIR, else ADK-Go beside this repository.
func adkGoDir() string {
	dir := os.Getenv("ADK_GO_DIR")
	if dir == "" {
		dir = "../../../adk-go"
	}
	abs, err := filepath.Abs(dir)
	check(err)
	return abs
}

func shapes(out string) {
	commit, err := os.ReadFile(commitFile)
	check(err)
	c := strings.TrimSpace(string(commit))
	if len(c) < 8 {
		fatalf("%s: short commit %q", commitFile, c)
	}
	src, err := filepath.Abs(shapesSrc)
	check(err)
	goDir := adkGoDir()
	tmp, err := os.MkdirTemp("", "gen-go-agentengine-shapes-*")
	check(err)
	defer os.RemoveAll(tmp)
	overlay := filepath.Join(tmp, "overlay.json")
	target := filepath.Join(goDir, "server", "agentengine", "zzgadkshapes", "main.go")
	check(os.WriteFile(overlay, []byte(mustJSON(map[string]any{"Replace": map[string]string{target: src}})), 0o644))

	cmd := exec.Command("go", "run", "-overlay", overlay, "./server/agentengine/zzgadkshapes", c[:8])
	cmd.Dir = goDir
	cmd.Env = append(os.Environ(), "GOFLAGS=-mod=mod")
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = os.Stderr
	check(cmd.Run())
	check(os.MkdirAll(filepath.Dir(out), 0o755))
	check(os.WriteFile(out, stdout.Bytes(), 0o644))
	fmt.Printf("wrote %s\n", out)
}

// ---------------------------------------------------------------------------
// Main.

func probeMain() {
	var err error
	self, err = os.Executable()
	check(err)
	for _, v := range []string{"OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN"} {
		check(os.Unsetenv(v))
	}

	// Record twice: every fixture must be byte-identical run to run.
	var runs [2]fileSet
	for i := range runs {
		runs[i] = generateServer()
		for p, data := range generateLauncher() {
			runs[i][p] = data
		}
	}
	var mismatches []string
	for p, want := range runs[0] {
		if got, ok := runs[1][p]; !ok || !bytes.Equal(got, want) {
			mismatches = append(mismatches, p)
		}
	}
	if len(mismatches) > 0 {
		sort.Strings(mismatches)
		for _, p := range mismatches {
			fmt.Fprintf(os.Stderr, "unstable fixture: %s\n--- first\n%s\n--- second\n%s\n", p, runs[0][p], runs[1][p])
		}
		os.Exit(1)
	}

	counts := map[string]int{}
	for _, dir := range []string{serverDir, launcherDir} {
		check(os.MkdirAll(dir, 0o755))
		old, err := filepath.Glob(filepath.Join(dir, "*.json"))
		check(err)
		for _, f := range old {
			if !strings.HasSuffix(f, ".deltas.json") {
				check(os.Remove(f))
			}
		}
	}
	for p, data := range runs[0] {
		check(os.WriteFile(p, data, 0o644))
		counts[filepath.Dir(p)]++
	}
	fmt.Printf("wrote server=%d launcher=%d fixtures\n", counts[serverDir], counts[launcherDir])
}

func main() {
	if os.Getenv(childEnv) != "" {
		childMain()
		return
	}
	args := os.Args[1:]
	if len(args) > 0 && args[0] == "--shapes" {
		out := shapesOut
		switch {
		case len(args) == 1:
		case len(args) == 3 && args[1] == "--out":
			out = args[2]
		default:
			fatal("usage: gen-go-agentengine [--shapes [--out PATH]]")
		}
		shapes(out)
		return
	}
	if len(args) > 0 {
		fatal("usage: gen-go-agentengine [--shapes [--out PATH]]")
	}
	probeMain()
}

// probeLog reports the probe's own failures on stderr: the server fixtures
// redirect the standard logger into the recorded log.
var probeLog = log.New(os.Stderr, "gen-go-agentengine: ", 0)

func fatal(v ...any) { probeLog.Fatal(v...) }

func fatalf(format string, v ...any) { probeLog.Fatalf(format, v...) }

func check(err error) {
	if err != nil {
		fatal(err)
	}
}
