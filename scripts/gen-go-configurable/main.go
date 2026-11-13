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

// Command gen-go-configurable records how ADK-Go's conformance record and
// replay plugins (internal/configurable/conformance/{recordplugin,replayplugin})
// write and read recordings files, as fixtures for gadk-configurable
// (sub-project 6c-2, spec R8, R10, R15; SCENARIO.md): a recording corpus
// (input JSON, Go record bytes, Go replay decode), the reflection-derived
// type-path fixup table and jsonschema-go field list, and a
// canonicalizeJSONSubstrings table.
//
// Go forbids importing ADK-Go's internal/ packages, so the helpers are copied
// verbatim (vendor_*.go, recording/); -vendored prints them for the drift
// check in scripts/check-fixtures.sh.
//
// Run from this directory: go run .
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"sort"

	"github.com/google/jsonschema-go/jsonschema"
	"google.golang.org/genai"
	"gopkg.in/yaml.v3"

	"gadk/gen-go-configurable/recording"
	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/runner"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
)

func main() {
	fixtures := flag.String("fixtures", "../../crates/gadk-configurable/tests/fixtures",
		"gadk-configurable fixtures dir (deletes and rewrites recordings/, fixups.json, canonicalize.json)")
	vendored := flag.String("vendored", "",
		`"adk" or "probe": print the vendored declarations from ADK-Go ($ADK_GO_DIR, else ../../../adk-go) or from this probe, and exit`)
	flag.Parse()
	if *vendored != "" {
		root := os.Getenv("ADK_GO_DIR")
		if root == "" {
			root = "../../../adk-go"
		}
		if err := extractVendored(*vendored, root); err != nil {
			fmt.Fprintln(os.Stderr, "gen-go-configurable:", err)
			os.Exit(1)
		}
		return
	}
	if err := run(*fixtures); err != nil {
		fmt.Fprintln(os.Stderr, "gen-go-configurable:", err)
		os.Exit(1)
	}
}

func run(fixtures string) error {
	if err := os.Setenv("TZ", "UTC"); err != nil {
		return err
	}
	if err := newConformanceTools(); err != nil {
		return err
	}
	cases, err := allCases()
	if err != nil {
		return err
	}
	recDir := filepath.Join(fixtures, "recordings")
	if err := os.RemoveAll(recDir); err != nil {
		return err
	}
	results := map[string]caseResult{}
	replayErrors := 0
	for _, c := range cases {
		failed, err := writeCase(recDir, c, results)
		if err != nil {
			return fmt.Errorf("case %s: %w", c.name, err)
		}
		if failed {
			replayErrors++
		}
	}
	fx, err := writeFixups(filepath.Join(fixtures, "fixups.json"), cases, results)
	if err != nil {
		return err
	}
	nCanon, err := writeCanonicalize(filepath.Join(fixtures, "canonicalize.json"))
	if err != nil {
		return err
	}
	fmt.Printf("wrote recordings=%d (replay errors %d) fixups=%d jsonschema_fields=%d shapes=%d shape_paths=%d"+
		" (self-check %d/%d go numbers, %d/%d json numbers) canonicalize=%d to %s\n",
		len(cases), replayErrors, len(fx.Fixups), len(fx.JSONSchemaFields), len(fx.Shapes), len(fx.ShapePaths),
		fx.ShapeSelfCheck.GoNumbers.Pass, len(cases), fx.ShapeSelfCheck.JSONNumbers.Pass, len(cases), nCanon, fixtures)
	return nil
}

// caseInput is input.json.
type caseInput struct {
	Case             string          `json:"case"`
	StreamingMode    string          `json:"streaming_mode"`
	UserMessageIndex int             `json:"user_message_index"`
	Existing         *string         `json:"existing"`
	Recordings       json.RawMessage `json:"recordings"`
}

// jsonRecording is the JSON form of a recording.Recording (input.json and
// replay.json): genai JSON below the roots, LLMRequest as
// {"model", "contents", "config"}.
type jsonRecording struct {
	UserMessageIndex int                `json:"user_message_index"`
	AgentName        string             `json:"agent_name"`
	LLMRecording     *jsonLLMRecording  `json:"llm_recording,omitempty"`
	ToolRecording    *jsonToolRecording `json:"tool_recording,omitempty"`
}

type jsonLLMRecording struct {
	LLMRequest   *jsonLLMRequest      `json:"llm_request,omitempty"`
	LLMResponses []*model.LLMResponse `json:"llm_responses,omitempty"`
}

