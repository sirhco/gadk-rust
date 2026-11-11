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

// Command gen-go-launcher runs ADK-Go's launcher (cmd/launcher/full and
// cmd/launcher/prod) as a subprocess around a scripted agent and records web,
// console and command-line fixtures for gadk-launcher's replay (SCENARIO.md).
//
// The same binary is the probe and the launched program: with
// GEN_GO_LAUNCHER_CHILD set it is the child (childMain), otherwise the probe
// (probeMain), which re-executes itself.
//
// Run from this directory: go run .
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
	"google.golang.org/adk/v2/cmd/launcher"
	"google.golang.org/adk/v2/cmd/launcher/full"
	"google.golang.org/adk/v2/cmd/launcher/prod"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
)

const (
	testdata   = "../../crates/gadk-launcher/testdata"
	childEnv   = "GEN_GO_LAUNCHER_CHILD"
	errFileEnv = "GEN_GO_LAUNCHER_ERROR_FILE"

	// portPlaceholder stands for the live port in args, request headers,
	// response headers and logs.
	portPlaceholder = "<port>"

	// inlineBodyLimit is the largest response body stored as text.
	inlineBodyLimit = 8192

	// quietPeriod is how long the child's stderr must stay unchanged before
	// the probe takes the lines written so far.
	quietPeriod = 150 * time.Millisecond
)

// t0 is the first value the fixed clock returns; every read advances it 1 s.
var t0 = time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)

// ---------------------------------------------------------------------------
// Child: the launched program.

// fixedPlatform is the child's clock and UUID sequence.
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

// scriptedModel answers from a fixed function of the request's last content.
type scriptedModel struct{}

func (scriptedModel) Name() string { return "scripted-assistant" }

func final(parts ...*genai.Part) *model.LLMResponse {
	return &model.LLMResponse{
		Content: &genai.Content{Role: genai.RoleModel, Parts: parts},
		UsageMetadata: &genai.GenerateContentResponseUsageMetadata{
			PromptTokenCount:     10,
			CandidatesTokenCount: 5,
			TotalTokenCount:      15,
		},
		FinishReason: genai.FinishReasonStop,
	}
}

func chunk(text string) *model.LLMResponse {
	return &model.LLMResponse{
		Content: &genai.Content{Role: genai.RoleModel, Parts: []*genai.Part{{Text: text}}},
		Partial: true,
	}
}

// textReply is a final text answer, preceded in streaming mode by the chunks.
func textReply(stream bool, chunks ...string) []*model.LLMResponse {
	out := []*model.LLMResponse{}
	if stream {
		for _, c := range chunks {
			out = append(out, chunk(c))
		}
	}
	return append(out, final(&genai.Part{Text: strings.Join(chunks, "")}))
}

func reply(last *genai.Content, stream bool) ([]*model.LLMResponse, error) {
	if last != nil {
		for _, p := range last.Parts {
			if fr := p.FunctionResponse; fr != nil {
				if _, failed := fr.Response["error"]; failed {
					return textReply(stream, "Transfer ", "cancelled."), nil
				}
				return textReply(stream, "Transfer ", "done."), nil
			}
		}
	}
	text := ""
	if last != nil {
		for _, p := range last.Parts {
			if p.Text != "" {
				text = p.Text
				break
			}
		}
	}
	switch text {
	case "hello":
		return textReply(stream, "Hello! ", "How can I help?"), nil
	case "revise":
		if stream {
			return []*model.LLMResponse{chunk("Draft "), chunk("answer."), final(&genai.Part{Text: "Final answer."})}, nil
		}
		return []*model.LLMResponse{final(&genai.Part{Text: "Final answer."})}, nil
	case "fail":
		return nil, errors.New("scripted model failure")
	case "transfer":
		return []*model.LLMResponse{final(&genai.Part{FunctionCall: &genai.FunctionCall{
			Name: "transfer_funds",
			Args: map[string]any{"amount": 100},
		}})}, nil
	default:
		return textReply(stream, "I can only ", "say hello."), nil
	}
}

func (scriptedModel) GenerateContent(_ context.Context, req *model.LLMRequest, stream bool) iter.Seq2[*model.LLMResponse, error] {
	return func(yield func(*model.LLMResponse, error) bool) {
		var last *genai.Content
		if n := len(req.Contents); n > 0 {
			last = req.Contents[n-1]
		}
		responses, err := reply(last, stream)
		if err != nil {
			yield(nil, err)
			return
		}
		for _, r := range responses {
			if !yield(r, nil) {
				return
			}
		}
	}
}

