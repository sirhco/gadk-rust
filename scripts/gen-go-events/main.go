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

// Command gen-go-events marshals ten reference ADK-Go session.Event values
// with encoding/json and writes them as gadk-session parity fixtures (R18).
//
// Run from this directory: go run .
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"time"

	"google.golang.org/genai"

	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool/toolconfirmation"
)

const outDir = "../../crates/gadk-session/tests/fixtures/go_events"

// baseTime is the fixed timestamp of every fixture event.
var baseTime = time.Date(2024, 12, 1, 8, 0, 0, 123000000, time.UTC)

// newEvent builds an event through session.NewEvent with the time and UUID
// providers fixed, so the output is deterministic.
func newEvent(id, author string) *session.Event {
	ctx := platform.WithTimeProvider(context.Background(), func() time.Time { return baseTime })
	ctx = platform.WithUUIDProvider(ctx, func() string { return id })
	ev := session.NewEvent(ctx, "inv-1")
	ev.Author = author
	return ev
}

type fixture struct {
	name  string
	event *session.Event
}

func fixtures() []fixture {
	var out []fixture

	// 1. Bare event straight from session.NewEvent.
	out = append(out, fixture{"01_bare", newEvent("evt-1", "user")})

	// 2. Model text with usage metadata and model version.
	ev := newEvent("evt-2", "model")
	ev.Content = genai.NewContentFromText("hello", genai.RoleModel)
	ev.UsageMetadata = &genai.GenerateContentResponseUsageMetadata{
		PromptTokenCount:     3,
		CandidatesTokenCount: 5,
		TotalTokenCount:      8,
	}
	ev.ModelVersion = "gemini-2.5-flash"
	out = append(out, fixture{"02_text_usage", ev})

	// 3. A function call and its response.
	ev = newEvent("evt-3", "model")
	ev.Content = &genai.Content{
		Role: genai.RoleModel,
		Parts: []*genai.Part{
			{FunctionCall: &genai.FunctionCall{
				ID:   "c1",
				Name: "get_weather",
				Args: map[string]any{"city": "Lisbon"},
			}},
			{FunctionResponse: &genai.FunctionResponse{
				ID:       "c1",
				Name:     "get_weather",
				Response: map[string]any{"temp": 21},
			}},
		},
	}
	out = append(out, fixture{"03_function_call_response", ev})

	// 4. Allocated-but-empty StateDelta versus nil ArtifactDelta.
	ev = newEvent("evt-4", "model")
	ev.Actions.StateDelta = map[string]any{}
	ev.Actions.ArtifactDelta = nil
	out = append(out, fixture{"04_delta_nil_vs_empty", ev})

	// 5. Compaction record.
	ev = newEvent("evt-5", "model")
	ev.Actions.Compaction = &session.EventCompaction{
		StartTimestamp:   baseTime.Add(-10 * time.Minute),
		EndTimestamp:     baseTime.Add(-1 * time.Minute),
		CompactedContent: genai.NewContentFromText("summary", genai.RoleModel),
		ExcludedEvents: []session.EventRef{
			{InvocationID: "inv-0", Timestamp: baseTime.Add(-5 * time.Minute)},
		},
	}
	out = append(out, fixture{"05_compaction", ev})

	// 6. Requested tool confirmations.
	ev = newEvent("evt-6", "model")
	ev.Actions.RequestedToolConfirmations = map[string]toolconfirmation.ToolConfirmation{
		"c1": {Hint: "approve?", Confirmed: false, Payload: map[string]any{"x": 1}},
	}
	out = append(out, fixture{"06_tool_confirmations", ev})

	// 7. Workflow node info, requested input, output and routes.
	ev = newEvent("evt-7", "model")
	ev.NodeInfo = &session.NodeInfo{Path: "root/a", MessageAsOutput: true, OutputFor: []string{"b"}}
	ev.RequestedInput = &session.RequestInput{
		InterruptID: "i1",
		Message:     "need input",
		Payload:     map[string]any{"k": "v"},
	}
	ev.Output = map[string]any{"summary": "ok"}
	ev.Routes = []string{"reviewer"}
	out = append(out, fixture{"07_node_info_input_output", ev})

	// 8. A part carrying only a thought signature.
	ev = newEvent("evt-8", "model")
	ev.Content = &genai.Content{
		Role:  genai.RoleModel,
		Parts: []*genai.Part{{ThoughtSignature: []byte("sig")}},
	}
	out = append(out, fixture{"08_thought_signature_only", ev})

	// 9. Streaming flags and transcriptions.
	ev = newEvent("evt-9", "model")
	ev.Partial = true
	ev.TurnComplete = true
	ev.Interrupted = true
	ev.InputTranscription = &genai.Transcription{Text: "hi", Finished: true}
	ev.OutputTranscription = &genai.Transcription{Text: "yo"}
	out = append(out, fixture{"09_partial_transcriptions", ev})

	// 10. Error fields, finish reason, and HTML-escaped characters.
	ev = newEvent("evt-10", "model")
	ev.ErrorCode = "SAFETY"
	ev.ErrorMessage = "<blocked> & done"
	ev.FinishReason = genai.FinishReasonSafety
	ev.Content = genai.NewContentFromText("a<b>&c", genai.RoleModel)
	out = append(out, fixture{"10_error_finish_escape", ev})

	return out
}

func main() {
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		log.Fatal(err)
	}
	for _, f := range fixtures() {
		data, err := json.Marshal(f.event)
		if err != nil {
			log.Fatalf("%s: %v", f.name, err)
		}
		path := filepath.Join(outDir, f.name+".json")
		if err := os.WriteFile(path, append(data, '\n'), 0o644); err != nil {
			log.Fatalf("%s: %v", f.name, err)
		}
		fmt.Println("wrote", path)
	}
}
