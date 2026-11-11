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

// Command gen-go-a2a runs ADK-Go's A2A v2 executor (server/adka2a/v2) behind
// a2a-go's a2asrv (JSON-RPC and REST bindings plus a static agent card) on
// loopback listeners, drives it with raw HTTP requests, runs ADK-Go's v2
// remote agent (agent/remoteagent/v2) against it, and writes every scenario of
// SCENARIO.md as one normalized gadk-a2a wire fixture. Each fixture is then
// replayed (server units against a fresh ADK-Go server, client units against
// a fake server serving the fixture's responses) and must reproduce itself.
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
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/a2aproject/a2a-go/v2/a2a"
	"github.com/a2aproject/a2a-go/v2/a2asrv"
	"github.com/google/uuid"
	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/agent/remoteagent/v2"
	"google.golang.org/adk/v2/agent/workflowagents/loopagent"
	"google.golang.org/adk/v2/agent/workflowagents/parallelagent"
	"google.golang.org/adk/v2/agent/workflowagents/sequentialagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/runner"
	"google.golang.org/adk/v2/server/adka2a/v2"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
)

const outDir = "../../crates/gadk-a2a/testdata/wire"

// t0 is the first value a fixed clock returns; every read advances it 1 s.
var t0 = time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)

// ---------------------------------------------------------------------------
// Fixed providers.

// fixedPlatform is one clock and UUID sequence. variant distinguishes the
// server-side sequence ("8000") from the client-side one ("9000"), so the two
// never produce the same ID inside one fixture unit.
type fixedPlatform struct {
	mu      sync.Mutex
	variant string
	reads   int
	ids     int
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
	return fmt.Sprintf("00000000-0000-4000-%s-%012d", p.variant, p.ids)
}

func (p *fixedPlatform) attach(ctx context.Context) context.Context {
	ctx = platform.WithTimeProvider(ctx, p.now)
	return platform.WithUUIDProvider(ctx, p.uuid)
}

// ---------------------------------------------------------------------------
// Scripted models.

// scriptedModel answers from a fixed function of the request and the stream
// flag; every response in the returned list is yielded in order.
type scriptedModel struct {
	name  string
	reply func(req *model.LLMRequest, stream bool) ([]*model.LLMResponse, error)
}

func (m *scriptedModel) Name() string { return m.name }

func (m *scriptedModel) GenerateContent(_ context.Context, req *model.LLMRequest, stream bool) iter.Seq2[*model.LLMResponse, error] {
	return func(yield func(*model.LLMResponse, error) bool) {
		resps, err := m.reply(req, stream)
		if err != nil {
			yield(nil, err)
			return
		}
		for _, r := range resps {
			if !yield(r, nil) {
				return
			}
		}
	}
}

func usage() *genai.GenerateContentResponseUsageMetadata {
	return &genai.GenerateContentResponseUsageMetadata{PromptTokenCount: 10, CandidatesTokenCount: 5, TotalTokenCount: 15}
}

// final is the one complete response of a model turn.
func final(parts ...*genai.Part) *model.LLMResponse {
	return &model.LLMResponse{
		Content:       genai.NewContentFromParts(parts, genai.RoleModel),
		UsageMetadata: usage(),
		FinishReason:  genai.FinishReasonStop,
	}
}

// chunk is one partial streaming response.
func chunk(text string) *model.LLMResponse {
	return &model.LLMResponse{Content: genai.NewContentFromText(text, genai.RoleModel), Partial: true}
}

func lastContent(req *model.LLMRequest) *genai.Content {
	if n := len(req.Contents); n > 0 {
		return req.Contents[n-1]
	}
	return nil
}

func functionResponses(c *genai.Content) []*genai.FunctionResponse {
	if c == nil {
		return nil
	}
	var out []*genai.FunctionResponse
	for _, p := range c.Parts {
		if p.FunctionResponse != nil {
			out = append(out, p.FunctionResponse)
		}
	}
	return out
}

func textReply(text string) func(*model.LLMRequest, bool) ([]*model.LLMResponse, error) {
	return func(*model.LLMRequest, bool) ([]*model.LLMResponse, error) {
		return []*model.LLMResponse{final(genai.NewPartFromText(text))}, nil
	}
}

// ---------------------------------------------------------------------------
// Agents.

func must[T any](v T, err error) T {
	check(err)
	return v
}

// assistant answers every message with one text.
func newAssistant() agent.Agent {
	return must(llmagent.New(llmagent.Config{
		Name:        "assistant",
		Description: "Answers greetings.",
		Instruction: "Answer briefly.",
		Model:       &scriptedModel{name: "scripted-assistant", reply: textReply("Hello! How can I help?")},
	}))
}

type weatherArgs struct {
	City string `json:"city"`
}

type weatherResult struct {
	Forecast string `json:"forecast"`
}

// weatherReply streams two chunks per turn when stream is set: first a text
// plus a get_weather call, then (after the tool answered) the forecast.
func weatherReply(req *model.LLMRequest, stream bool) ([]*model.LLMResponse, error) {
	var out []*model.LLMResponse
	if len(functionResponses(lastContent(req))) > 0 {
		if stream {
			out = append(out, chunk("It is "), chunk("sunny."))
		}
		return append(out, final(genai.NewPartFromText("It is sunny."))), nil
	}
	if stream {
		out = append(out, chunk("Let me "), chunk("check."))
	}
	return append(out, final(
		genai.NewPartFromText("Let me check."),
		genai.NewPartFromFunctionCall("get_weather", map[string]any{"city": "Paris"}),
	)), nil
}

func newWeather() agent.Agent {
	getWeather := must(functiontool.New(functiontool.Config{
		Name:        "get_weather",
		Description: "Returns the forecast for a city.",
	}, func(agent.Context, weatherArgs) (weatherResult, error) {
		return weatherResult{Forecast: "sunny"}, nil
	}))
	return must(llmagent.New(llmagent.Config{
		Name:        "weather",
		Description: "Reports the weather.",
		Instruction: "Use get_weather to answer.",
		Model:       &scriptedModel{name: "scripted-weather", reply: weatherReply},
		Tools:       []tool.Tool{getWeather},
	}))
}

type approvalResult struct {
	Status   string `json:"status"`
	TicketID string `json:"ticket_id"`
}

