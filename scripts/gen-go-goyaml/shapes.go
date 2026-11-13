// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

// Serde shapes (spec R6/R11): Go structs whose yaml tags are what spec R6
// derives from the Rust mirror in crates/gadk-goyaml/tests/support/shapes.rs
// (SCENARIO.md "Serde shapes" lists each mirror). Sized integer types are
// used throughout so Go's type names in TypeError texts (int8, uint64, ...)
// are the ones serde's entry points name.

import "gopkg.in/yaml.v3"

// Scalars covers every scalar kind; all keys tagged (verbatim).
type Scalars struct {
	B   bool    `yaml:"b"`
	I8  int8    `yaml:"i8"`
	I16 int16   `yaml:"i16"`
	I32 int32   `yaml:"i32"`
	I64 int64   `yaml:"i64"`
	U8  uint8   `yaml:"u8"`
	U16 uint16  `yaml:"u16"`
	U32 uint32  `yaml:"u32"`
	U64 uint64  `yaml:"u64"`
	F32 float32 `yaml:"f32"`
	F64 float64 `yaml:"f64"`
	S   string  `yaml:"s"`
}

// Inner is the nested struct of Nested and Optionals.
type Inner struct {
	A int64  `yaml:"a"`
	B string `yaml:"b,omitempty"`
}

// Nested covers struct values, struct pointers and slices of structs.
type Nested struct {
	Name     string  `yaml:"name"`
	Inner    Inner   `yaml:"inner"`
	InnerPtr *Inner  `yaml:"inner_ptr,omitempty"`
	List     []Inner `yaml:"list"`
}

// Optionals covers pointers (Rust Option), with and without omitempty.
type Optionals struct {
	S     *string  `yaml:"s"`
	I     *int64   `yaml:"i,omitempty"`
	F     *float64 `yaml:"f"`
	B     *bool    `yaml:"b,omitempty"`
	Inner *Inner   `yaml:"inner"`
}

// Collections covers []byte, slices and maps.
type Collections struct {
	Bytes  []byte                 `yaml:"bytes"`
	Ints   []int64                `yaml:"ints"`
	Strs   []string               `yaml:"strs,omitempty"`
	Counts map[string]int64       `yaml:"counts"`
	Any    map[string]interface{} `yaml:"any"`
	Items  []interface{}          `yaml:"items"`
}

// WithInline covers the inline catch-all map (Rust InlineMap).
type WithInline struct {
	Name  string                 `yaml:"name"`
	Count int64                  `yaml:"count,omitempty"`
	Extra map[string]interface{} `yaml:",inline"`
}

// WithNode covers yaml.Node and []yaml.Node fields (Rust Document and
// Vec<Document>).
type WithNode struct {
	Kind  string      `yaml:"kind"`
	Spec  yaml.Node   `yaml:"spec"`
	Edges []yaml.Node `yaml:"edges"`
}

// GoDefaults has untagged fields: Go's key is the field name lowercased
// (usermessageindex, ...), and Index is skipped.
type GoDefaults struct {
	UserMessageIndex int64
	AgentName        string
	HTTPTimeout      float64
	Enabled          bool
	Index            int64 `yaml:"-"`
}

// Recordings, Recording, LLMRecording and ToolRecording follow ADK-Go's
// internal/configurable/conformance/replayplugin/recording types, with the
// genai/model payloads as generic maps.
type Recordings struct {
	Recordings []Recording `yaml:"recordings"`
}

// Recording is one recordings entry.
type Recording struct {
	UserMessageIndex int64          `yaml:"user_message_index"`
	AgentName        string         `yaml:"agent_name"`
	LLMRecording     *LLMRecording  `yaml:"llm_recording,omitempty"`
	ToolRecording    *ToolRecording `yaml:"tool_recording,omitempty"`
}