type transferArgs struct {
	Amount int `json:"amount"`
}

type transferResult struct {
	Status string `json:"status"`
}

func newAgent() (agent.Agent, error) {
	transfer, err := functiontool.New(functiontool.Config{
		Name:                "transfer_funds",
		Description:         "Transfers funds.",
		RequireConfirmation: true,
	}, func(_ agent.Context, args transferArgs) (transferResult, error) {
		return transferResult{Status: fmt.Sprintf("transferred %d", args.Amount)}, nil
	})
	if err != nil {
		return nil, err
	}
	return llmagent.New(llmagent.Config{
		Name:        "assistant",
		Description: "Answers greetings and transfers funds.",
		Model:       scriptedModel{},
		Instruction: "Answer briefly. Use transfer_funds to transfer funds.",
		Tools:       []tool.Tool{transfer},
	})
}

// childMain is ADK-Go's example main (examples/quickstart) with the scripted
// agent, the fixed providers on the context, SIGTERM cancelling the context,
// and the returned error also written to $GEN_GO_LAUNCHER_ERROR_FILE.
func childMain() {
	args := os.Args[1:]
	if len(args) == 0 {
		log.Fatal("child: missing preset")
	}
	preset, args := args[0], args[1:]

	fp := &fixedPlatform{}
	ctx := platform.WithTimeProvider(context.Background(), fp.now)
	ctx = platform.WithUUIDProvider(ctx, fp.uuid)
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGTERM)
	defer stop()

	a, err := newAgent()
	if err != nil {
		log.Fatalf("Failed to create agent: %v", err)
	}
	config := &launcher.Config{
		AgentLoader: agent.NewSingleLoader(a),
	}

	var l launcher.Launcher
	switch preset {
	case "full":
		l = full.NewLauncher()
	case "prod":
		l = prod.NewLauncher()
	default:
		log.Fatalf("child: unknown preset %q", preset)
	}
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
// Fixtures.

// procFixture is one console or CLI case: a child run to completion.
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

type wireRequest struct {
	Method  string      `json:"method"`
	Target  string      `json:"target"`
	Headers [][2]string `json:"headers"`
	Body    string      `json:"body"`
}

type wireResponse struct {
	Status     int         `json:"status"`
	StatusText string      `json:"status_text"`
	Headers    [][2]string `json:"headers"`
	Body       *string     `json:"body"`
	BodyLength *int        `json:"body_length"`
	BodySHA256 *string     `json:"body_sha256"`
	BaseHref   *string     `json:"base_href"`
}

// webFixture is one step of a web server's script: its startup, one HTTP
// exchange, or its shutdown.
type webFixture struct {
	Name       string        `json:"name"`
	Server     string        `json:"server"`
	Launcher   string        `json:"launcher"`
	Args       []string      `json:"args"`
	Kind       string        `json:"kind"`
	Request    *wireRequest  `json:"request"`
	Response   *wireResponse `json:"response"`
	Log        []string      `json:"log"`
	ExitStatus *int          `json:"exit_status"`
	Stdout     *string       `json:"stdout"`
	Error      *string       `json:"error"`
}

func encode(v any) []byte {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	check(enc.Encode(v))
	return buf.Bytes()
}

// ---------------------------------------------------------------------------
// Normalization.

var (
	logTimestamp = regexp.MustCompile(`(?m)^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} `)
	// requestLogLine is the web launcher's request logger line, "%s %s %s"
	// (method, RequestURI, elapsed time.Duration), after logTimestamp.
	requestLogLine = regexp.MustCompile(`^(<ts> [A-Z]+ \S+ )(?:\d+(?:\.\d+)?(?:ns|µs|ms|s|m|h))+$`)
	userMessage    = regexp.MustCompile(`^<ts> +([a-z0-9]+):`)
	uuidPattern    = regexp.MustCompile(`[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`)
	timeField      = regexp.MustCompile(`"(lastUpdateTime|timestamp)":(-?[0-9][0-9.eE+-]*|"[^"]*")`)
	baseHref       = regexp.MustCompile(`(?is)<base\s[^>]*>`)
)

// webSublauncherOrder is each preset's web sub-launcher declaration order.
var webSublauncherOrder = map[string][]string{
	"full": {"webui", "a2a", "pubsub", "eventarc", "api"},
	"prod": {"api", "a2a"},
}