// approverReply follows ADK-Go's a2a_e2e_test.go input-requesting agent.
func approverReply(req *model.LLMRequest, _ bool) ([]*model.LLMResponse, error) {
	responses := functionResponses(lastContent(req))
	switch {
	case len(responses) == 0:
		return []*model.LLMResponse{final(
			genai.NewPartFromText("need to request approval first!"),
			genai.NewPartFromFunctionCall("request_approval", map[string]any{}),
		)}, nil
	case len(responses) == 1 && responses[0].Response["status"] == "pending":
		return []*model.LLMResponse{final(genai.NewPartFromText("waiting for user's approval..."))}, nil
	default:
		return []*model.LLMResponse{final(genai.NewPartFromText("Task complete!"))}, nil
	}
}

func newApprover() agent.Agent {
	requestApproval := must(functiontool.New(functiontool.Config{
		Name:          "request_approval",
		Description:   "Request approval before proceeding.",
		IsLongRunning: true,
	}, func(agent.Context, map[string]any) (approvalResult, error) {
		return approvalResult{Status: "pending", TicketID: "ticket-1"}, nil
	}))
	return must(llmagent.New(llmagent.Config{
		Name:        "approver",
		Description: "Requests approval before acting.",
		Instruction: "Ask for approval first.",
		Model:       &scriptedModel{name: "scripted-approver", reply: approverReply},
		Tools:       []tool.Tool{requestApproval},
	}))
}

func newBroken() agent.Agent {
	return must(llmagent.New(llmagent.Config{
		Name:        "broken",
		Description: "Always fails.",
		Instruction: "Fail.",
		Model: &scriptedModel{name: "scripted-broken", reply: func(*model.LLMRequest, bool) ([]*model.LLMResponse, error) {
			return nil, errors.New("scripted model failure")
		}},
	}))
}

// newRoot transfers every request to its one (remote) sub-agent.
func newRoot(child agent.Agent) agent.Agent {
	return must(llmagent.New(llmagent.Config{
		Name:        "root",
		Description: "Delegates to the child agent.",
		Instruction: "Transfer to child.",
		SubAgents:   []agent.Agent{child},
		Model: &scriptedModel{name: "scripted-root", reply: func(*model.LLMRequest, bool) ([]*model.LLMResponse, error) {
			return []*model.LLMResponse{final(
				genai.NewPartFromText("transferring to child"),
				genai.NewPartFromFunctionCall("transfer_to_agent", map[string]any{"agent_name": child.Name()}),
			)}, nil
		}},
	}))
}

type lookupArgs struct {
	Query string `json:"query"`
}

// newPipeline is the agent_card_workflow tree: sequential(llm with a tool,
// parallel(llm), loop(llm), custom).
func newPipeline() agent.Agent {
	lookup := must(functiontool.New(functiontool.Config{Name: "lookup"},
		func(agent.Context, lookupArgs) (map[string]any, error) { return map[string]any{"found": true}, nil }))
	drafter := must(llmagent.New(llmagent.Config{
		Name:        "drafter",
		Description: "Writes a draft.",
		Instruction: "You write a draft of your reply.",
		Model:       &scriptedModel{name: "scripted-drafter", reply: textReply("Draft ready.")},
		Tools:       []tool.Tool{lookup},
	}))
	factChecker := must(llmagent.New(llmagent.Config{
		Name:        "fact_checker",
		Description: "Checks facts.",
		Instruction: "Check the facts.",
		Model:       &scriptedModel{name: "scripted-fact-checker", reply: textReply("Facts checked.")},
	}))
	checks := must(parallelagent.New(parallelagent.Config{AgentConfig: agent.Config{
		Name:      "checks",
		SubAgents: []agent.Agent{factChecker},
	}}))
	editor := must(llmagent.New(llmagent.Config{
		Name:        "editor",
		Instruction: "You're the editor; polish your draft.",
		Model:       &scriptedModel{name: "scripted-editor", reply: textReply("Polished.")},
	}))
	polisher := must(loopagent.New(loopagent.Config{
		AgentConfig:   agent.Config{Name: "polisher", Description: "Polishes the draft.", SubAgents: []agent.Agent{editor}},
		MaxIterations: 1,
	}))
	stamper := must(agent.New(agent.Config{
		Name: "stamper",
		Run: func(ic agent.InvocationContext) iter.Seq2[*session.Event, error] {
			return func(yield func(*session.Event, error) bool) {
				ev := session.NewEvent(ic, ic.InvocationID())
				ev.Author = "stamper"
				ev.Branch = ic.Branch()
				ev.Content = genai.NewContentFromText("Stamped.", genai.RoleModel)
				yield(ev, nil)
			}
		},
	}))
	return must(sequentialagent.New(sequentialagent.Config{AgentConfig: agent.Config{
		Name:        "pipeline",
		Description: "Drafts, checks, polishes and stamps a reply.",
		SubAgents:   []agent.Agent{drafter, checks, polisher, stamper},
	}}))
}

// ---------------------------------------------------------------------------
// Wire records.

// wireRequest is one HTTP request as the server received it.
type wireRequest struct {
	Method  string            `json:"method"`
	Path    string            `json:"path"`
	Headers map[string]string `json:"headers"`
	Body    any               `json:"body"`
}

// wireResponse is one HTTP response as the handler wrote it. Exactly one of
// Body (a JSON document, or a string for a non-JSON body, or null) and SSE
// (the parsed data: payloads of a text/event-stream body) is meaningful.
type wireResponse struct {
	Status  int               `json:"status"`
	Headers map[string]string `json:"headers"`
	Body    any               `json:"body"`
	SSE     []any             `json:"sse,omitempty"`
}

type exchange struct {
	Request  wireRequest  `json:"request"`
	Response wireResponse `json:"response"`
	// Child holds the exchanges the scenario's child server received while
	// this exchange ran (and until the scenario's wait for it ended).
	Child []exchange `json:"child,omitempty"`
}

// Request headers that are transport details, not A2A behavior.
var droppedRequestHeaders = map[string]bool{
	"Host": true, "Content-Length": true, "Accept-Encoding": true, "User-Agent": true, "Connection": true,
}

func joinHeaders(h http.Header, drop map[string]bool) map[string]string {
	out := map[string]string{}
	for name, values := range h {
		name = http.CanonicalHeaderKey(name)
		if drop[name] {
			continue
		}
		out[name] = strings.Join(values, ", ")
	}
	return out
}

// parseBody decodes a JSON body; an empty body is null and a non-JSON body
// is kept as a string.
func parseBody(b []byte) any {
	if len(bytes.TrimSpace(b)) == 0 {
		return nil
	}
	var v any
	if err := json.Unmarshal(b, &v); err != nil {
		return string(b)
	}
	return v
}

