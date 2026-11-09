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

// Command gen-go-sessiondb runs ADK-Go's database session service on sqlite
// (glebarez/sqlite, the driver ADK-Go's own tests use) through a fixed
// script, and writes the schema, the stored rows, the script and ADK-Go's
// read-back results as gadk-session-database parity fixtures.
//
// Run from this directory: TZ=UTC go run .
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/glebarez/sqlite"
	"github.com/google/jsonschema-go/jsonschema"
	"google.golang.org/genai"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"

	"google.golang.org/adk/v2/platform"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/session/database"
)

const outDir = "../../crates/gadk-session-database/tests/testdata"

// t0 is the script's base time; its nanoseconds exercise microsecond truncation.
var t0 = time.Date(2026, 3, 4, 5, 6, 7, 123456789, time.UTC)

var tables = []string{"sessions", "events", "app_states", "user_states"}

type step struct {
	Op        string          `json:"op"`
	Now       *time.Time      `json:"now,omitempty"`
	AppName   string          `json:"app_name"`
	UserID    string          `json:"user_id"`
	SessionID string          `json:"session_id"`
	State     map[string]any  `json:"state,omitempty"`
	Event     json.RawMessage `json:"event,omitempty"`
}

type view struct {
	AppName        string           `json:"app_name"`
	UserID         string           `json:"user_id"`
	ID             string           `json:"id"`
	State          map[string]any   `json:"state"`
	LastUpdateTime time.Time        `json:"last_update_time"`
	Events         []*session.Event `json:"events,omitempty"`
}

func main() {
	if _, off := time.Now().Zone(); off != 0 {
		log.Fatal("run with TZ=UTC: the stored text depends on the local zone")
	}
	dir, err := os.MkdirTemp("", "gen-go-sessiondb")
	check(err)
	defer os.RemoveAll(dir)
	dsn := filepath.Join(dir, "go.db")
	quiet := &gorm.Config{Logger: logger.Default.LogMode(logger.Silent)}

	svc, err := database.NewSessionService(sqlite.Open(dsn), quiet)
	check(err)
	check(database.AutoMigrate(svc))
	steps := script()
	for _, s := range steps {
		apply(svc, s)
	}

	db, err := gorm.Open(sqlite.Open(dsn), quiet)
	check(err)
	write("schema.sqlite.txt", dumpSchema(db))
	write("rows.sqlite.sql", dumpRows(db))
	write("script.json", marshal(map[string]any{"steps": steps}))
	write("expected.json", marshal(expected(svc)))
}

// newEvent builds an event through session.NewEvent with fixed providers.
func newEvent(id, author string, ts time.Time) *session.Event {
	ctx := platform.WithUUIDProvider(context.Background(), func() string { return id })
	ctx = platform.WithTimeProvider(ctx, func() time.Time { return ts })
	ev := session.NewEvent(ctx, "inv-1")
	ev.Author = author
	return ev
}

func raw(ev *session.Event) json.RawMessage {
	b, err := json.Marshal(ev)
	check(err)
	return b
}