func portPattern(port int) *regexp.Regexp {
	return regexp.MustCompile(`(127\.0\.0\.1|localhost|port):` + strconv.Itoa(port) + `\b`)
}

// normText applies N1 (log timestamps) and, with a port, N2.
func normText(s string, port int) string {
	s = logTimestamp.ReplaceAllString(s, "<ts> ")
	if port > 0 {
		s = portPattern(port).ReplaceAllString(s, "${1}:"+portPlaceholder)
	}
	return s
}

// normLog splits normalized stderr into lines (N1–N3).
func normLog(s string, port int) []string {
	s = normText(s, port)
	lines := strings.Split(s, "\n")
	if lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	for i, l := range lines {
		lines[i] = requestLogLine.ReplaceAllString(l, "${1}<elapsed>")
	}
	return lines
}

// sortUserMessages applies N4 to a web server's startup lines.
func sortUserMessages(lines []string, preset string) []string {
	start := -1
	for i, l := range lines {
		if strings.HasPrefix(l, "<ts> Web servers starts on ") {
			start = i + 1
			break
		}
	}
	if start < 0 {
		return lines
	}
	end := start
	for end < len(lines) && lines[end] != "<ts> " {
		end++
	}
	rank := func(l string) int {
		m := userMessage.FindStringSubmatch(l)
		if m == nil {
			log.Fatalf("unexpected startup line %q", l)
		}
		for i, k := range webSublauncherOrder[preset] {
			if k == m[1] {
				return i
			}
		}
		log.Fatalf("unknown sub-launcher in %q", l)
		return 0
	}
	block := lines[start:end]
	sort.SliceStable(block, func(i, j int) bool { return rank(block[i]) < rank(block[j]) })
	return lines
}

// hasWallClockTime reports whether a body holds an N5 time field with a
// fractional-seconds part. Such a value is a wall-clock time: a2a-go stamps
// task status with time.Now() (not the child's fixed clock, which is whole
// seconds), and RFC 3339 drops trailing zero digits, so the body's raw length
// varies run to run. Whole-second values (the fixed clock, adkrest's integer
// Unix-seconds lastUpdateTime) have a fixed width.
func hasWallClockTime(s string) bool {
	for _, m := range timeField.FindAllStringSubmatch(s, -1) {
		if strings.Contains(m[2], ".") {
			return true
		}
	}
	return false
}

// normBody applies N2, N5 and N6 to an inline body.
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

// ---------------------------------------------------------------------------
// Child processes.

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

var self string

func childCommand(preset string, args []string, errFile string) *exec.Cmd {
	cmd := exec.Command(self, append([]string{preset}, args...)...)
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
	f, err := os.CreateTemp("", "gen-go-launcher-err-*")
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

// runProc runs a console or CLI case and normalizes it (N1).
func runProc(name, preset string, args []string, stdin string) procFixture {
	errFile := errFilePath()
	cmd := childCommand(preset, args, errFile)
	cmd.Stdin = strings.NewReader(stdin)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	status := 0
	if err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			log.Fatalf("%s: %v", name, err)
		}
		status = exit.ExitCode()
	}
	return procFixture{
		Name:       name,
		Launcher:   preset,
		Args:       args,
		Stdin:      stdin,
		Stdout:     stdout.String(),
		Stderr:     normText(stderr.String(), 0),
		ExitStatus: status,
		Error:      readError(errFile),
	}
}

// ---------------------------------------------------------------------------
// Web servers.

type webServer struct {
	name    string
	preset  string
	args    []string // with portPlaceholder
	port    int
	cmd     *exec.Cmd
	errFile string
	stdout  syncBuffer
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

func startServer(name, preset string, args []string) *webServer {
	s := &webServer{name: name, preset: preset, args: args, port: freePort(), errFile: errFilePath(), done: make(chan error, 1)}
	s.cmd = childCommand(preset, substitutePort(args, s.port), s.errFile)
	s.cmd.Stdout = &s.stdout
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
			log.Fatalf("%s: server exited before listening: %v\nstderr:\n%s", name, err, s.stderr.String())
		default:
		}
		if time.Now().After(deadline) {
			s.kill()
			log.Fatalf("%s: server did not listen\nstderr:\n%s", name, s.stderr.String())
		}
		time.Sleep(20 * time.Millisecond)
	}
	return s
}

func (s *webServer) kill() {
	_ = s.cmd.Process.Kill()
}

// newLog waits until stderr has been quiet for quietPeriod and returns the
// normalized lines written since the previous call.
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

