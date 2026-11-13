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

package main

import (
	"bytes"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"

	"github.com/google/jsonschema-go/jsonschema"
	"gopkg.in/yaml.v3"

	"gadk/gen-go-configurable/recording"
)

// shape is the Go struct shape yaml.v3 encodes for one struct type
// (SCENARIO.md "Shapes"): its fields in Go order with the zero node Go emits
// for each.
type shape struct {
	GoType string       `json:"go_type"`
	Fields []shapeField `json:"fields"`
}

type shapeField struct {
	Key      string   `json:"key"`
	GoName   string   `json:"go_name"`
	JSON     string   `json:"json"`
	Category string   `json:"category"`
	GoType   string   `json:"go_type"`
	Elem     string   `json:"elem"`
	Zero     zeroNode `json:"zero"`
}

// zeroNode is the node Go's node.Encode emits for a field's zero value:
// kind "scalar" (tag, style, value), "sequence"/"mapping" (empty; tag,
// style), "struct" (a non-pointer struct: the zero mapping of shape ref) or
// "unencodable" (yaml.v3 panics on it).
type zeroNode struct {
	Kind  string   `json:"kind"`
	Tag   string   `json:"tag,omitempty"`
	Style []string `json:"style,omitempty"`
	Value *string  `json:"value,omitempty"`
	Ref   string   `json:"ref,omitempty"`
}

type shapePath struct {
	Path string `json:"path"`
	Type string `json:"type"`
}

var styleNames = []struct {
	s    yaml.Style
	name string
}{
	{yaml.TaggedStyle, "tagged"}, {yaml.DoubleQuotedStyle, "double"}, {yaml.SingleQuotedStyle, "single"},
	{yaml.LiteralStyle, "literal"}, {yaml.FoldedStyle, "folded"}, {yaml.FlowStyle, "flow"},
}

func styleList(s yaml.Style) []string {
	var out []string
	for _, n := range styleNames {
		if s&n.s != 0 {
			out = append(out, n.name)
		}
	}
	return out
}

// fieldCategory classifies a field type as the shapes table reports it.
func fieldCategory(t reflect.Type) string {
	switch fixupClass(t) {
	case "Bytes":
		return "bytes"
	case "Duration":
		if t.Kind() != reflect.Pointer {
			return "duration"
		}
	case "Time":
		if t.Kind() != reflect.Pointer {
			return "time"
		}
	case "TextMarshaler":
		if t.Kind() != reflect.Pointer {
			return "textmarshaler"
		}
	}
	switch t.Kind() {
	case reflect.Pointer:
		return "ptr"
	case reflect.Interface:
		return "iface"
	case reflect.Slice, reflect.Array:
		return "slice"
	case reflect.Map:
		return "map"
	case reflect.String:
		return "string"
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return "int"
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return "uint"
	case reflect.Float32, reflect.Float64:
		return "float"
	case reflect.Bool:
		return "bool"
	case reflect.Struct:
		return "struct"
	case reflect.Func, reflect.Chan:
		return "func"
	}
	return t.Kind().String()
}

// structElem is the struct type name reached through t's pointers, slice
// elements and map values, or "" if there is none.
func structElem(t reflect.Type) string {
	for {
		switch t.Kind() {
		case reflect.Pointer, reflect.Slice, reflect.Array, reflect.Map:
			if fixupClass(t) == "Bytes" {
				return ""
			}
			t = t.Elem()
			continue
		case reflect.Struct:
			if fixupClass(t) != "" && fixupClass(t) != "SchemaTitle" {
				return ""
			}
			return typeName(t)
		}
		return ""
	}
}

// encodeValue is Go's node.Encode of one value (nil when yaml.v3 panics).
func encodeValue(v reflect.Value) (n *yaml.Node) {
	defer func() {
		if recover() != nil {
			n = nil
		}
	}()
	n = new(yaml.Node)
	var iv any
	if v.IsValid() {
		iv = v.Interface()
	}
	if err := n.Encode(iv); err != nil {
		return nil
	}
	return n
}