// LLMRecording is a request and its responses.
type LLMRecording struct {
	LLMRequest   map[string]interface{}   `yaml:"llm_request,omitempty"`
	LLMResponses []map[string]interface{} `yaml:"llm_responses,omitempty"`
}

// ToolRecording is a tool call and its response.
type ToolRecording struct {
	ToolCall     map[string]interface{} `yaml:"tool_call,omitempty"`
	ToolResponse map[string]interface{} `yaml:"tool_response,omitempty"`
}

type shape struct {
	name     string
	newValue func() interface{} // a pointer to a new zero value
	cases    []shapeCase
}

type shapeCase struct {
	name  string
	input *string // nil: the zero value, not decoded
}

func in(s string) *string { return &s }

// zero is every shape's first case.
var zero = shapeCase{"zero", nil}

var shapes = []shape{
	{"scalars", func() interface{} { return new(Scalars) }, []shapeCase{
		zero,
		{"full", in("b: true\ni8: -128\ni16: 32767\ni32: -2147483648\ni64: 9223372036854775807\nu8: 255\nu16: 65535\nu32: 4294967295\nu64: 18446744073709551615\nf32: 0.7\nf64: 0.1\ns: hello\n")},
		{"float-special", in("f32: .inf\nf64: -.inf\n")},
		{"float-nan", in("f32: .nan\nf64: .NaN\n")},
		{"float-format", in("f32: 3.4028235e38\nf64: 1e21\n")},
		{"float-small", in("f32: 1e-7\nf64: 0.00001\n")},
		{"float-from-int", in("f32: 16777217\nf64: -0\n")},
		{"int-from-float", in("i64: 1.0\ni32: 2e2\nu8: 1.5\n")},
		{"overflow", in("i8: 128\nu8: 256\nu16: -1\ni16: 0x8000\nu64: -1\n")},
		{"mismatch", in("b: yes\ni32: abc\nf64: true\ns: [1, 2]\nu32: {}\n")},
		{"string-from-int", in("s: 123\n")},
		{"string-from-bool", in("s: true\n")},
		{"string-from-null", in("s: null\n")},
		{"string-from-timestamp", in("s: 2001-12-14T21:59:43Z\n")},
		{"string-from-binary", in("s: !!binary aGVsbG8=\n")},
		{"string-needs-quotes", in("s: \"123\"\n")},
		{"string-multiline", in("s: \"a\\nb\"\n")},
		{"bool-forms", in("b: True\n")},
		{"bool-from-int", in("b: 1\n")},
		{"int-forms", in("i64: 0x1F\ni32: 0o17\ni16: 0b101\ni8: -0x10\nu32: 1_000\n")},
		{"unknown-keys", in("x: 1\nb: true\ny: [1]\nB: false\n")},
		{"alias", in("base: &v 42\ni64: *v\ns: *v\n")},
		{"merge", in("<<: {i8: 1, s: merged}\ns: own\n")},
		{"merge-alias", in("defaults: &d {u8: 7, s: d}\n<<: *d\nb: true\n")},
		{"duplicate-key", in("i8: 1\ni8: 2\n")},
		{"not-a-map", in("[1, 2]\n")},
		{"scalar-root", in("hello\n")},
		{"null-root", in("~\n")},
		{"empty", in("")},
		{"syntax-error", in("b: [\n")},
	}},
	{"nested", func() interface{} { return new(Nested) }, []shapeCase{
		zero,
		{"full", in("name: n\ninner: {a: 1, b: x}\ninner_ptr: {a: 2}\nlist:\n  - a: 3\n  - {a: 4, b: y}\n")},
		{"nulls", in("inner: ~\ninner_ptr: ~\nlist: ~\n")},
		{"list-mismatch", in("list: [1, {a: x}]\n")},
		{"alias-struct", in("inner: &i {a: 5, b: z}\ninner_ptr: *i\nlist: [*i, *i]\n")},
		{"merge-into-inner", in("inner:\n  <<: {a: 9}\n  b: m\n")},
		{"empty-ptr", in("inner_ptr: {}\n")},
		{"inner-not-map", in("inner: 5\n")},
	}},
	{"optionals", func() interface{} { return new(Optionals) }, []shapeCase{
		zero,
		{"full", in("s: x\ni: 0\nf: 1.5\nb: false\ninner: {a: 1}\n")},
		{"nulls", in("s: null\ni: ~\nf:\nb: null\ninner: null\n")},
		{"empty-string", in("s: ''\n")},
		{"mismatch", in("i: x\nf: [1]\n")},
		{"alias", in("s: &a v\ninner: {b: *a}\n")},
	}},
	{"collections", func() interface{} { return new(Collections) }, []shapeCase{
		zero,
		{"full", in("bytes: [1, 2, 255]\nints: [1, -2]\nstrs: [a, b]\ncounts: {a: 1, b: 2}\nany: {k: v, n: 1, l: [1, x], m: {z: 1, a: 2}}\nitems: [1, 1.5, x, true, null, 2001-12-14]\n")},
		{"binary", in("bytes: !!binary aGVsbG8=\n")},
		{"bytes-from-string", in("bytes: hello\n")},
		{"bytes-overflow", in("bytes: [256, -1]\n")},
		{"mismatch", in("counts: {a: x, b: 2}\nints: [1, y]\n")},
		{"nonstring-keys", in("any: {1: a, true: b}\ncounts: {1: 1}\n")},
		{"sorted-keys", in("counts: {z: 1, a: 2, a10: 3, a2: 4, B: 5, '1': 6, '10': 7, '9': 8, a-b: 9, ab: 10}\n")},
		{"empty-strs", in("strs: []\nints: []\n")},
		{"nested-items", in("items: [[1, 2], {a: b}, {1: x}, {}]\n")},
		{"null-maps", in("counts: ~\nany: ~\nitems: ~\n")},
	}},
	{"inline", func() interface{} { return new(WithInline) }, []shapeCase{
		zero,
		{"full", in("name: n\ncount: 2\nzeta: 1\nalpha: [x]\nbeta: {k: v}\n")},
		{"only-extras", in("x: 1\n")},
		{"count-zero", in("name: n\ncount: 0\n")},
		{"merge-extras", in("<<: {extra1: 1, name: merged}\nextra2: 2\n")},
		{"alias-extras", in("a: &x {k: v}\nb: *x\nname: q\n")},
		{"mismatch", in("name: [1]\nextra: 1\n")},
		{"nonstring-keys", in("1: one\ntrue: t\nname: n\n")},
	}},
	{"node", func() interface{} { return new(WithNode) }, []shapeCase{
		zero,
		{"full", in("kind: k\nspec:\n  a: 1 # comment\n  b: [x, y]\nedges:\n  - {from: a, to: b}\n  - scalar\n")},
		{"spec-scalar", in("kind: k\nspec: 'quoted'\n")},
		{"anchor-within", in("spec:\n  base: &b {x: 1}\n  use: *b\n")},
		{"comments", in("# head\nkind: k # line\nspec:\n  # inner head\n  a: 1\n  # inner foot\nedges: []\n")},
		{"edges-not-seq", in("edges: {a: 1}\n")},
	}},
	{"godefaults", func() interface{} { return new(GoDefaults) }, []shapeCase{
		zero,
		{"full", in("usermessageindex: 3\nagentname: root\nhttptimeout: 2.5\nenabled: true\nindex: 9\n")},
		{"snake-keys", in("user_message_index: 3\nagent_name: root\n")},
		{"field-name-keys", in("UserMessageIndex: 3\nAgentName: root\n")},
	}},
	{"recordings", func() interface{} { return new(Recordings) }, []shapeCase{
		zero,
		{"llm", in(recordingsLLM)},
		{"tools", in(recordingsTools)},
		{"mismatch", in("recordings:\n  - user_message_index: x\n    agent_name: [a]\n")},
	}},
}