type jsonLLMRequest struct {
	Model    string                       `json:"model"`
	Contents []*genai.Content             `json:"contents,omitempty"`
	Config   *genai.GenerateContentConfig `json:"config,omitempty"`
}

type jsonToolRecording struct {
	ToolCall     *genai.FunctionCall     `json:"tool_call,omitempty"`
	ToolResponse *genai.FunctionResponse `json:"tool_response,omitempty"`
}

// replayOutput is replay.json.
type replayOutput struct {
	Error      *string         `json:"error"`
	Recordings []jsonRecording `json:"recordings"`
}

// probeCase is a corpus case ready to record: an authored recordingCase or a
// captured one (recordings JSON produced from a Go value).
type probeCase struct {
	recordingCase
	direct []recording.Recording // captured cases: the Go value the JSON came from
}

func allCases() ([]probeCase, error) {
	var out []probeCase
	for _, c := range authoredCases {
		if c.mode == "" {
			c.mode = "none"
		}
		out = append(out, probeCase{recordingCase: c})
	}
	captured, err := capturedCases()
	if err != nil {
		return nil, err
	}
	out = append(out, captured...)
	seen := map[string]bool{}
	for _, c := range out {
		if seen[c.name] {
			return nil, fmt.Errorf("duplicate case %s", c.name)
		}
		seen[c.name] = true
	}
	return out, nil
}

// writeCase records one case with the vendored saveRecordings and decodes the
// result as the replay plugin does. It reports whether the replay decode
// failed.
// caseResult is what the shapes self-check needs from a recorded case.
type caseResult struct {
	existing []byte
	recs     []recording.Recording
	out      []byte
}

func writeCase(recDir string, c probeCase, results map[string]caseResult) (bool, error) {
	dir := filepath.Join(recDir, c.name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return false, err
	}
	var existing []byte
	switch {
	case c.existingFrom != "":
		r, ok := results[c.existingFrom]
		if !ok {
			return false, fmt.Errorf("existingFrom %s: not recorded yet", c.existingFrom)
		}
		existing = r.out
	case c.existing != "":
		existing = []byte(c.existing)
	}
	var existingName *string
	if existing != nil {
		name := "existing.yaml"
		existingName = &name
		if err := os.WriteFile(filepath.Join(dir, name), existing, 0o644); err != nil {
			return false, err
		}
	}
	var raw bytes.Buffer
	if err := json.Compact(&raw, []byte(c.recordings)); err != nil {
		return false, fmt.Errorf("recordings JSON: %w", err)
	}
	if err := writeJSON(filepath.Join(dir, "input.json"), caseInput{
		Case: c.name, StreamingMode: c.mode, UserMessageIndex: c.index,
		Existing: existingName, Recordings: raw.Bytes(),
	}); err != nil {
		return false, err
	}
	recs, err := decodeRecordings(raw.Bytes())
	if err != nil {
		return false, err
	}
	filename, out, err := record(c.mode, c.index, existing, recs)
	if err != nil {
		return false, err
	}
	if c.direct != nil {
		_, direct, err := record(c.mode, c.index, existing, c.direct)
		if err != nil {
			return false, err
		}
		if !bytes.Equal(direct, out) {
			return false, fmt.Errorf("record bytes from the captured Go value differ from those of its JSON:\n%s\n---\n%s", direct, out)
		}
	}
	results[c.name] = caseResult{existing: existing, recs: recs, out: out}
	if err := os.WriteFile(filepath.Join(dir, filename), out, 0o644); err != nil {
		return false, err
	}
	rep := replay(out)
	return rep.Error != nil, writeJSON(filepath.Join(dir, "replay.json"), rep)
}

// decodeRecordings builds the Go recordings from input JSON: genai/model
// values through encoding/json, FunctionDeclaration
// ParametersJsonSchema/ResponseJsonSchema as *jsonschema.Schema (what
// functiontool stores), each request passed through sanitizeLLMRequest as the
// record plugin's beforeModel does.
func decodeRecordings(raw []byte) ([]recording.Recording, error) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	var in []jsonRecording
	if err := dec.Decode(&in); err != nil {
		return nil, fmt.Errorf("recordings JSON: %w", err)
	}
	out := []recording.Recording{}
	for _, r := range in {
		rec := recording.Recording{UserMessageIndex: r.UserMessageIndex, AgentName: r.AgentName}
		if l := r.LLMRecording; l != nil {
			rec.LLMRecording = &recording.LLMRecording{LLMResponses: l.LLMResponses}
			if q := l.LLMRequest; q != nil {
				req := &model.LLMRequest{Model: q.Model, Contents: q.Contents, Config: q.Config}
				if err := toJSONSchemas(req.Config); err != nil {
					return nil, err
				}
				rec.LLMRecording.LLMRequest = sanitizeLLMRequest(req)
			}
		}
		if t := r.ToolRecording; t != nil {
			rec.ToolRecording = &recording.ToolRecording{ToolCall: t.ToolCall, ToolResponse: t.ToolResponse}
		}
		out = append(out, rec)
	}
	return out, nil
}