// parseSSE returns the data: payloads of an event stream, each parsed as JSON.
func parseSSE(b []byte) []any {
	out := []any{}
	for _, frame := range strings.Split(string(b), "\n\n") {
		var data []string
		for _, line := range strings.Split(frame, "\n") {
			if rest, ok := strings.CutPrefix(line, "data:"); ok {
				data = append(data, strings.TrimPrefix(rest, " "))
			}
		}
		if len(data) == 0 {
			continue
		}
		var v any
		check(json.Unmarshal([]byte(strings.Join(data, "\n")), &v))
		out = append(out, v)
	}
	return out
}

// recordingWriter tees what a handler writes and keeps streaming working.
type recordingWriter struct {
	http.ResponseWriter
	status  int
	headers http.Header
	body    bytes.Buffer
}

func (w *recordingWriter) WriteHeader(code int) {
	if w.headers == nil {
		w.status = code
		w.headers = w.ResponseWriter.Header().Clone()
	}
	w.ResponseWriter.WriteHeader(code)
}

func (w *recordingWriter) Write(b []byte) (int, error) {
	if w.headers == nil {
		w.WriteHeader(http.StatusOK)
	}
	w.body.Write(b)
	return w.ResponseWriter.Write(b)
}

func (w *recordingWriter) Flush() {
	if f, ok := w.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

func (w *recordingWriter) Unwrap() http.ResponseWriter { return w.ResponseWriter }

// recorder logs every exchange a server handled, in completion order.
type recorder struct {
	mu        sync.Mutex
	exchanges []exchange
	inflight  int
}

func (r *recorder) wrap(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		r.mu.Lock()
		r.inflight++
		r.mu.Unlock()
		body, err := io.ReadAll(req.Body)
		check(err)
		req.Body = io.NopCloser(bytes.NewReader(body))
		rw := &recordingWriter{ResponseWriter: w}
		next.ServeHTTP(rw, req)
		if rw.headers == nil {
			rw.status, rw.headers = http.StatusOK, w.Header().Clone()
		}
		resp := wireResponse{Status: rw.status, Headers: joinHeaders(rw.headers, nil)}
		if strings.HasPrefix(rw.headers.Get("Content-Type"), "text/event-stream") {
			resp.SSE = parseSSE(rw.body.Bytes())
		} else {
			resp.Body = parseBody(rw.body.Bytes())
		}
		ex := exchange{
			Request: wireRequest{
				Method:  req.Method,
				Path:    req.URL.RequestURI(),
				Headers: joinHeaders(req.Header, droppedRequestHeaders),
				Body:    parseBody(body),
			},
			Response: resp,
		}
		r.mu.Lock()
		r.exchanges = append(r.exchanges, ex)
		r.inflight--
		r.mu.Unlock()
	})
}

func (r *recorder) len() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.exchanges)
}

func (r *recorder) since(n int) []exchange {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]exchange(nil), r.exchanges[n:]...)
}