func script() []step {
	at := func(d time.Duration) *time.Time { t := t0.Add(d); return &t }

	full := newEvent("e-full", "agent", t0.Add(2*time.Second))
	full.Branch = "root.child"
	full.IsolationScope = "task-1"
	full.LongRunningToolIDs = []string{"call-1"}
	full.Routes = []string{"route_a", "route_b"}
	full.Output = map[string]any{"answer": 42.5, "html": "<b>&</b>"}
	full.NodeInfo = &session.NodeInfo{Path: "wf/ask", MessageAsOutput: true, OutputFor: []string{"wf"}}
	full.RequestedInput = &session.RequestInput{InterruptID: "ask", Message: "Name?", ResponseSchema: &jsonschema.Schema{Type: "string"}}
	full.Actions.StateDelta = map[string]any{"app:theme": "light", "user:lang": "fr", "temp:t": 1.0, "count": 2.0, "ratio": 1e-7}
	full.Actions.ArtifactDelta = map[string]int64{"report.txt": 3}
	full.Actions.SkipSummarization = true
	full.Actions.TransferToAgent = "helper"
	full.Actions.Escalate = true
	full.LLMResponse.Content = genai.NewContentFromText("hello <world> &  ", genai.RoleModel)
	full.LLMResponse.GroundingMetadata = &genai.GroundingMetadata{WebSearchQueries: []string{"weather"}}
	full.LLMResponse.CustomMetadata = map[string]any{"trace": "abc", "n": 1.0}
	full.LLMResponse.UsageMetadata = &genai.GenerateContentResponseUsageMetadata{PromptTokenCount: 3, CandidatesTokenCount: 5, TotalTokenCount: 8}
	full.LLMResponse.CitationMetadata = &genai.CitationMetadata{Citations: []*genai.Citation{{URI: "https://example.com", StartIndex: 1, EndIndex: 4}}}
	full.LLMResponse.InputTranscription = &genai.Transcription{Text: "what time is it", Finished: true}
	full.LLMResponse.OutputTranscription = &genai.Transcription{Text: "it is noon"}
	full.LLMResponse.TurnComplete = true
	full.LLMResponse.Interrupted = true
	full.LLMResponse.ErrorCode = "E42"
	full.LLMResponse.ErrorMessage = "partial failure"

	partial := newEvent("e-partial", "agent", t0.Add(3*time.Second))
	partial.LLMResponse.Partial = true
	partial.LLMResponse.Content = genai.NewContentFromText("stream", genai.RoleModel)

	plain := newEvent("e-plain", "user", t0.Add(4*time.Second))
	plain.LLMResponse.Content = genai.NewContentFromText("hi", genai.RoleUser)

	record := newEvent("e-compaction", "user", t0.Add(5*time.Second))
	record.Actions.Compaction = &session.EventCompaction{
		StartTimestamp:   t0,
		EndTimestamp:     t0.Add(4 * time.Second),
		CompactedContent: genai.NewContentFromText("summary", genai.RoleModel),
	}

	orphan := newEvent("e-orphan", "user", t0.Add(7*time.Second))

	return []step{
		{Op: "create", Now: at(0), AppName: "app", UserID: "u1", SessionID: "s1", State: map[string]any{
			"sk": "v", "app:theme": "dark", "user:lang": "en", "temp:scratch": 1.0, "n": 1.5, "big": 1e21, "html": "<a&b>",
		}},
		{Op: "create", Now: at(time.Second), AppName: "app", UserID: "u2", SessionID: "s2"},
		{Op: "append", AppName: "app", UserID: "u1", SessionID: "s1", Event: raw(full)},
		{Op: "append", AppName: "app", UserID: "u1", SessionID: "s1", Event: raw(partial)},
		{Op: "append", AppName: "app", UserID: "u1", SessionID: "s1", Event: raw(plain)},
		{Op: "append", AppName: "app", UserID: "u1", SessionID: "s1", Event: raw(record)},
		{Op: "create", Now: at(6 * time.Second), AppName: "app", UserID: "u1", SessionID: "s3", State: map[string]any{"x": true}},
		{Op: "append", AppName: "app", UserID: "u1", SessionID: "s3", Event: raw(orphan)},
		{Op: "delete", AppName: "app", UserID: "u1", SessionID: "s3"},
	}
}

// apply runs one step. Appends re-read the session first, as the Rust replay does, and
// decode the event from the script's JSON so both sides append the same value.
func apply(svc session.Service, s step) {
	ctx := context.Background()
	switch s.Op {
	case "create":
		now := *s.Now
		ctx = platform.WithTimeProvider(ctx, func() time.Time { return now })
		_, err := svc.Create(ctx, &session.CreateRequest{AppName: s.AppName, UserID: s.UserID, SessionID: s.SessionID, State: s.State})
		check(err)
	case "append":
		got, err := svc.Get(ctx, &session.GetRequest{AppName: s.AppName, UserID: s.UserID, SessionID: s.SessionID})
		check(err)
		var ev session.Event
		check(json.Unmarshal(s.Event, &ev))
		check(svc.AppendEvent(ctx, got.Session, &ev))
	case "delete":
		check(svc.Delete(ctx, &session.DeleteRequest{AppName: s.AppName, UserID: s.UserID, SessionID: s.SessionID}))
	default:
		log.Fatalf("unknown op %q", s.Op)
	}
}