// exchange sends one request on a fresh connection and reads the response
// to EOF (every request carries Connection: close).
func (s *webServer) exchange(req wireRequest) *wireResponse {
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
		log.Fatalf("no header block in %q", raw)
	}
	lines := strings.Split(string(head), "\r\n")
	statusText := ""
	if parts := strings.SplitN(lines[0], " ", 3); len(parts) == 3 {
		statusText = parts[2]
	}
	w := &wireResponse{Status: resp.StatusCode, StatusText: statusText}

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
			log.Fatalf("malformed header line %q", line)
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
	sort.SliceStable(headers, func(i, j int) bool {
		if headers[i][0] != headers[j][0] {
			return headers[i][0] < headers[j][0]
		}
		return headers[i][1] < headers[j][1]
	})
	w.Headers = headers
	return w
}

// shutdown sends SIGTERM (the child cancels its context) and waits.
func (s *webServer) shutdown() (int, string, *string) {
	check(s.cmd.Process.Signal(syscall.SIGTERM))
	var err error
	select {
	case err = <-s.done:
	case <-time.After(60 * time.Second):
		s.kill()
		log.Fatalf("%s: server did not exit", s.name)
	}
	status := 0
	if err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			log.Fatalf("%s: %v", s.name, err)
		}
		status = exit.ExitCode()
	}
	return status, s.stdout.String(), readError(s.errFile)
}

// ---------------------------------------------------------------------------
// Scripts.

// step is one request of a web server script. The probe adds Host first (unless
// the step's first header is Host) and
// Content-Length (when the body is non-empty or the method is POST, PUT or
// PATCH) and Connection: close last.
type step struct {
	name    string
	method  string
	target  string
	headers [][2]string
	body    string
}

func (s step) request() wireRequest {
	headers := [][2]string{}
	if len(s.headers) == 0 || s.headers[0][0] != "Host" {
		headers = append(headers, [2]string{"Host", "127.0.0.1:" + portPlaceholder})
	}
	headers = append(headers, s.headers...)
	if s.body != "" || s.method == "POST" || s.method == "PUT" || s.method == "PATCH" {
		headers = append(headers, [2]string{"Content-Length", strconv.Itoa(len(s.body))})
	}
	headers = append(headers, [2]string{"Connection", "close"})
	return wireRequest{Method: s.method, Target: s.target, Headers: headers, Body: s.body}
}

type serverScript struct {
	name   string
	preset string
	args   []string
	steps  []step
}

const (
	jsonCT       = "application/json"
	webuiOrigin  = "http://localhost:8080"
	extraOrigin  = "http://allowed.example"
	evilOrigin   = "http://evil.example"
	maxBody      = 1024
	pubsubBody   = `{"message":{"data":"aGVsbG8=","messageId":"m1","attributes":{"k":"v"}},"subscription":"projects/p/subscriptions/s"}`
	eventarcBody = `{"specversion":"1.0","type":"google.cloud.storage.object.v1.finalized","source":"//storage.googleapis.com/projects/_/buckets/b","id":"e1","data":{"name":"file.txt"}}`
	a2aV1Body    = `{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"msg-1","role":"ROLE_USER","parts":[{"text":"hello"}]}}}`
	a2aV0Body    = `{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"kind":"message","messageId":"msg-2","role":"user","parts":[{"kind":"text","text":"hello"}]}}}`
	oidcAudience = "https://example.com/push"
	oidcAccount  = "push@example.iam.gserviceaccount.com"
)

func h(kv ...string) [][2]string {
	out := [][2]string{}
	for i := 0; i+1 < len(kv); i += 2 {
		out = append(out, [2]string{kv[i], kv[i+1]})
	}
	return out
}