func zeroOf(t reflect.Type) zeroNode {
	if t.Kind() == reflect.Struct && fieldCategory(t) == "struct" {
		return zeroNode{Kind: "struct", Ref: typeName(t)}
	}
	n := encodeValue(reflect.Zero(t))
	if n == nil {
		return zeroNode{Kind: "unencodable"}
	}
	switch n.Kind {
	case yaml.ScalarNode:
		v := n.Value
		return zeroNode{Kind: "scalar", Tag: n.Tag, Style: styleList(n.Style), Value: &v}
	case yaml.SequenceNode:
		return zeroNode{Kind: "sequence", Tag: n.Tag, Style: styleList(n.Style)}
	case yaml.MappingNode:
		return zeroNode{Kind: "mapping", Tag: n.Tag, Style: styleList(n.Style)}
	}
	return zeroNode{Kind: fmt.Sprint(n.Kind)}
}

// buildShapes returns the shape of every struct type in types.
func buildShapes(types map[string]reflect.Type) map[string]shape {
	out := map[string]shape{}
	for name, t := range types {
		s := shape{GoType: name, Fields: []shapeField{}}
		for i := 0; i < t.NumField(); i++ {
			sf := t.Field(i)
			if sf.PkgPath != "" && !sf.Anonymous {
				continue
			}
			jsonName, _, _ := strings.Cut(sf.Tag.Get("json"), ",")
			s.Fields = append(s.Fields, shapeField{
				Key: strings.ToLower(sf.Name), GoName: sf.Name, JSON: jsonName,
				Category: fieldCategory(sf.Type), GoType: typeName(sf.Type),
				Elem: structElem(sf.Type), Zero: zeroOf(sf.Type),
			})
		}
		out[name] = s
	}
	return out
}

// selfCheck is fixups.json's "shape_selfcheck": how many corpus cases the
// shapes-driven rebuild reproduces byte for byte, per numbers mode.
type selfCheck struct {
	Cases       int             `json:"cases"`
	GoNumbers   selfCheckResult `json:"go_numbers"`
	JSONNumbers selfCheckResult `json:"json_numbers"`
}

type selfCheckResult struct {
	Pass     int      `json:"pass"`
	Failures []string `json:"failures"`
}

// shapeBuilder rebuilds the node tree Go's node.Encode(&recordings) builds,
// as gadk will: struct mappings come only from the shapes table (field order,
// keys, zero nodes for every field a zero-skipping encoding leaves out),
// present leaves are encoded on their own. jsonNumbers turns integral float64
// values in `any` positions into int64, as a serde_json Value holds JSON
// integers.
type shapeBuilder struct {
	shapes      map[string]shape
	jsonNumbers bool
}

func scalar(tag, value string, style yaml.Style) *yaml.Node {
	return &yaml.Node{Kind: yaml.ScalarNode, Tag: tag, Value: value, Style: style}
}

func (b *shapeBuilder) zeroNode(z zeroNode) *yaml.Node {
	switch z.Kind {
	case "scalar":
		return scalar(z.Tag, *z.Value, styleOf(z.Style))
	case "sequence":
		return &yaml.Node{Kind: yaml.SequenceNode, Tag: z.Tag, Style: styleOf(z.Style)}
	case "mapping":
		return &yaml.Node{Kind: yaml.MappingNode, Tag: z.Tag, Style: styleOf(z.Style)}
	case "struct":
		return b.structNode(z.Ref, reflect.Value{})
	}
	panic("zero node of kind " + z.Kind)
}

func styleOf(names []string) yaml.Style {
	var s yaml.Style
	for _, n := range styleNames {
		for _, m := range names {
			if m == n.name {
				s |= n.s
			}
		}
	}
	return s
}

// skipped reports whether a zero-skipping (omitempty) encoding leaves v out.
func skipped(v reflect.Value) bool {
	switch v.Kind() {
	case reflect.Pointer, reflect.Interface:
		return v.IsNil()
	case reflect.Slice, reflect.Map:
		return v.Len() == 0
	case reflect.Struct:
		return v.IsZero()
	}
	return v.IsZero()
}