// waitFor polls until the recorder holds at least n exchanges.
func (r *recorder) waitFor(n int, what string) {
	deadline := time.Now().Add(10 * time.Second)
	for r.len() < n {
		if time.Now().After(deadline) {
			log.Fatalf("timed out waiting for %s (%d exchanges recorded, want %d)", what, r.len(), n)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// waitIdle polls until no request is in flight.
func (r *recorder) waitIdle() {
	deadline := time.Now().Add(10 * time.Second)
	for {
		r.mu.Lock()
		n := r.inflight
		r.mu.Unlock()
		if n == 0 {
			return
		}
		if time.Now().After(deadline) {
			log.Fatal("timed out waiting for in-flight requests")
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// ---------------------------------------------------------------------------
// A2A servers.

// Server layout (SCENARIO.md "Server layout").
const (
	cardPath    = "/.well-known/agent-card.json"
	jsonrpcPath = "/jsonrpc"
	restPrefix  = "/rest"
)

type a2aServer struct {
	ts   *httptest.Server
	rec  *recorder
	base string
	card *a2a.AgentCard
}

func (s *a2aServer) close() { s.ts.Close() }

// executorSpec describes the ADK executor of one server.
type executorSpec struct {
	agent     func() agent.Agent
	mode      adka2a.OutputMode
	streaming agent.StreamingMode
}

// newCard is the agent card every server serves: both bindings unless
// interfaces names a subset ("jsonrpc", "rest").
func newCard(ag agent.Agent, base string, interfaces ...string) *a2a.AgentCard {
	if len(interfaces) == 0 {
		interfaces = []string{"jsonrpc", "rest"}
	}
	var ifaces []*a2a.AgentInterface
	for _, i := range interfaces {
		switch i {
		case "jsonrpc":
			ifaces = append(ifaces, a2a.NewAgentInterface(base+jsonrpcPath, a2a.TransportProtocolJSONRPC))
		case "rest":
			ifaces = append(ifaces, a2a.NewAgentInterface(base+restPrefix, a2a.TransportProtocolHTTPJSON))
		}
	}
	return &a2a.AgentCard{
		Name:                ag.Name(),
		Description:         ag.Description(),
		Version:             "1.0.0",
		SupportedInterfaces: ifaces,
		Capabilities:        a2a.AgentCapabilities{Streaming: true},
		DefaultInputModes:   []string{"text/plain"},
		DefaultOutputModes:  []string{"text/plain"},
		Skills:              adka2a.BuildAgentSkills(ag),
	}
}

// startExecutorServer serves an ADK-Go executor for spec.agent() through
// a2asrv with the fixed server-side providers on every request.
func startExecutorServer(spec executorSpec, interfaces ...string) *a2aServer {
	return startExecutorServerWithSessions(spec, session.InMemoryService(), interfaces...)
}

// startExecutorServerWithSessions is startExecutorServer with a given
// session service, so the harness can read the agent's session afterwards.
func startExecutorServerWithSessions(spec executorSpec, sessions session.Service, interfaces ...string) *a2aServer {
	ag := spec.agent()
	executor := adka2a.NewExecutor(adka2a.ExecutorConfig{
		RunnerConfig: runner.Config{AppName: ag.Name(), Agent: ag, SessionService: sessions},
		RunConfig:    agent.RunConfig{StreamingMode: spec.streaming},
		OutputMode:   spec.mode,
	})
	handler := a2asrv.NewHandler(executor)
	p := &fixedPlatform{variant: "8000"}
	s := &a2aServer{rec: &recorder{}}
	var mux http.Handler
	s.ts = httptest.NewServer(s.rec.wrap(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mux.ServeHTTP(w, r.WithContext(p.attach(r.Context())))
	})))
	s.base = s.ts.URL
	s.card = newCard(ag, s.base, interfaces...)
	m := http.NewServeMux()
	m.Handle(cardPath, a2asrv.NewStaticAgentCardHandler(s.card))
	m.Handle(jsonrpcPath, a2asrv.NewJSONRPCHandler(handler))
	m.Handle(restPrefix+"/", http.StripPrefix(restPrefix, a2asrv.NewRESTHandler(handler)))
	mux = m
	return s
}

// ---------------------------------------------------------------------------
// Normalization (SCENARIO.md "Normalization").

var (
	uuidRE        = regexp.MustCompile(`[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`)
	placeholderRE = regexp.MustCompile(`<(task|ctx|msg|artifact|id)-[0-9]+>`)
)

const tsPlaceholder = "<t>"

// toValue round-trips v through encoding/json into plain maps and slices.
func toValue(v any) any {
	b, err := json.Marshal(v)
	check(err)
	var out any
	check(json.Unmarshal(b, &out))
	return out
}

// walk visits every string value in canonical order (object keys sorted
// bytewise, arrays in order). key is the object key the string (or the array
// holding it) is stored under, parent that object. f returns the string's
// replacement.
func walk(v any, parent map[string]any, key string, f func(s string, parent map[string]any, key string) string) any {
	switch t := v.(type) {
	case map[string]any:
		keys := make([]string, 0, len(t))
		for k := range t {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			t[k] = walk(t[k], t, k, f)
		}
		return t
	case []any:
		for i := range t {
			t[i] = walk(t[i], parent, key, f)
		}
		return t
	case string:
		return f(t, parent, key)
	default:
		return v
	}
}

// replaceTimestamps sets the value of every "timestamp" key to "<t>".
func replaceTimestamps(v any) {
	switch t := v.(type) {
	case map[string]any:
		for k, val := range t {
			if k == "timestamp" {
				t[k] = tsPlaceholder
				continue
			}
			replaceTimestamps(val)
		}
	case []any:
		for _, e := range t {
			replaceTimestamps(e)
		}
	}
}

// checkKeys fails on any object key that contains a UUID: such a key would
// make the canonical order depend on random values.
func checkKeys(v any) {
	switch t := v.(type) {
	case map[string]any:
		for k, val := range t {
			if uuidRE.MatchString(k) {
				log.Fatalf("object key %q contains a UUID", k)
			}
			checkKeys(val)
		}
	case []any:
		for _, e := range t {
			checkKeys(e)
		}
	}
}

// idKind is the placeholder kind of a string that is exactly a UUID.
func idKind(parent map[string]any, key string) string {
	switch key {
	case "taskId", "a2a:task_id", "referenceTaskIds":
		return "task"
	case "contextId", "a2a:context_id":
		return "ctx"
	case "messageId":
		return "msg"
	case "artifactId":
		return "artifact"
	case "id":
		_, hasStatus := parent["status"]
		_, hasContext := parent["contextId"]
		if hasStatus && hasContext {
			return "task"
		}
	}
	return "id"
}

// normalizer maps live values to placeholders, first seen first numbered.
type normalizer struct {
	bases  [][2]string // {live base URL, placeholder}
	ids    map[string]string
	counts map[string]int
}

func newNormalizer(bases map[string]string) *normalizer {
	n := &normalizer{ids: map[string]string{}, counts: map[string]int{}}
	for live, ph := range bases {
		n.bases = append(n.bases, [2]string{live, ph})
	}
	sort.Slice(n.bases, func(i, j int) bool { return len(n.bases[i][0]) > len(n.bases[j][0]) })
	return n
}

func (n *normalizer) assign(id, kind string) string {
	if ph, ok := n.ids[id]; ok {
		return ph
	}
	n.counts[kind]++
	ph := fmt.Sprintf("<%s-%d>", kind, n.counts[kind])
	n.ids[id] = ph
	return ph
}

// normalize returns the normalized copy of doc.
func (n *normalizer) normalize(doc any) any {
	v := toValue(doc)
	checkKeys(v)
	v = walk(v, nil, "", func(s string, _ map[string]any, _ string) string {
		for _, b := range n.bases {
			s = strings.ReplaceAll(s, b[0], b[1])
		}
		return s
	})
	replaceTimestamps(v)
	return walk(v, nil, "", func(s string, parent map[string]any, key string) string {
		if uuidRE.MatchString(s) && uuidRE.FindString(s) == s {
			return n.assign(s, idKind(parent, key))
		}
		return uuidRE.ReplaceAllStringFunc(s, func(id string) string { return n.assign(id, "id") })
	})
}

// normalizeUnit normalizes a whole unit document with a fresh normalizer.
func normalizeUnit(doc any, bases map[string]string) any {
	return newNormalizer(bases).normalize(doc)
}

// align binds the placeholders of a normalized value to the live values at
// the same positions of an actual (unnormalized) value: objects by key, arrays
// by index, strings by matching the normalized string as a template in which
// each <kind-N> placeholder matches one UUID. Positions whose shapes differ
// are skipped; a placeholder already bound keeps its first binding.
func align(fx, actual any, values map[string]string) {
	switch f := fx.(type) {
	case map[string]any:
		a, ok := actual.(map[string]any)
		if !ok {
			return
		}
		for k, v := range f {
			if av, ok := a[k]; ok {
				align(v, av, values)
			}
		}
	case []any:
		a, ok := actual.([]any)
		if !ok {
			return
		}
		for i := range f {
			if i < len(a) {
				align(f[i], a[i], values)
			}
		}
	case string:
		a, ok := actual.(string)
		if !ok {
			return
		}
		phs := placeholderRE.FindAllString(f, -1)
		if len(phs) == 0 {
			return
		}
		pattern := "^" + placeholderRE.ReplaceAllStringFunc(regexp.QuoteMeta(f), func(string) string {
			return "(" + uuidRE.String() + ")"
		}) + "$"
		m := regexp.MustCompile(pattern).FindStringSubmatch(a)
		if m == nil {
			return
		}
		for i, ph := range phs {
			if _, ok := values[ph]; !ok {
				values[ph] = m[i+1]
			}
		}
	}
}

// denormalize replaces placeholders in a normalized value: <kind-N> by
// values[<kind-N>] (a fresh UUID, remembered in values, when missing), "<t>"
// by t0 in RFC 3339, and each base placeholder by its URL.
func denormalize(v any, values map[string]string, bases map[string]string) any {
	return walk(toValue(v), nil, "", func(s string, _ map[string]any, _ string) string {
		if s == tsPlaceholder {
			return t0.Format(time.RFC3339)
		}
		for ph, live := range bases {
			s = strings.ReplaceAll(s, ph, live)
		}
		return placeholderRE.ReplaceAllStringFunc(s, func(ph string) string {
			if live, ok := values[ph]; ok {
				return live
			}
			live := uuid.NewString()
			values[ph] = live
			return live
		})
	})
}

// ---------------------------------------------------------------------------
// Server direction: requests built from earlier responses.

// binding builds the HTTP request of one A2A operation.
type binding string

const (
	bindingJSONRPC binding = "jsonrpc"
	bindingREST    binding = "rest"
)

type op struct {
	kind   string // card, send, stream, get, cancel
	params map[string]any
}

// httpCall is one request the harness sends; a nil body sends no body.
type httpCall struct {
	method  string
	path    string
	headers [][2]string
	body    any
}

// httpRequest returns the HTTP request of an operation, with
// the headers a2a-go's own client (a2aclient v2.6.0) sends; rpcID is the
// JSON-RPC request id (the 1-based step number).
func (b binding) httpRequest(o op, rpcID int) httpCall {
	if o.kind == "card" {
		return httpCall{"GET", cardPath, nil, nil}
	}
	version := [2]string{"A2A-Version", "1.0"}
	ct := [2]string{"Content-Type", "application/json"}
	acceptJSON := [2]string{"Accept", "application/json"}
	acceptSSE := [2]string{"Accept", "text/event-stream"}
	if b == bindingJSONRPC {
		method := map[string]string{
			"send": "SendMessage", "stream": "SendStreamingMessage", "get": "GetTask", "cancel": "CancelTask",
		}[o.kind]
		headers := [][2]string{version, ct}
		if o.kind == "stream" {
			headers = append(headers, acceptSSE)
		}
		return httpCall{"POST", jsonrpcPath, headers, map[string]any{
			"jsonrpc": "2.0", "id": rpcID, "method": method, "params": o.params,
		}}
	}
	switch o.kind {
	case "send":
		return httpCall{"POST", restPrefix + "/message:send", [][2]string{version, ct, acceptJSON}, o.params}
	case "stream":
		return httpCall{"POST", restPrefix + "/message:stream", [][2]string{version, ct, acceptSSE}, o.params}
	case "get":
		return httpCall{"GET", restPrefix + "/tasks/" + o.params["id"].(string), [][2]string{version, acceptJSON}, nil}
	case "cancel":
		return httpCall{"POST", restPrefix + "/tasks/" + o.params["id"].(string) + ":cancel", [][2]string{version, acceptJSON}, nil}
	}
	log.Fatalf("unknown op %q", o.kind)
	return httpCall{}
}

// result is the A2A result of a non-streaming response (JSON-RPC "result" or
// the REST body).
func result(ex exchange) map[string]any {
	body, _ := ex.Response.Body.(map[string]any)
	if r, ok := body["result"].(map[string]any); ok {
		return r
	}
	return body
}

// firstStreamTask is the task of a streaming response's first payload.
func firstStreamTask(ex exchange) map[string]any {
	if len(ex.Response.SSE) == 0 {
		log.Fatalf("no SSE payloads in %+v", ex.Response)
	}
	p, _ := ex.Response.SSE[0].(map[string]any)
	if r, ok := p["result"].(map[string]any); ok {
		p = r
	}
	return p["task"].(map[string]any)
}

// taskOf is the task a send or stream exchange created.
func taskOf(ex exchange) map[string]any {
	if ex.Response.SSE != nil {
		return firstStreamTask(ex)
	}
	return result(ex)["task"].(map[string]any)
}

func userMessage(id string, parts ...any) map[string]any {
	return map[string]any{"messageId": id, "role": "ROLE_USER", "parts": parts}
}

func text(s string) map[string]any { return map[string]any{"text": s} }

func forTask(msg map[string]any, task map[string]any) map[string]any {
	msg["taskId"] = task["id"]
	msg["contextId"] = task["contextId"]
	return msg
}

func inContext(msg map[string]any, task map[string]any) map[string]any {
	msg["contextId"] = task["contextId"]
	return msg
}

func send(msg map[string]any) op   { return op{kind: "send", params: map[string]any{"message": msg}} }
func stream(msg map[string]any) op { return op{kind: "stream", params: map[string]any{"message": msg}} }
func getTask(task map[string]any) op {
	return op{kind: "get", params: map[string]any{"id": task["id"]}}
}
func cancelTask(task map[string]any) op {
	return op{kind: "cancel", params: map[string]any{"id": task["id"]}}
}

// pendingCallID is the id of the long-running function call in an
// input-required task's status message.
func pendingCallID(task map[string]any) string {
	msg := task["status"].(map[string]any)["message"].(map[string]any)
	for _, p := range msg["parts"].([]any) {
		part := p.(map[string]any)
		meta, _ := part["metadata"].(map[string]any)
		if meta["adk_type"] == "function_call" {
			return part["data"].(map[string]any)["id"].(string)
		}
	}
	log.Fatalf("no function_call part in %v", msg)
	return ""
}

// step builds the next operation from the exchanges so far.
type step func(prev []exchange) op

// serverScenario is the server direction of one scenario.
type serverScenario struct {
	spec  executorSpec
	child *executorSpec // when set, the agent is newRoot(remote agent "child" -> child server)
	steps []step
	// waitChild, when set, returns how many child exchanges the scenario waits
	// for after the given 0-based step before the next step.
	waitChild func(i int) int
}

// sendRaw sends one request and waits until the server recorded it.
func sendRaw(client *http.Client, s *a2aServer, c httpCall) {
	var r io.Reader = http.NoBody
	if c.body != nil {
		b, err := json.Marshal(c.body)
		check(err)
		r = bytes.NewReader(b)
	}
	req, err := http.NewRequest(c.method, s.base+c.path, r)
	check(err)
	for _, h := range c.headers {
		req.Header.Set(h[0], h[1])
	}
	n := s.rec.len()
	resp, err := client.Do(req)
	check(err)
	_, err = io.ReadAll(resp.Body)
	check(err)
	check(resp.Body.Close())
	s.rec.waitFor(n+1, c.method+" "+c.path)
}

// serverUnit is one binding of a scenario's server direction as recorded.
type serverUnit struct {
	Exchanges     []exchange        `json:"exchanges"`
	SessionEvents []json.RawMessage `json:"session_events,omitempty"`
}

// startScenarioServers starts the server under test (and its child).
func startScenarioServers(sc serverScenario) (*a2aServer, *a2aServer, session.Service) {
	if sc.child == nil {
		return startExecutorServer(sc.spec), nil, nil
	}
	child := startExecutorServer(*sc.child, "jsonrpc")
	remote := must(remoteagent.NewA2A(remoteagent.A2AConfig{
		Name:        "child",
		Description: "Remote approver.",
		AgentCard:   child.card,
	}))
	sessions := session.InMemoryService()
	spec := sc.spec
	spec.agent = func() agent.Agent { return newRoot(remote) }
	return startExecutorServerWithSessions(spec, sessions), child, sessions
}

// sessionEvents returns the root agent's stored session events for the
// context of the scenario's first task.
func sessionEvents(sessions session.Service, appName, contextID string) []json.RawMessage {
	resp, err := sessions.Get(context.Background(), &session.GetRequest{
		AppName: appName, UserID: "A2A_USER_" + contextID, SessionID: contextID,
	})
	check(err)
	var out []json.RawMessage
	for ev := range resp.Session.Events().All() {
		b, err := json.Marshal(ev)
		check(err)
		out = append(out, b)
	}
	return out
}

func bases(s, child *a2aServer) map[string]string {
	m := map[string]string{s.base: "<base>"}
	if child != nil {
		m[child.base] = "<child-base>"
	}
	return m
}

// driveServer starts the scenario's servers and sends n requests, the i-th
// built by next from the exchanges so far.
func driveServer(sc serverScenario, n int, next func(i int, done []exchange) httpCall) (serverUnit, map[string]string) {
	s, child, sessions := startScenarioServers(sc)
	defer s.close()
	if child != nil {
		defer child.close()
	}
	client := &http.Client{}
	var done []exchange
	for i := range n {
		childBefore := 0
		if child != nil {
			childBefore = child.rec.len()
		}
		sendRaw(client, s, next(i, done))
		ex := s.rec.since(len(done))[0]
		if child != nil {
			if sc.waitChild != nil {
				child.rec.waitFor(childBefore+sc.waitChild(i), "child exchanges")
			}
			ex.Child = child.rec.since(childBefore)
		}
		done = append(done, ex)
	}
	unit := serverUnit{Exchanges: done}
	if sessions != nil {
		unit.SessionEvents = sessionEvents(sessions, "root", taskOf(done[0])["contextId"].(string))
	}
	return unit, bases(s, child)
}

// runServerLive runs a server scenario's script on one binding and returns
// the normalized unit.
func runServerLive(sc serverScenario, b binding) any {
	unit, bs := driveServer(sc, len(sc.steps), func(i int, done []exchange) httpCall {
		return b.httpRequest(sc.steps[i](done), i+1)
	})
	return normalizeUnit(unit, bs)
}

// replayServer sends a fixture unit's requests to a fresh server, each
// denormalized with the placeholders bound by aligning the fixture's earlier
// exchanges with the actual ones, and returns the normalized result.
func replayServer(sc serverScenario, fixture map[string]any) any {
	fxs := fixture["exchanges"].([]any)
	unit, bs := driveServer(sc, len(fxs), func(i int, done []exchange) httpCall {
		values := map[string]string{}
		align(map[string]any{"exchanges": fxs[:i]}, toValue(serverUnit{Exchanges: done}), values)
		req := denormalize(fxs[i].(map[string]any)["request"], values, map[string]string{}).(map[string]any)
		c := httpCall{method: req["method"].(string), path: req["path"].(string), body: req["body"]}
		for k, v := range req["headers"].(map[string]any) {
			c.headers = append(c.headers, [2]string{k, v.(string)})
		}
		return c
	})
	return normalizeUnit(unit, bs)
}

// ---------------------------------------------------------------------------
// Client direction: ADK-Go's remote agent.

// clientRun is one runner.Run of the remote agent.
type clientRun struct {
	mode    agent.StreamingMode
	message func(prevEvents []*session.Event) *genai.Content
}

type clientScenario struct {
	runs []clientRun
	// cardProvider: resolve the card with NewAgentCardProvider(base) instead of
	// a static card naming only the unit's binding.
	cardProvider bool
}

type recordedRun struct {
	StreamingMode string            `json:"streaming_mode"`
	NewMessage    *genai.Content    `json:"new_message"`
	Exchanges     []exchange        `json:"exchanges"`
	Events        []json.RawMessage `json:"events"`
}

type clientUnit struct {
	Runs []recordedRun `json:"runs"`
}

func modeName(m agent.StreamingMode) string {
	if m == agent.StreamingModeSSE {
		return "sse"
	}
	return "none"
}

func userText(s string) func([]*session.Event) *genai.Content {
	return func([]*session.Event) *genai.Content { return genai.NewContentFromText(s, genai.RoleUser) }
}

// approvalFor answers the long-running request_approval call found in the
// earlier events.
func approvalFor(prev []*session.Event) *genai.Content {
	for _, ev := range prev {
		if ev.Content == nil {
			continue
		}
		for _, p := range ev.Content.Parts {
			if p.FunctionCall != nil && p.FunctionCall.Name == "request_approval" {
				return &genai.Content{Role: genai.RoleUser, Parts: []*genai.Part{{FunctionResponse: &genai.FunctionResponse{
					ID:       p.FunctionCall.ID,
					Name:     "request_approval",
					Response: map[string]any{"status": "approved", "ticket_id": "ticket-1"},
				}}}}
			}
		}
	}
	log.Fatal("no request_approval call in the remote agent's events")
	return nil
}

// runRemoteAgent runs the client scenario's runs against the server at base
// (recording through rec) and returns the unit before normalization.
func runRemoteAgent(cs clientScenario, card *a2a.AgentCard, cardSource string, rec *recorder) clientUnit {
	cfg := remoteagent.A2AConfig{Name: "remote", Description: "Remote A2A agent."}
	if cs.cardProvider {
		cfg.AgentCardProvider = remoteagent.NewAgentCardProvider(cardSource)
	} else {
		cfg.AgentCard = card
	}
	remote := must(remoteagent.NewA2A(cfg))
	sessions := session.InMemoryService()
	r := must(runner.New(runner.Config{AppName: "client", Agent: remote, SessionService: sessions}))
	p := &fixedPlatform{variant: "9000"}
	ctx := p.attach(context.Background())
	_, err := sessions.Create(ctx, &session.CreateRequest{AppName: "client", UserID: "user", SessionID: "s1"})
	check(err)
	var unit clientUnit
	var all []*session.Event
	for _, run := range cs.runs {
		msg := run.message(all)
		before := rec.len()
		rr := recordedRun{StreamingMode: modeName(run.mode), NewMessage: msg}
		for ev, err := range r.Run(ctx, "user", "s1", msg, agent.RunConfig{StreamingMode: run.mode}) {
			if err != nil {
				b, merr := json.Marshal(map[string]string{"error": err.Error()})
				check(merr)
				rr.Events = append(rr.Events, b)
				continue
			}
			all = append(all, ev)
			b, err := json.Marshal(ev)
			check(err)
			rr.Events = append(rr.Events, b)
		}
		// A cleanup CancelTask runs before Run returns; the client may stop
		// reading a stream before its handler finished recording.
		rec.waitIdle()
		rr.Exchanges = rec.since(before)
		unit.Runs = append(unit.Runs, rr)
	}
	return unit
}

// replayHandler serves a client unit's recorded responses in order. Each
// request is aligned with its recording first, so IDs the client generated
// (and the server echoed) are bound before the response is denormalized. A
// request whose method or path differs from the recording fails the run.
type replayHandler struct {
	mu        sync.Mutex
	exchanges []map[string]any // normalized fixture exchanges
	values    map[string]string
	base      string
	next      int
}

func (h *replayHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	check(err)
	h.mu.Lock()
	if h.next >= len(h.exchanges) {
		h.mu.Unlock()
		log.Fatalf("replay: unexpected request %s %s", r.Method, r.URL.RequestURI())
	}
	ex := h.exchanges[h.next]
	h.next++
	actual := toValue(wireRequest{
		Method:  r.Method,
		Path:    r.URL.RequestURI(),
		Headers: joinHeaders(r.Header, droppedRequestHeaders),
		Body:    parseBody(body),
	})
	align(ex["request"], actual, h.values)
	want := denormalize(ex["request"], h.values, map[string]string{"<base>": h.base}).(map[string]any)
	resp := denormalize(ex["response"], h.values, map[string]string{"<base>": h.base}).(map[string]any)
	h.mu.Unlock()
	if want["method"] != r.Method || want["path"] != r.URL.RequestURI() {
		log.Fatalf("replay: got %s %s, recording has %s %s", r.Method, r.URL.RequestURI(), want["method"], want["path"])
	}
	for k, v := range resp["headers"].(map[string]any) {
		w.Header().Set(k, v.(string))
	}
	w.WriteHeader(int(resp["status"].(float64)))
	if sse, ok := resp["sse"].([]any); ok {
		for _, payload := range sse {
			b, err := json.Marshal(payload)
			check(err)
			fmt.Fprintf(w, "data: %s\n\n", b)
			w.(http.Flusher).Flush()
		}
		return
	}
	if body := resp["body"]; body != nil {
		if s, ok := body.(string); ok {
			_, err := io.WriteString(w, s)
			check(err)
			return
		}
		b, err := json.Marshal(body)
		check(err)
		_, err = w.Write(b)
		check(err)
	}
}

// replayClient runs the client scenario against a fake server that serves the
// fixture's responses (denormalized) and returns the normalized unit.
func replayClient(cs clientScenario, fixture map[string]any, iface string) any {
	h := &replayHandler{values: map[string]string{}}
	rec := &recorder{}
	ts := httptest.NewServer(rec.wrap(h))
	defer ts.Close()
	h.base = ts.URL
	for _, run := range fixture["runs"].([]any) {
		for _, ex := range run.(map[string]any)["exchanges"].([]any) {
			h.exchanges = append(h.exchanges, ex.(map[string]any))
		}
	}
	var card *a2a.AgentCard
	if !cs.cardProvider {
		card = &a2a.AgentCard{}
		check(json.Unmarshal(must(json.Marshal(fixture["card"])), card))
		card = denormalizeCard(card, ts.URL)
	}
	unit := runRemoteAgent(cs, card, ts.URL, rec)
	if h.next != len(h.exchanges) {
		log.Fatalf("replay (%s): %d of %d recorded responses served", iface, h.next, len(h.exchanges))
	}
	out := normalizeUnit(unit, map[string]string{ts.URL: "<base>"}).(map[string]any)
	if !cs.cardProvider {
		out["card"] = fixture["card"]
	}
	return out
}

func denormalizeCard(card *a2a.AgentCard, base string) *a2a.AgentCard {
	for _, i := range card.SupportedInterfaces {
		i.URL = strings.ReplaceAll(i.URL, "<base>", base)
	}
	return card
}

// runClientLive runs the client scenario against a live executor server
// whose card names only iface (or both, with the card provider).
func runClientLive(cs clientScenario, spec executorSpec, iface string) any {
	var s *a2aServer
	if cs.cardProvider {
		s = startExecutorServer(spec)
	} else {
		s = startExecutorServer(spec, iface)
	}
	defer s.close()
	unit := runRemoteAgent(cs, s.card, s.base, s.rec)
	out := normalizeUnit(unit, bases(s, nil)).(map[string]any)
	if !cs.cardProvider {
		out["card"] = normalizeUnit(s.card, bases(s, nil))
	}
	return out
}

// ---------------------------------------------------------------------------
// Scenarios.

type scenario struct {
	name   string
	server serverScenario
	// client units by name; "jsonrpc" and "rest" run against a static card
	// naming only that binding, "card_provider" resolves the served card.
	client      clientScenario
	clientUnits []string
}

func scenarios() []scenario {
	assistant := executorSpec{agent: newAssistant}
	weatherPerRun := executorSpec{agent: newWeather, mode: adka2a.OutputArtifactPerRun, streaming: agent.StreamingModeSSE}
	weatherPerEvent := executorSpec{agent: newWeather, mode: adka2a.OutputArtifactPerEvent, streaming: agent.StreamingModeSSE}
	approver := executorSpec{agent: newApprover}
	broken := executorSpec{agent: newBroken}
	pipeline := executorSpec{agent: newPipeline}
	childApprover := executorSpec{agent: newApprover, mode: adka2a.OutputArtifactPerEvent, streaming: agent.StreamingModeSSE}
	root := executorSpec{mode: adka2a.OutputArtifactPerRun, streaming: agent.StreamingModeSSE}

	both := []string{"jsonrpc", "rest"}
	weatherClient := clientScenario{runs: []clientRun{{mode: agent.StreamingModeSSE, message: userText("weather in Paris?")}}}

	return []scenario{
		{
			name: "text_reply",
			server: serverScenario{spec: assistant, steps: []step{
				func([]exchange) op { return send(userMessage("msg-1", text("hello"))) },
				func(p []exchange) op { return getTask(taskOf(p[0])) },
				func([]exchange) op { return stream(userMessage("msg-2", text("hello"))) },
				func(p []exchange) op { return send(inContext(userMessage("msg-3", text("hello again")), taskOf(p[0]))) },
			}},
			client: clientScenario{runs: []clientRun{
				{mode: agent.StreamingModeNone, message: userText("hello")},
				{mode: agent.StreamingModeSSE, message: userText("hello again")},
			}},
			clientUnits: both,
		},
		{
			name: "streaming_partials_per_run",
			server: serverScenario{spec: weatherPerRun, steps: []step{
				func([]exchange) op { return stream(userMessage("msg-1", text("weather in Paris?"))) },
				func([]exchange) op { return send(userMessage("msg-2", text("weather in Paris?"))) },
			}},
			client:      weatherClient,
			clientUnits: both,
		},
		{
			name: "streaming_partials_per_event",
			server: serverScenario{spec: weatherPerEvent, steps: []step{
				func([]exchange) op { return stream(userMessage("msg-1", text("weather in Paris?"))) },
				func([]exchange) op { return send(userMessage("msg-2", text("weather in Paris?"))) },
			}},
			client:      weatherClient,
			clientUnits: both,
		},
		{
			name: "input_required_resume",
			server: serverScenario{spec: approver, steps: []step{
				func([]exchange) op { return send(userMessage("msg-1", text("Perform important task!"))) },
				func(p []exchange) op {
					return send(forTask(userMessage("msg-2", text("Is it really necessary?")), taskOf(p[0])))
				},
				func(p []exchange) op {
					return send(forTask(userMessage("msg-3", text("LGTM"), map[string]any{
						"data": map[string]any{
							"id":       pendingCallID(taskOf(p[0])),
							"name":     "request_approval",
							"response": map[string]any{"status": "approved", "ticket_id": "ticket-1"},
						},
						"metadata": map[string]any{"adk_type": "function_response"},
					}), taskOf(p[0])))
				},
				func(p []exchange) op { return getTask(taskOf(p[0])) },
			}},
			client: clientScenario{runs: []clientRun{
				{mode: agent.StreamingModeNone, message: userText("Perform important task!")},
				{mode: agent.StreamingModeNone, message: approvalFor},
			}},
			clientUnits: both,
		},
		{
			name: "failed_task",
			server: serverScenario{spec: broken, steps: []step{
				func([]exchange) op { return send(userMessage("msg-1", text("hello"))) },
				func([]exchange) op { return stream(userMessage("msg-2", text("hello"))) },
			}},
			client: clientScenario{runs: []clientRun{
				{mode: agent.StreamingModeNone, message: userText("hello")},
				{mode: agent.StreamingModeSSE, message: userText("hello again")},
			}},
			clientUnits: both,
		},
		{
			name: "cancel_input_required_with_child",
			server: serverScenario{spec: root, child: &childApprover, steps: []step{
				func([]exchange) op { return send(userMessage("msg-1", text("Hello"))) },
				func(p []exchange) op { return cancelTask(taskOf(p[0])) },
				func(p []exchange) op { return getTask(taskOf(p[0])) },
			}, waitChild: func(i int) int {
				// Step 1 makes the child run one streaming send; step 2's
				// cleanup cancels the child task asynchronously.
				if i <= 1 {
					return 1
				}
				return 0
			}},
		},
		{
			name: "agent_card_workflow",
			server: serverScenario{spec: pipeline, steps: []step{
				func([]exchange) op { return op{kind: "card"} },
				func([]exchange) op { return send(userMessage("msg-1", text("go"))) },
			}},
			client:      clientScenario{cardProvider: true, runs: []clientRun{{mode: agent.StreamingModeNone, message: userText("go")}}},
			clientUnits: []string{"card_provider"},
		},
	}
}

// ---------------------------------------------------------------------------
// Main.

func write(name string, doc any) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	check(enc.Encode(doc))
	check(os.WriteFile(filepath.Join(outDir, name+".json"), buf.Bytes(), 0o644))
}

func mustEqual(what string, want, got any) {
	if reflect.DeepEqual(toValue(want), toValue(got)) {
		return
	}
	dir, err := os.MkdirTemp("", "gen-go-a2a-")
	check(err)
	w, _ := json.MarshalIndent(want, "", "  ")
	g, _ := json.MarshalIndent(got, "", "  ")
	check(os.WriteFile(filepath.Join(dir, "recording.json"), w, 0o644))
	check(os.WriteFile(filepath.Join(dir, "replay.json"), g, 0o644))
	log.Fatalf("%s: replay differs from the recording (see %s/{recording,replay}.json)", what, dir)
}

func main() {
	check(os.MkdirAll(outDir, 0o755))
	old, err := filepath.Glob(filepath.Join(outDir, "*.json"))
	check(err)
	for _, f := range old {
		if strings.HasSuffix(f, ".deltas.json") {
			continue
		}
		check(os.Remove(f))
	}

	units := 0
	for _, sc := range scenarios() {
		doc := map[string]any{"scenario": sc.name}
		server := map[string]any{}
		for _, b := range []binding{bindingJSONRPC, bindingREST} {
			unit := runServerLive(sc.server, b)
			mustEqual(sc.name+" server "+string(b), unit, replayServer(sc.server, unit.(map[string]any)))
			server[string(b)] = unit
			units++
		}
		doc["server"] = server
		if len(sc.clientUnits) > 0 {
			client := map[string]any{}
			for _, iface := range sc.clientUnits {
				unit := runClientLive(sc.client, sc.server.spec, iface)
				mustEqual(sc.name+" client "+iface, unit, replayClient(sc.client, unit.(map[string]any), iface))
				client[iface] = unit
				units++
			}
			doc["client"] = client
		}
		write(sc.name, doc)
	}
	fmt.Printf("wrote %d fixtures (%d units) to %s\n", len(scenarios()), units, outDir)
}

func check(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