func serverScripts() []serverScript {
	oversized := fmt.Sprintf(`{"state":{"padding":%q}}`, strings.Repeat("a", 2*maxBody))
	oversizedPubsub := fmt.Sprintf(`{"message":{"data":%q},"subscription":"s"}`, strings.Repeat("a", 2*maxBody))
	return []serverScript{
		{
			name:   "full",
			preset: "full",
			args: []string{"web", "-port", portPlaceholder, "-max_request_body_size", strconv.Itoa(maxBody),
				"-allow_origins", extraOrigin, "api", "webui", "a2a", "pubsub", "eventarc"},
			steps: []step{
				{name: "health_get", method: "GET", target: "/health"},
				{name: "health_head", method: "HEAD", target: "/health"},
				{name: "health_wrong_method", method: "DELETE", target: "/health"},
				{name: "health_trailing_slash", method: "GET", target: "/health/"},
				{name: "non_clean_path", method: "GET", target: "/api/../health?x=1"},
				{name: "non_clean_path_post", method: "POST", target: "//api/list-apps"},
				{name: "unmatched_path", method: "GET", target: "/no/such/path"},
				{name: "unmatched_path_cross_origin", method: "GET", target: "/no/such/path", headers: h("Origin", evilOrigin)},
				{name: "origin_forbidden", method: "GET", target: "/api/list-apps", headers: h("Origin", evilOrigin)},
				{name: "origin_allowed_flag", method: "GET", target: "/api/list-apps", headers: h("Origin", extraOrigin)},
				{name: "origin_allowed_webui", method: "GET", target: "/api/list-apps", headers: h("Origin", webuiOrigin)},
				{name: "host_forbidden", method: "GET", target: "/health", headers: h("Host", "evil.example")},
				{name: "cors_preflight", method: "OPTIONS", target: "/api/apps/assistant/users/u1/sessions",
					headers: h("Origin", webuiOrigin, "Access-Control-Request-Method", "POST", "Access-Control-Request-Headers", "content-type")},
				{name: "cors_preflight_flag_origin", method: "OPTIONS", target: "/api/list-apps",
					headers: h("Origin", extraOrigin, "Access-Control-Request-Method", "GET")},
				{name: "api_list_apps", method: "GET", target: "/api/list-apps"},
				{name: "api_health", method: "GET", target: "/api/health"},
				{name: "api_bare_prefix", method: "GET", target: "/api"},
				{name: "api_prefix_word", method: "GET", target: "/apifoo"},
				{name: "api_session_create", method: "POST", target: "/api/apps/assistant/users/u1/sessions/s1",
					headers: h("Content-Type", jsonCT), body: `{"state":{"color":"blue"}}`},
				{name: "api_session_create_generated_id", method: "POST", target: "/api/apps/assistant/users/u1/sessions"},
				{name: "api_session_get_trailing_slash", method: "GET", target: "/api/apps/assistant/users/u1/sessions/s1/"},
				{name: "api_session_post_trailing_slash", method: "POST", target: "/api/apps/assistant/users/u1/sessions/s2/",
					headers: h("Content-Type", jsonCT), body: `{}`},
				{name: "api_redirect_get", method: "GET", target: "/api/health%2F?q=1"},
				{name: "api_redirect_head", method: "HEAD", target: "/api/health%2F"},
				{name: "api_redirect_post", method: "POST", target: "/api/health%2F"},
				{name: "api_wrong_method", method: "PUT", target: "/api/list-apps"},
				{name: "api_body_too_large", method: "POST", target: "/api/apps/assistant/users/u1/sessions",
					headers: h("Content-Type", jsonCT), body: oversized},
				{name: "a2a_card", method: "GET", target: "/.well-known/agent-card.json"},
				{name: "a2a_v1_send", method: "POST", target: "/a2a/v1/invoke",
					headers: h("A2A-Version", "1.0", "Content-Type", jsonCT), body: a2aV1Body},
				{name: "a2a_v1_send_without_version", method: "POST", target: "/a2a/v1/invoke",
					headers: h("Content-Type", jsonCT), body: a2aV1Body},
				{name: "a2a_v03_send", method: "POST", target: "/a2a/invoke",
					headers: h("Content-Type", jsonCT), body: a2aV0Body},
				{name: "webui_root_redirect", method: "GET", target: "/"},
				{name: "webui_bare_redirect", method: "GET", target: "/ui"},
				{name: "webui_index", method: "GET", target: "/ui/"},
				{name: "webui_index_head", method: "HEAD", target: "/ui/"},
				{name: "webui_index_file", method: "GET", target: "/ui/index.html"},
				{name: "webui_asset_svg", method: "GET", target: "/ui/adk_favicon.svg"},
				{name: "webui_asset_js", method: "GET", target: "/ui/polyfills-5CFQRCPP.js"},
				{name: "webui_asset_css", method: "GET", target: "/ui/styles-LBC36Z6S.css"},
				{name: "webui_asset_if_modified_since", method: "GET", target: "/ui/adk_favicon.svg",
					headers: h("If-Modified-Since", "Sat, 01 Jan 2000 00:00:00 GMT")},
				{name: "webui_missing_js", method: "GET", target: "/ui/missing.js"},
				{name: "webui_deep_link", method: "GET", target: "/ui/apps/assistant/sessions"},
				{name: "webui_dir_no_listing", method: "GET", target: "/ui/assets/"},
				{name: "webui_runtime_config", method: "GET", target: "/ui/assets/config/runtime-config.json"},
				{name: "webui_post", method: "POST", target: "/ui/"},
				{name: "pubsub_trigger", method: "POST", target: "/api/apps/assistant/trigger/pubsub",
					headers: h("Content-Type", jsonCT), body: pubsubBody},
				{name: "pubsub_trigger_bad_json", method: "POST", target: "/api/apps/assistant/trigger/pubsub",
					headers: h("Content-Type", jsonCT), body: `{`},
				{name: "pubsub_trigger_body_too_large", method: "POST", target: "/api/apps/assistant/trigger/pubsub",
					headers: h("Content-Type", jsonCT), body: oversizedPubsub},
				{name: "pubsub_trigger_get", method: "GET", target: "/api/apps/assistant/trigger/pubsub"},
				{name: "eventarc_trigger_structured", method: "POST", target: "/api/apps/assistant/trigger/eventarc",
					headers: h("Content-Type", "application/cloudevents+json"), body: eventarcBody},
				{name: "eventarc_trigger_binary", method: "POST", target: "/api/apps/assistant/trigger/eventarc",
					headers: h("Content-Type", jsonCT, "Ce-Id", "e2", "Ce-Source", "//pubsub.googleapis.com/projects/p/topics/t",
						"Ce-Specversion", "1.0", "Ce-Type", "google.cloud.pubsub.topic.v1.messagePublished"),
					body: pubsubBody},
			},
		},
		{
			name:   "oidc",
			preset: "full",
			args: []string{"web", "-port", portPlaceholder, "pubsub", "-oidc_audience", oidcAudience,
				"-oidc_service_accounts", oidcAccount, "eventarc"},
			steps: []step{
				{name: "pubsub_no_token", method: "POST", target: "/api/apps/assistant/trigger/pubsub",
					headers: h("Content-Type", jsonCT), body: pubsubBody},
				{name: "pubsub_malformed_token", method: "POST", target: "/api/apps/assistant/trigger/pubsub",
					headers: h("Authorization", "Bearer not-a-jwt", "Content-Type", jsonCT), body: pubsubBody},
				{name: "eventarc_unauthenticated", method: "POST", target: "/api/apps/assistant/trigger/eventarc",
					headers: h("Content-Type", "application/cloudevents+json"), body: eventarcBody},
			},
		},
		{
			name:   "noprefix",
			preset: "full",
			args:   []string{"web", "-port", portPlaceholder, "api", "-path_prefix="},
			steps: []step{
				{name: "health", method: "GET", target: "/health"},
				{name: "list_apps", method: "GET", target: "/list-apps"},
				{name: "list_apps_trailing_slash", method: "GET", target: "/list-apps/"},
				{name: "unknown_path", method: "GET", target: "/no/such/path"},
				{name: "root", method: "GET", target: "/"},
			},
		},
		{
			name:   "prod",
			preset: "prod",
			args:   []string{"web", "-port", portPlaceholder, "a2a", "api"},
			steps: []step{
				{name: "health", method: "GET", target: "/health"},
				{name: "api_list_apps", method: "GET", target: "/api/list-apps"},
				{name: "webui_absent", method: "GET", target: "/ui/"},
			},
		},
	}
}