func toView(s session.Session, withEvents bool) view {
	v := view{AppName: s.AppName(), UserID: s.UserID(), ID: s.ID(), State: map[string]any{}, LastUpdateTime: s.LastUpdateTime()}
	for k, val := range s.State().All() {
		v.State[k] = val
	}
	if withEvents {
		for ev := range s.Events().All() {
			v.Events = append(v.Events, ev)
		}
	}
	return v
}

func expected(svc session.Service) map[string]any {
	ctx := context.Background()
	var gets []view
	for _, k := range [][3]string{{"app", "u1", "s1"}, {"app", "u2", "s2"}} {
		got, err := svc.Get(ctx, &session.GetRequest{AppName: k[0], UserID: k[1], SessionID: k[2]})
		check(err)
		gets = append(gets, toView(got.Session, true))
	}
	listed, err := svc.List(ctx, &session.ListRequest{AppName: "app"})
	check(err)
	var lists []view
	for _, s := range listed.Sessions {
		lists = append(lists, toView(s, false))
	}
	sort.Slice(lists, func(i, j int) bool {
		if lists[i].UserID != lists[j].UserID {
			return lists[i].UserID < lists[j].UserID
		}
		return lists[i].ID < lists[j].ID
	})
	return map[string]any{"get": gets, "list": lists}
}

// dumpSchema: one `type\tname\ttbl_name\tsql` line per sqlite_master row.
func dumpSchema(db *gorm.DB) []byte {
	rows, err := db.Raw("SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name").Rows()
	check(err)
	defer rows.Close()
	var b bytes.Buffer
	for rows.Next() {
		var typ, name, tbl, sql string
		check(rows.Scan(&typ, &name, &tbl, &sql))
		fmt.Fprintf(&b, "%s\t%s\t%s\t%s\n", typ, name, tbl, sql)
	}
	check(rows.Err())
	return b.Bytes()
}

// dumpRows: per table, one INSERT line per row with sqlite quote() values in column order,
// rows ordered by the primary key.
func dumpRows(db *gorm.DB) []byte {
	var b bytes.Buffer
	for _, t := range tables {
		type col struct {
			Cid  int
			Name string
			Pk   int
		}
		var cols []col
		check(db.Raw("SELECT cid, name, pk FROM pragma_table_info(?) ORDER BY cid", t).Scan(&cols).Error)
		var vals, pks []string
		for _, c := range cols {
			vals = append(vals, fmt.Sprintf("quote(`%s`)", c.Name))
		}
		sort.SliceStable(cols, func(i, j int) bool { return cols[i].Pk < cols[j].Pk })
		for _, c := range cols {
			if c.Pk > 0 {
				pks = append(pks, fmt.Sprintf("`%s`", c.Name))
			}
		}
		q := fmt.Sprintf("SELECT %s FROM `%s` ORDER BY %s", strings.Join(vals, " || ',' || "), t, strings.Join(pks, ", "))
		rows, err := db.Raw(q).Rows()
		check(err)
		for rows.Next() {
			var line string
			check(rows.Scan(&line))
			fmt.Fprintf(&b, "INSERT INTO `%s` VALUES(%s);\n", t, line)
		}
		check(rows.Err())
		rows.Close()
	}
	return b.Bytes()
}

func marshal(v any) []byte {
	b, err := json.MarshalIndent(v, "", "  ")
	check(err)
	return append(b, '\n')
}

func write(name string, data []byte) {
	check(os.MkdirAll(outDir, 0o755))
	check(os.WriteFile(filepath.Join(outDir, name), data, 0o644))
}

func check(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