// structNode builds the mapping of struct type name from v (invalid = all
// fields absent).
func (b *shapeBuilder) structNode(name string, v reflect.Value) *yaml.Node {
	s, ok := b.shapes[name]
	if !ok {
		panic("no shape for " + name)
	}
	m := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
	for _, f := range s.Fields {
		var fv reflect.Value
		if v.IsValid() {
			fv = v.FieldByName(f.GoName)
		}
		var n *yaml.Node
		if !fv.IsValid() || skipped(fv) {
			n = b.zeroNode(f.Zero)
		} else {
			n = b.valueNode(fv)
		}
		m.Content = append(m.Content, b.key(f.Key), n)
	}
	return m
}

// valueNode builds the node of a present value.
func (b *shapeBuilder) valueNode(v reflect.Value) *yaml.Node {
	t := v.Type()
	if v.Kind() == reflect.Interface {
		if v.IsNil() {
			return scalar("!!null", "null", 0)
		}
		if s, ok := v.Interface().(*jsonschema.Schema); ok {
			return b.structNode(typeName(reflect.TypeFor[jsonschema.Schema]()), reflect.ValueOf(s).Elem())
		}
		return b.leaf(v)
	}
	if name := structElem(t); name != "" {
		switch t.Kind() {
		case reflect.Pointer:
			if v.IsNil() {
				return scalar("!!null", "null", 0)
			}
			return b.valueNode(v.Elem())
		case reflect.Struct:
			return b.structNode(name, v)
		case reflect.Slice, reflect.Array:
			seq := &yaml.Node{Kind: yaml.SequenceNode, Tag: "!!seq"}
			if v.Len() == 0 {
				seq.Style = yaml.FlowStyle
			}
			for i := 0; i < v.Len(); i++ {
				seq.Content = append(seq.Content, b.valueNode(v.Index(i)))
			}
			return seq
		case reflect.Map:
			m := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
			if v.Len() == 0 {
				m.Style = yaml.FlowStyle
			}
			keys := v.MapKeys()
			sort.Slice(keys, func(i, j int) bool { return keys[i].String() < keys[j].String() })
			for _, k := range keys {
				m.Content = append(m.Content, b.key(k.String()), b.valueNode(v.MapIndex(k)))
			}
			return m
		}
	}
	return b.leaf(v)
}

// key is a mapping key node, encoded as Go encodes a string (yaml.v3's
// stringv: quoted when the plain form would resolve to another tag or is a
// YAML 1.1 bool such as "y").
func (b *shapeBuilder) key(k string) *yaml.Node {
	return b.leaf(reflect.ValueOf(k))
}

// leaf encodes a value with no struct below it (or an `any`) on its own.
func (b *shapeBuilder) leaf(v reflect.Value) *yaml.Node {
	iv := v.Interface()
	if b.jsonNumbers {
		iv = intNumbers(iv)
	}
	n := new(yaml.Node)
	if err := n.Encode(iv); err != nil {
		panic(err)
	}
	return n
}

func intNumbers(v any) any {
	switch x := v.(type) {
	case float64:
		if x == math.Trunc(x) && math.Abs(x) < 1<<53 {
			return int64(x)
		}
	case map[string]any:
		out := make(map[string]any, len(x))
		for k, e := range x {
			out[k] = intNumbers(e)
		}
		return out
	case []any:
		out := make([]any, len(x))
		for i, e := range x {
			out[i] = intNumbers(e)
		}
		return out
	}
	return v
}