type procCase struct {
	name   string
	preset string
	args   []string
	stdin  string
}

func consoleCases() []procCase {
	none := []string{"console", "-streaming_mode", "none"}
	sse := []string{"console", "-streaming_mode", "sse"}
	return []procCase{
		{"none_hello", "full", none, "hello\n"},
		{"none_two_turns", "full", none, "hello\nsomething else\n"},
		{"sse_hello", "full", sse, "hello\n"},
		{"sse_two_turns", "full", sse, "hello\nsomething else\n"},
		{"none_revised_final", "full", none, "revise\n"},
		{"sse_revised_final", "full", sse, "revise\n"},
		{"default_mode_no_args", "full", []string{}, "hello\n"},
		{"crlf_input", "full", none, "hello\r\n"},
		{"agent_error", "full", none, "fail\nhello\n"},
		{"sse_agent_error", "full", sse, "fail\n"},
		{"hitl_accept", "full", none, "transfer\nyes\n"},
		{"hitl_reject", "full", none, "transfer\nno\n"},
		{"sse_hitl_accept", "full", sse, "transfer\ny\n"},
		{"eof_empty", "full", none, ""},
		{"eof_partial_line", "full", none, "hello"},
	}
}

func cliCases() []procCase {
	return []procCase{
		{"help_top", "full", []string{"-h"}, ""},
		{"help_console", "full", []string{"console", "-help"}, ""},
		{"help_web", "full", []string{"web", "-h"}, ""},
		{"help_web_api", "full", []string{"web", "api", "-h"}, ""},
		{"help_web_webui", "full", []string{"web", "webui", "-h"}, ""},
		{"help_web_a2a", "full", []string{"web", "a2a", "-h"}, ""},
		{"help_web_pubsub", "full", []string{"web", "pubsub", "-h"}, ""},
		{"help_web_eventarc", "full", []string{"web", "eventarc", "-h"}, ""},
		{"help_prod", "prod", []string{"-h"}, ""},
		{"unknown_flag", "full", []string{"web", "-nope"}, ""},
		{"unknown_flag_console", "full", []string{"-nope"}, ""},
		{"flag_needs_argument", "full", []string{"web", "-port"}, ""},
		{"bad_flag_syntax", "full", []string{"web", "---port", "1"}, ""},
		{"bad_int", "full", []string{"web", "-port", "abc"}, ""},
		{"bad_int_out_of_range", "full", []string{"web", "-port", "99999999999999999999"}, ""},
		{"bad_int64", "full", []string{"web", "-max_request_body_size", "1k"}, ""},
		{"bad_duration", "full", []string{"web", "-read-timeout", "5x"}, ""},
		{"bad_bool", "full", []string{"web", "-h2c=maybe"}, ""},
		{"bad_streaming_mode", "full", []string{"console", "-streaming_mode", "bidi"}, ""},
		{"web_no_sublaunchers", "full", []string{"web"}, ""},
		{"duplicate_keyword", "full", []string{"web", "api", "api"}, ""},
		{"unparsed_args_web", "full", []string{"web", "api", "extra", "args"}, ""},
		{"unparsed_args_console", "full", []string{"console", "extra"}, ""},
		{"unparsed_args_prod", "prod", []string{"console"}, ""},
		{"api_bad_flag", "full", []string{"web", "api", "-trace_capacity", "x"}, ""},
		{"pubsub_max_retries_zero", "full", []string{"web", "pubsub", "-trigger_max_retries", "0"}, ""},
		{"pubsub_negative_base_delay", "full", []string{"web", "pubsub", "-trigger_base_delay", "-1s"}, ""},
		{"eventarc_max_delay_zero", "full", []string{"web", "eventarc", "-trigger_max_delay", "0s"}, ""},
		{"eventarc_max_concurrent_runs_zero", "full", []string{"web", "eventarc", "-trigger_max_concurrent_runs", "0"}, ""},
		{"oidc_audience_empty", "full", []string{"web", "eventarc", "-oidc_audience="}, ""},
		{"oidc_audience_without_accounts", "full", []string{"web", "pubsub", "-oidc_audience", oidcAudience}, ""},
		{"oidc_accounts_without_audience", "full", []string{"web", "pubsub", "-oidc_service_accounts", oidcAccount}, ""},
		{"oidc_audience_dash", "full", []string{"web", "pubsub", "-oidc_audience", "-oidc_service_accounts", oidcAccount}, ""},
		{"oidc_accounts_empty_entry", "full", []string{"web", "pubsub", "-oidc_audience", oidcAudience, "-oidc_service_accounts", oidcAccount + ","}, ""},
	}
}