func toJSONSchemas(cfg *genai.GenerateContentConfig) error {
	if cfg == nil {
		return nil
	}
	conv := func(v any) (any, error) {
		if v == nil {
			return nil, nil
		}
		b, err := json.Marshal(v)
		if err != nil {
			return nil, err
		}
		s := new(jsonschema.Schema)
		if err := json.Unmarshal(b, s); err != nil {
			return nil, fmt.Errorf("jsonschema %s: %w", b, err)
		}
		return s, nil
	}
	for _, t := range cfg.Tools {
		if t == nil {
			continue
		}
		for _, d := range t.FunctionDeclarations {
			var err error
			if d.ParametersJsonSchema, err = conv(d.ParametersJsonSchema); err != nil {
				return err
			}
			if d.ResponseJsonSchema, err = conv(d.ResponseJsonSchema); err != nil {
				return err
			}
		}
	}
	return nil
}

func toJSONRecordings(recs []recording.Recording) []jsonRecording {
	out := []jsonRecording{}
	for _, r := range recs {
		j := jsonRecording{UserMessageIndex: r.UserMessageIndex, AgentName: r.AgentName}
		if l := r.LLMRecording; l != nil {
			j.LLMRecording = &jsonLLMRecording{LLMResponses: l.LLMResponses}
			if q := l.LLMRequest; q != nil {
				j.LLMRecording.LLMRequest = &jsonLLMRequest{Model: q.Model, Contents: q.Contents, Config: q.Config}
			}
		}
		if t := r.ToolRecording; t != nil {
			j.ToolRecording = &jsonToolRecording{ToolCall: t.ToolCall, ToolResponse: t.ToolResponse}
		}
		out = append(out, j)
	}
	return out
}

// record runs the vendored saveRecordings in a fresh temp case dir (seeded
// with existing when non-nil) and returns the file name and bytes it wrote.
func record(mode string, index int, existing []byte, recs []recording.Recording) (string, []byte, error) {
	dir, err := os.MkdirTemp("", "gen-go-configurable-")
	if err != nil {
		return "", nil, err
	}
	defer os.RemoveAll(dir)
	state := newInvocationRecordState(dir, index, mode)
	state.recordings = recs
	filename := "generated-recordings.yaml"
	if mode == "sse" {
		filename = "generated-recordings-sse.yaml"
	}
	if existing != nil {
		if err := os.WriteFile(filepath.Join(dir, filename), existing, 0o644); err != nil {
			return "", nil, err
		}
	}
	p := &recordPlugin{invocationStates: map[string]*invocationRecordState{}, allowedBaseDir: dir}
	p.saveRecordings(state)
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", nil, err
	}
	if len(entries) != 1 || entries[0].Name() != filename {
		return "", nil, fmt.Errorf("saveRecordings wrote %v, want only %s", entries, filename)
	}
	out, err := os.ReadFile(filepath.Join(dir, filename))
	return filename, out, err
}

// replay decodes recordings bytes as replayplugin's loadInvocationState does:
// yaml.Unmarshal into a yaml.Node, normalizeYAMLNode, Decode into
// recording.Recordings. Error is the Unmarshal or Decode error text (the
// plugin wraps them as "failed to parse recordings from %s: %w" /
// "failed to decode recordings: %w"); Recordings is what Decode filled in,
// also on error.
func replay(data []byte) replayOutput {
	var root yaml.Node
	if err := yaml.Unmarshal(data, &root); err != nil {
		msg := err.Error()
		return replayOutput{Error: &msg, Recordings: []jsonRecording{}}
	}
	normalizeYAMLNode(&root)
	var recs recording.Recordings
	out := replayOutput{}
	if err := root.Decode(&recs); err != nil {
		msg := err.Error()
		out.Error = &msg
	}
	out.Recordings = toJSONRecordings(recs.Recordings)
	return out
}