// recordingsNode builds the recordings mapping with the recording roots'
// yaml tags (omitempty as Go) and shapes below them.
func (b *shapeBuilder) recordingsNode(recs []recording.Recording) *yaml.Node {
	seq := &yaml.Node{Kind: yaml.SequenceNode, Tag: "!!seq"}
	if len(recs) == 0 {
		seq.Style = yaml.FlowStyle
	}
	str := func(s string) *yaml.Node { return b.leaf(reflect.ValueOf(s)) }
	for _, r := range recs {
		m := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
		m.Content = append(m.Content, scalar("!!str", "user_message_index", 0),
			b.leaf(reflect.ValueOf(r.UserMessageIndex)), scalar("!!str", "agent_name", 0), str(r.AgentName))
		if l := r.LLMRecording; l != nil {
			lm := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
			if l.LLMRequest != nil {
				lm.Content = append(lm.Content, scalar("!!str", "llm_request", 0),
					b.structNode("model.LLMRequest", reflect.ValueOf(l.LLMRequest).Elem()))
			}
			if len(l.LLMResponses) > 0 {
				lm.Content = append(lm.Content, scalar("!!str", "llm_responses", 0),
					b.valueNode(reflect.ValueOf(l.LLMResponses)))
			}
			if len(lm.Content) == 0 {
				lm.Style = yaml.FlowStyle
			}
			m.Content = append(m.Content, scalar("!!str", "llm_recording", 0), lm)
		}
		if t := r.ToolRecording; t != nil {
			tm := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
			if t.ToolCall != nil {
				tm.Content = append(tm.Content, scalar("!!str", "tool_call", 0),
					b.structNode("genai.FunctionCall", reflect.ValueOf(t.ToolCall).Elem()))
			}
			if t.ToolResponse != nil {
				tm.Content = append(tm.Content, scalar("!!str", "tool_response", 0),
					b.structNode("genai.FunctionResponse", reflect.ValueOf(t.ToolResponse).Elem()))
			}
			if len(tm.Content) == 0 {
				tm.Style = yaml.FlowStyle
			}
			m.Content = append(m.Content, scalar("!!str", "tool_recording", 0), tm)
		}
		seq.Content = append(seq.Content, m)
	}
	return &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map",
		Content: []*yaml.Node{scalar("!!str", "recordings", 0), seq}}
}

// shapeRecord is saveRecordings with node.Encode replaced by the shapes
// rebuild (the merge read stays Go's plain yaml.Unmarshal, as gadk's
// from_node_partial port does).
func (b *shapeBuilder) shapeRecord(index int, existing []byte, recs []recording.Recording) (out []byte, err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("panic: %v", r)
		}
	}()
	var existingRecordings recording.Recordings
	if existing != nil {
		_ = yaml.Unmarshal(existing, &existingRecordings)
	}
	var filtered []recording.Recording
	for _, r := range existingRecordings.Recordings {
		if r.UserMessageIndex != index {
			filtered = append(filtered, r)
		}
	}
	filtered = append(filtered, recs...)
	node := b.recordingsNode(filtered)
	cleanYAMLNode(node, false)
	return yaml.Marshal(node)
}

// runSelfCheck rebuilds every case with both numbers modes and compares
// with Go's bytes; mismatching cases are listed with the first differing
// line and their bytes written under failDir (when set).
func runSelfCheck(shapes map[string]shape, cases []probeCase, results map[string]caseResult) (selfCheck, error) {
	sc := selfCheck{Cases: len(cases), GoNumbers: selfCheckResult{Failures: []string{}},
		JSONNumbers: selfCheckResult{Failures: []string{}}}
	for _, mode := range []struct {
		json bool
		res  *selfCheckResult
	}{{false, &sc.GoNumbers}, {true, &sc.JSONNumbers}} {
		b := &shapeBuilder{shapes: shapes, jsonNumbers: mode.json}
		for _, c := range cases {
			r := results[c.name]
			got, err := b.shapeRecord(c.index, r.existing, r.recs)
			if err != nil {
				return sc, fmt.Errorf("self-check %s: %w", c.name, err)
			}
			if bytes.Equal(got, r.out) {
				mode.res.Pass++
				continue
			}
			mode.res.Failures = append(mode.res.Failures, c.name+": "+firstDiff(r.out, got))
			if d := os.Getenv("GEN_GO_CONFIGURABLE_SELFCHECK_DIR"); d != "" {
				_ = os.WriteFile(filepath.Join(d, fmt.Sprintf("%s-%v.yaml", c.name, mode.json)), got, 0o644)
			}
		}
	}
	return sc, nil
}

func firstDiff(want, got []byte) string {
	w, g := strings.Split(string(want), "\n"), strings.Split(string(got), "\n")
	for i := 0; i < len(w) || i < len(g); i++ {
		var a, b string
		if i < len(w) {
			a = w[i]
		}
		if i < len(g) {
			b = g[i]
		}
		if a != b {
			return fmt.Sprintf("line %d: go %q, shapes %q", i+1, strings.TrimSpace(a), strings.TrimSpace(b))
		}
	}
	return "identical lines"
}