// ---------------------------------------------------------------------------
// Generation and replay.

// fileSet maps a path relative to testdata to the fixture bytes.
type fileSet map[string][]byte

func recordProc(dir string, c procCase, files fileSet) {
	files[filepath.Join(dir, c.name+".json")] = encode(runProc(c.name, c.preset, c.args, c.stdin))
}

// runServer runs one server's script: startup, the requests, shutdown.
func runServer(sc serverScript, requests []wireRequest, names []string, files fileSet) {
	s := startServer(sc.name, sc.preset, sc.args)
	defer s.kill()
	fixture := func(name, kind string) webFixture {
		return webFixture{Name: name, Server: sc.name, Launcher: sc.preset, Args: sc.args, Kind: kind}
	}
	put := func(f webFixture) {
		files[filepath.Join("wire", f.Name+".json")] = encode(f)
	}

	start := fixture(sc.name+"_00_startup", "startup")
	start.Log = sortUserMessages(s.newLog(), sc.preset)
	put(start)

	for i, req := range requests {
		f := fixture(fmt.Sprintf("%s_%02d_%s", sc.name, i+1, names[i]), "exchange")
		r := req
		f.Request = &r
		f.Response = s.exchange(req)
		f.Log = s.newLog()
		put(f)
	}

	status, stdout, errText := s.shutdown()
	stop := fixture(sc.name+"_99_shutdown", "shutdown")
	stop.Log = s.newLog()
	stop.ExitStatus = &status
	stop.Stdout = &stdout
	stop.Error = errText
	put(stop)
}

