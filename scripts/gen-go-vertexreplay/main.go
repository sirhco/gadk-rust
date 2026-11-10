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

// Command gen-go-vertexreplay converts ADK-Go's recorded Agent Engine
// conversations (session/vertexai/testdata/Test_vertexaiService_*.replay,
// cloud.google.com/go/rpcreplay files) into gadk-vertexai replay fixtures:
// one JSON file per recording holding the ordered unary calls, each with its
// request and its response (or recorded gRPC error) as protojson with enum
// numbers, the form Go's REST client puts on the wire.
//
// The stale recording Test_vertexaiService_StateManagement_temp_state_is_not_persisted
// (ADK-Go skips that subtest) is not converted, leaving 22 fixtures.
//
// Run from this directory:
//
//	go run . [-adk-go DIR] [-out DIR]   # (re)write the fixtures
//	go run . -check [-adk-go DIR]       # exit 1 unless the committed fixtures match
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"google.golang.org/grpc/codes"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/anypb"

	spb "google.golang.org/genproto/googleapis/rpc/status"

	pb "cloud.google.com/go/rpcreplay/proto/rpcreplay"

	// Register the message types the recordings' Any values name.
	_ "cloud.google.com/go/aiplatform/apiv1beta1/aiplatformpb"
	_ "cloud.google.com/go/longrunning/autogen/longrunningpb"
)

// magic opens every rpcreplay file (rpcreplay.go, "File format").
const magic = "RPCReplay"

// stale is the one recording ADK-Go no longer replays.
const stale = "Test_vertexaiService_StateManagement_temp_state_is_not_persisted"

// wantFixtures is the number of recordings converted.
const wantFixtures = 22

// marshal is Go's REST-client protojson form (aiplatform apiv1beta1 *_client.go).
var marshal = protojson.MarshalOptions{AllowPartial: true, UseEnumNumbers: true}

var unmarshal = proto.UnmarshalOptions{AllowPartial: true, DiscardUnknown: true}

type callError struct {
	Code     int32  `json:"code"`
	CodeName string `json:"codeName"`
	Message  string `json:"message"`
}

type call struct {
	Method   string     `json:"method"`
	Request  any        `json:"request"`
	Response any        `json:"response,omitempty"`
	Error    *callError `json:"error,omitempty"`
}

type fixture struct {
	Calls []*call `json:"calls"`
}

func main() {
	adkGo := flag.String("adk-go", "../../../adk-go", "ADK-Go checkout holding session/vertexai/testdata")
	out := flag.String("out", "../../crates/gadk-vertexai/testdata/replays", "fixture output directory")
	checkOnly := flag.Bool("check", false, "regenerate into a temporary directory and compare with -out")
	flag.Parse()
	log.SetFlags(0)

	fixtures, err := convertAll(filepath.Join(*adkGo, "session", "vertexai", "testdata"))
	if err != nil {
		log.Fatal(err)
	}
	if !*checkOnly {
		if err := os.MkdirAll(*out, 0o755); err != nil {
			log.Fatal(err)
		}
		if err := writeAll(*out, fixtures); err != nil {
			log.Fatal(err)
		}
		fmt.Printf("wrote %d fixtures to %s\n", len(fixtures), *out)
		return
	}
	tmp, err := os.MkdirTemp("", "gen-go-vertexreplay")
	if err != nil {
		log.Fatal(err)
	}
	defer os.RemoveAll(tmp)
	if err := writeAll(tmp, fixtures); err != nil {
		log.Fatal(err)
	}
	problems, err := compare(tmp, *out)
	if err != nil {
		log.Fatal(err)
	}
	if len(problems) > 0 {
		for _, p := range problems {
			fmt.Fprintln(os.Stderr, p)
		}
		os.RemoveAll(tmp)
		os.Exit(1)
	}
	fmt.Printf("ok: %d fixtures match\n", len(fixtures))
}

// convertAll converts every used recording in dir, keyed by output file name.
func convertAll(dir string) (map[string][]byte, error) {
	paths, err := filepath.Glob(filepath.Join(dir, "Test_vertexaiService_*.replay"))
	if err != nil {
		return nil, err
	}
	fixtures := map[string][]byte{}
	for _, p := range paths {
		stem := strings.TrimSuffix(filepath.Base(p), ".replay")
		if stem == stale {
			continue
		}
		f, err := convert(p)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", p, err)
		}
		data, err := encode(f)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", p, err)
		}
		fixtures[stem+".json"] = data
	}
	if len(fixtures) != wantFixtures {
		return nil, fmt.Errorf("found %d recordings under %s, want %d", len(fixtures), dir, wantFixtures)
	}
	return fixtures, nil
}