// capturedCases are the cases whose input comes from ADK-Go itself: one per
// conformance function tool (the functiontool.New declaration, read through
// Declaration()), and the first LLM request of a public llmagent run under the
// public runner (transfer_to_agent from sub-agents; the conformance tools
// packed into a request).
func capturedCases() ([]probeCase, error) {
	var out []probeCase
	for _, t := range conformanceTools {
		decl := t.(interface {
			Declaration() *genai.FunctionDeclaration
		}).Declaration()
		req := &model.LLMRequest{
			Model:    "gemini-2.5-flash",
			Contents: []*genai.Content{genai.NewContentFromText("Hi", genai.RoleUser)},
			Config: &genai.GenerateContentConfig{
				Tools: []*genai.Tool{{FunctionDeclarations: []*genai.FunctionDeclaration{decl}}},
			},
		}
		c, err := capturedCase("conformance-"+t.Name(), "tools_agent", req)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}

	sub := func(name, desc string) (agent.Agent, error) {
		return llmagent.New(llmagent.Config{Name: name, Description: desc, Model: stubModel{}})
	}
	billing, err := sub("billing_agent", "Handles billing questions.")
	if err != nil {
		return nil, err
	}
	support, err := sub("support_agent", "Handles technical support.")
	if err != nil {
		return nil, err
	}
	var captured []*model.LLMRequest
	capture := func(ctx agent.Context, req *model.LLMRequest) (*model.LLMResponse, error) {
		captured = append(captured, sanitizeLLMRequest(req))
		return &model.LLMResponse{Content: genai.NewContentFromText("done", genai.RoleModel)}, nil
	}
	root, err := llmagent.New(llmagent.Config{
		Name: "root_agent", Description: "Routes requests.", Model: stubModel{},
		Instruction:          "Route the user to the right agent.",
		SubAgents:            []agent.Agent{billing, support},
		BeforeModelCallbacks: []llmagent.BeforeModelCallback{capture},
	})
	if err != nil {
		return nil, err
	}
	toolsAgent, err := llmagent.New(llmagent.Config{
		Name: "tools_agent", Model: stubModel{}, Instruction: "Use the tools.",
		Tools:                append([]tool.Tool(nil), conformanceTools...),
		BeforeModelCallbacks: []llmagent.BeforeModelCallback{capture},
	})
	if err != nil {
		return nil, err
	}
	for _, r := range []struct {
		name string
		a    agent.Agent
	}{{"transfer-to-agent", root}, {"runner-tools-agent", toolsAgent}} {
		captured = nil
		if err := runOnce(r.a); err != nil {
			return nil, fmt.Errorf("%s: %w", r.name, err)
		}
		if len(captured) != 1 {
			return nil, fmt.Errorf("%s: captured %d requests, want 1", r.name, len(captured))
		}
		c, err := capturedCase(r.name, r.a.Name(), captured[0])
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, nil
}

func capturedCase(name, agentName string, req *model.LLMRequest) (probeCase, error) {
	recs := []recording.Recording{{
		UserMessageIndex: 0, AgentName: agentName,
		LLMRecording: &recording.LLMRecording{
			LLMRequest:   sanitizeLLMRequest(req),
			LLMResponses: []*model.LLMResponse{{Content: genai.NewContentFromText("done", genai.RoleModel)}},
		},
	}}
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(toJSONRecordings(recs)); err != nil {
		return probeCase{}, err
	}
	return probeCase{
		recordingCase: recordingCase{name: name, mode: "none", recordings: b.String()},
		direct:        recs,
	}, nil
}

// runOnce runs a over the public runner with one user message; the capture
// callback answers the first model call, so the stub model is never called.
func runOnce(a agent.Agent) error {
	r, err := runner.New(runner.Config{
		AppName: "gen-go-configurable", Agent: a,
		SessionService: session.InMemoryService(), AutoCreateSession: true,
	})
	if err != nil {
		return err
	}
	for _, err := range r.Run(context.Background(), "user", "s1",
		genai.NewContentFromText("Hi", genai.RoleUser), agent.RunConfig{}) {
		if err != nil {
			return err
		}
	}
	return nil
}

type stubModel struct{}

func (stubModel) Name() string { return "gemini-2.5-flash" }

func (stubModel) GenerateContent(context.Context, *model.LLMRequest, bool) iter.Seq2[*model.LLMResponse, error] {
	return func(yield func(*model.LLMResponse, error) bool) {
		yield(nil, errors.New("gen-go-configurable: the stub model must not be called"))
	}
}

// writeJSON writes v as 2-space-indented JSON without HTML escaping and with a
// trailing newline.
func writeJSON(path string, v any) error {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		return err
	}
	return os.WriteFile(path, buf.Bytes(), 0o644)
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