func generate() fileSet {
	files := fileSet{}
	for _, sc := range serverScripts() {
		var reqs []wireRequest
		var names []string
		for _, st := range sc.steps {
			reqs = append(reqs, st.request())
			names = append(names, st.name)
		}
		runServer(sc, reqs, names, files)
	}
	for _, c := range consoleCases() {
		recordProc("console", c, files)
	}
	for _, c := range cliCases() {
		recordProc("cli", c, files)
	}
	return files
}

// replay re-runs every fixture from its own content alone, as gadk-launcher's
// replay does: web fixtures grouped by server and run in name order, process
// fixtures from their launcher, args and stdin.
func replay(fixtures fileSet) fileSet {
	files := fileSet{}
	paths := make([]string, 0, len(fixtures))
	for p := range fixtures {
		paths = append(paths, p)
	}
	sort.Strings(paths)

	type server struct {
		script serverScript
		reqs   []wireRequest
		names  []string
	}
	servers := map[string]*server{}
	var order []string
	for _, p := range paths {
		switch filepath.Dir(p) {
		case "wire":
			var f webFixture
			check(json.Unmarshal(fixtures[p], &f))
			sv, ok := servers[f.Server]
			if !ok {
				sv = &server{script: serverScript{name: f.Server, preset: f.Launcher, args: f.Args}}
				servers[f.Server] = sv
				order = append(order, f.Server)
			}
			if f.Kind == "exchange" {
				sv.reqs = append(sv.reqs, *f.Request)
				// "<server>_<NN>_<case>"
				sv.names = append(sv.names, strings.TrimPrefix(f.Name, f.Server+"_")[3:])
			}
		default:
			var f procFixture
			check(json.Unmarshal(fixtures[p], &f))
			recordProc(filepath.Dir(p), procCase{name: f.Name, preset: f.Launcher, args: f.Args, stdin: f.Stdin}, files)
		}
	}
	for _, name := range order {
		sv := servers[name]
		runServer(sv.script, sv.reqs, sv.names, files)
	}
	return files
}

func probeMain() {
	var err error
	self, err = os.Executable()
	check(err)
	for _, v := range []string{"OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN"} {
		check(os.Unsetenv(v))
	}

	recorded := generate()
	replayed := replay(recorded)
	var mismatches []string
	for p, want := range recorded {
		if got, ok := replayed[p]; !ok || !bytes.Equal(got, want) {
			mismatches = append(mismatches, p)
		}
	}
	for p := range replayed {
		if _, ok := recorded[p]; !ok {
			mismatches = append(mismatches, p)
		}
	}
	if len(mismatches) > 0 {
		sort.Strings(mismatches)
		for _, p := range mismatches {
			fmt.Fprintf(os.Stderr, "replay mismatch: %s\n--- recorded\n%s\n--- replayed\n%s\n", p, recorded[p], replayed[p])
		}
		os.Exit(1)
	}

	counts := map[string]int{}
	for _, dir := range []string{"wire", "console", "cli"} {
		full := filepath.Join(testdata, dir)
		check(os.MkdirAll(full, 0o755))
		old, err := filepath.Glob(filepath.Join(full, "*.json"))
		check(err)
		for _, f := range old {
			if !strings.HasSuffix(f, ".deltas.json") {
				check(os.Remove(f))
			}
		}
	}
	for p, data := range recorded {
		check(os.WriteFile(filepath.Join(testdata, p), data, 0o644))
		counts[filepath.Dir(p)]++
	}
	fmt.Printf("wrote wire=%d console=%d cli=%d fixtures to %s\n", counts["wire"], counts["console"], counts["cli"], testdata)
}

func main() {
	if os.Getenv(childEnv) != "" {
		childMain()
		return
	}
	probeMain()
}

func check(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