// convert reads one rpcreplay file: the header (magic and the initial-state
// record, which is skipped), then length-prefixed Entry protos. Each RESPONSE
// is attached to the REQUEST its 1-based ref_index names.
func convert(path string) (*fixture, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	r := bytes.NewReader(data)
	head := make([]byte, len(magic))
	if _, err := io.ReadFull(r, head); err != nil || string(head) != magic {
		return nil, errors.New("not a replay file (does not begin with magic string)")
	}
	if _, err := readRecord(r); err != nil {
		return nil, fmt.Errorf("missing initial state: %w", err)
	}
	f := &fixture{Calls: []*call{}}
	byIndex := map[int]*call{}
	for i := 1; ; i++ {
		rec, err := readRecord(r)
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("entry #%d: %w", i, err)
		}
		var e pb.Entry
		if err := proto.Unmarshal(rec, &e); err != nil {
			return nil, fmt.Errorf("entry #%d: %w", i, err)
		}
		switch e.GetKind() {
		case pb.Entry_REQUEST:
			if e.GetMessage() == nil || e.GetIsError() {
				return nil, fmt.Errorf("entry #%d: request without a message", i)
			}
			req, err := messageJSON(e.GetMessage())
			if err != nil {
				return nil, fmt.Errorf("entry #%d: %w", i, err)
			}
			c := &call{Method: e.GetMethod(), Request: req}
			f.Calls = append(f.Calls, c)
			byIndex[i] = c
		case pb.Entry_RESPONSE:
			c := byIndex[int(e.GetRefIndex())]
			if c == nil {
				return nil, fmt.Errorf("entry #%d: response refers to unknown request #%d", i, e.GetRefIndex())
			}
			delete(byIndex, int(e.GetRefIndex()))
			// Recorded responses leave method unset; check it only when present.
			if m := e.GetMethod(); m != "" && m != c.Method {
				return nil, fmt.Errorf("entry #%d: response method %s, request method %s", i, m, c.Method)
			}
			if e.GetMessage() == nil {
				return nil, fmt.Errorf("entry #%d: response without a message", i)
			}
			if e.GetIsError() {
				var s spb.Status
				if err := anypb.UnmarshalTo(e.GetMessage(), &s, unmarshal); err != nil {
					return nil, fmt.Errorf("entry #%d: %w", i, err)
				}
				c.Error = &callError{
					Code:     s.GetCode(),
					CodeName: codes.Code(s.GetCode()).String(),
					Message:  s.GetMessage(),
				}
			} else {
				resp, err := messageJSON(e.GetMessage())
				if err != nil {
					return nil, fmt.Errorf("entry #%d: %w", i, err)
				}
				c.Response = resp
			}
		default:
			return nil, fmt.Errorf("entry #%d: unsupported entry kind %s (streams are not converted)", i, e.GetKind())
		}
	}
	if len(byIndex) > 0 {
		return nil, fmt.Errorf("%d request(s) without a recorded response", len(byIndex))
	}
	return f, nil
}

// readRecord reads one record: a little-endian uint32 length L, then L bytes.
func readRecord(r io.Reader) ([]byte, error) {
	var size uint32
	if err := binary.Read(r, binary.LittleEndian, &size); err != nil {
		return nil, err
	}
	buf := make([]byte, size)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

// messageJSON decodes an Any to its concrete message and returns its protojson
// as a generic JSON value (numbers kept verbatim as json.Number).
func messageJSON(a *anypb.Any) (any, error) {
	m, err := anypb.UnmarshalNew(a, unmarshal)
	if err != nil {
		return nil, err
	}
	b, err := marshal.Marshal(m)
	if err != nil {
		return nil, err
	}
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		return nil, err
	}
	return v, nil
}

// encode writes stable pretty JSON: sorted keys, 2-space indent, no HTML
// escaping, trailing newline.
func encode(f *fixture) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(f); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func writeAll(dir string, fixtures map[string][]byte) error {
	for name, data := range fixtures {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o644); err != nil {
			return err
		}
	}
	return nil
}

// compare lists every fixture that differs between the regenerated set in
// fresh and the committed set in committed, or exists in only one of them.
func compare(fresh, committed string) ([]string, error) {
	want, err := readJSONFiles(fresh)
	if err != nil {
		return nil, err
	}
	have, err := readJSONFiles(committed)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	var problems []string
	for name, w := range want {
		h, ok := have[name]
		switch {
		case !ok:
			problems = append(problems, "missing fixture: "+name)
		case !bytes.Equal(w, h):
			problems = append(problems, "fixture differs: "+name)
		}
	}
	for name := range have {
		if _, ok := want[name]; !ok {
			problems = append(problems, "extra fixture: "+name)
		}
	}
	sort.Strings(problems)
	return problems, nil
}

func readJSONFiles(dir string) (map[string][]byte, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	files := map[string][]byte{}
	for _, e := range entries {
		if e.IsDir() || filepath.Ext(e.Name()) != ".json" {
			continue
		}
		data, err := os.ReadFile(filepath.Join(dir, e.Name()))
		if err != nil {
			return nil, err
		}
		files[e.Name()] = data
	}
	return files, nil
}
