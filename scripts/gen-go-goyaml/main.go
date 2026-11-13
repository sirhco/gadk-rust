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

// Command gen-go-goyaml records gopkg.in/yaml.v3 v3.0.1's behaviour as JSON
// goldens for gadk-goyaml (SCENARIO.md): parse/emit/value goldens for the
// parse corpus, the seeded differential corpus, serde-shape goldens, and the
// strconv.FormatFloat(f, 'g', -1, 32|64) table for gadk-platform.
//
// Run from this directory: go run .
package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"math/rand"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"time"
	"unicode/utf8"

	"gopkg.in/yaml.v3"
)

// maxJSONDepth keeps every golden loadable by serde_json's default
// recursion limit (128: at most 127 nested arrays/objects).
const maxJSONDepth = 127

var panics int

func main() {
	testdata := flag.String("testdata", "../../crates/gadk-goyaml/testdata",
		"gadk-goyaml testdata dir (reads corpus/differential.json; rewrites corpus/parse and golden/)")
	floatOut := flag.String("float", "../../crates/gadk-platform/tests/testdata/go_format_float_g.json",
		"FormatFloat table output file")
	flag.Parse()
	// Timestamps decoded with an offset keep it; with TZ=UTC no zone name
	// depends on the host (TestNodeRoundtrip does the same).
	os.Setenv("TZ", "UTC")
	if err := run(*testdata, *floatOut); err != nil {
		fmt.Fprintln(os.Stderr, "gen-go-goyaml:", err)
		os.Exit(1)
	}
}

func run(testdata, floatOut string) error {
	dir, err := goyamlDir()
	if err != nil {
		return err
	}
	tables, err := tableCorpus(dir)
	if err != nil {
		return fmt.Errorf("reading yaml.v3 test tables in %s (go mod download gopkg.in/yaml.v3): %w", dir, err)
	}
	corpus := append(tables, edgeCorpus()...)
	seen := map[string]bool{}
	for _, c := range corpus {
		if seen[c.name] {
			return fmt.Errorf("duplicate case name %s", c.name)
		}
		seen[c.name] = true
	}
	seeds, err := readSeeds(filepath.Join(testdata, "corpus", "differential.json"))
	if err != nil {
		return err
	}

	corpusDir := filepath.Join(testdata, "corpus", "parse")
	goldenDir := filepath.Join(testdata, "golden")
	for _, d := range []string{corpusDir, goldenDir} {
		if err := os.RemoveAll(d); err != nil {
			return err
		}
	}
	for _, d := range []string{corpusDir, filepath.Join(goldenDir, "parse"), filepath.Join(goldenDir, "diff"), filepath.Join(goldenDir, "serde")} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}

	for _, c := range corpus {
		if err := os.WriteFile(filepath.Join(corpusDir, c.name+".yaml"), c.data, 0o644); err != nil {
			return err
		}
		g := parseGolden(c.name, c.data, nil)
		if err := writeJSON(filepath.Join(goldenDir, "parse", c.name+".json"), g); err != nil {
			return err
		}
	}

	ndiff := 0
	for _, seed := range seeds {
		for n, v := range diffValues(seed) {
			name := fmt.Sprintf("%d-%d", seed, n)
			var in []byte
			if err := protect(func() (err error) { in, err = yaml.Marshal(v); return err }); err != nil {
				return fmt.Errorf("diff %s: marshal: %v", name, err)
			}
			src := dumpValue(v)
			g := parseGolden(name, in, src)
			if err := writeJSON(filepath.Join(goldenDir, "diff", name+".json"), g); err != nil {
				return err
			}
			ndiff++
		}
	}

	nserde := 0
	for _, s := range shapes {
		for _, c := range s.cases {
			g := serdeGolden(s, c)
			if err := writeJSON(filepath.Join(goldenDir, "serde", s.name+"-"+c.name+".json"), g); err != nil {
				return err
			}
			nserde++
		}
	}

	table := floatTable()
	if err := os.MkdirAll(filepath.Dir(floatOut), 0o755); err != nil {
		return err
	}
	if err := writeJSON(floatOut, table); err != nil {
		return err
	}

	if panics > 0 {
		fmt.Fprintf(os.Stderr, "note: %d yaml.v3 calls panicked (recorded as \"panic: ...\" errors)\n", panics)
	}
	fmt.Printf("wrote corpus/parse=%d golden/parse=%d golden/diff=%d golden/serde=%d to %s; float=%d to %s\n",
		len(corpus), len(corpus), ndiff, nserde, testdata, len(table), floatOut)
	return nil
}

func runGo(args ...string) (string, error) {
	out, err := exec.Command("go", args...).Output()
	if err != nil {
		return "", fmt.Errorf("go %v: %w", args, err)
	}
	return string(out), nil
}

func readSeeds(path string) ([]int64, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var f struct {
		Seeds []int64 `json:"seeds"`
	}
	if err := json.Unmarshal(data, &f); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if len(f.Seeds) != 32 {
		return nil, fmt.Errorf("%s: want 32 seeds, have %d", path, len(f.Seeds))
	}
	return f.Seeds, nil
}

// protect runs f, turning a panic that is not one of yaml.v3's own (those
// come back as errors already) into an error "panic: <value>".
func protect(f func() error) (err error) {
	defer func() {
		if r := recover(); r != nil {
			panics++
			err = fmt.Errorf("panic: %v", r)
		}
	}()
	return f()
}

// ---- JSON output ----

type kv struct {
	k string
	v interface{}
}

// obj is a JSON object whose keys keep their order.
type obj []kv

func (o obj) MarshalJSON() ([]byte, error) {
	var b bytes.Buffer
	b.WriteByte('{')
	for i, e := range o {
		if i > 0 {
			b.WriteByte(',')
		}
		k, err := compactJSON(e.k)
		if err != nil {
			return nil, err
		}
		v, err := compactJSON(e.v)
		if err != nil {
			return nil, err
		}
		b.Write(k)
		b.WriteByte(':')
		b.Write(v)
	}
	b.WriteByte('}')
	return b.Bytes(), nil
}

func compactJSON(v interface{}) ([]byte, error) {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(b.Bytes(), []byte("\n")), nil
}

// writeJSON writes v with 2-space indentation, no HTML escaping and a
// trailing newline.
func writeJSON(path string, v interface{}) error {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		return fmt.Errorf("%s: %w", path, err)
	}
	if d := jsonDepth(b.Bytes()); d > maxJSONDepth {
		return fmt.Errorf("%s: JSON nesting %d exceeds %d", path, d, maxJSONDepth)
	}
	return os.WriteFile(path, b.Bytes(), 0o644)
}

func jsonDepth(b []byte) int {
	depth, max := 0, 0
	inStr, esc := false, false
	for _, c := range b {
		switch {
		case inStr && esc:
			esc = false
		case inStr && c == '\\':
			esc = true
		case inStr && c == '"':
			inStr = false
		case inStr:
		case c == '"':
			inStr = true
		case c == '{' || c == '[':
			depth++
			if depth > max {
				max = depth
			}
		case c == '}' || c == ']':
			depth--
		}
	}
	return max
}

// text returns s for a JSON string field; Go's JSON encoder would replace
// invalid UTF-8 silently, so it is a probe error instead.
func text(s string) string {
	if !utf8.ValidString(s) {
		panic(fmt.Sprintf("invalid UTF-8 in a text field: %q", s))
	}
	return s
}

func b64(b []byte) *string {
	s := base64.StdEncoding.EncodeToString(b)
	return &s
}

func errText(err error) interface{} {
	if err == nil {
		return nil
	}
	return text(err.Error())
}

// ---- Node dump ----

var kindNames = map[yaml.Kind]string{
	yaml.DocumentNode: "document",
	yaml.SequenceNode: "sequence",
	yaml.MappingNode:  "mapping",
	yaml.ScalarNode:   "scalar",
	yaml.AliasNode:    "alias",
}

var styleNames = []struct {
	s    yaml.Style
	name string
}{
	{yaml.TaggedStyle, "tagged"},
	{yaml.DoubleQuotedStyle, "double"},
	{yaml.SingleQuotedStyle, "single"},
	{yaml.LiteralStyle, "literal"},
	{yaml.FoldedStyle, "folded"},
	{yaml.FlowStyle, "flow"},
}

// dumpNode dumps root and its content; an alias target is the index path
// from root to the first node (pre-order) that is the target pointer.
func dumpNode(root *yaml.Node) obj {
	paths := map[*yaml.Node][]int{}
	var index func(n *yaml.Node, p []int)
	index = func(n *yaml.Node, p []int) {
		if _, ok := paths[n]; !ok {
			paths[n] = append([]int{}, p...)
		}
		for i, c := range n.Content {
			index(c, append(append([]int{}, p...), i))
		}
	}
	index(root, []int{})
	var dump func(n *yaml.Node) obj
	dump = func(n *yaml.Node) obj {
		kind, ok := kindNames[n.Kind]
		if !ok {
			panic(fmt.Sprintf("unexpected node kind %d", n.Kind))
		}
		style := []string{}
		for _, s := range styleNames {
			if n.Style&s.s != 0 {
				style = append(style, s.name)
			}
		}
		var alias interface{}
		if n.Alias != nil {
			p, ok := paths[n.Alias]
			if !ok {
				panic("alias target outside the dumped tree")
			}
			alias = p
		}
		content := []obj{}
		for _, c := range n.Content {
			content = append(content, dump(c))
		}
		return obj{
			{"kind", kind},
			{"style", style},
			{"tag", text(n.Tag)},
			{"value", text(n.Value)},
			{"anchor", text(n.Anchor)},
			{"alias", alias},
			{"head", text(n.HeadComment)},
			{"line_comment", text(n.LineComment)},
			{"foot", text(n.FootComment)},
			{"line", n.Line},
			{"column", n.Column},
			{"content", content},
		}
	}
	return dump(root)
}

// ---- GoValue dump ----

func dumpValue(v interface{}) obj {
	switch v := v.(type) {
	case nil:
		return obj{{"t", "null"}}
	case bool:
		return obj{{"t", "bool"}, {"v", v}}
	case int:
		return obj{{"t", "int"}, {"v", strconv.FormatInt(int64(v), 10)}}
	case int64:
		return obj{{"t", "int"}, {"v", strconv.FormatInt(v, 10)}}
	case uint64:
		return obj{{"t", "uint"}, {"v", strconv.FormatUint(v, 10)}}
	case float64:
		return obj{{"t", "float"}, {"v", strconv.FormatFloat(v, 'g', -1, 64)}}
	case string:
		if !utf8.ValidString(v) {
			return obj{{"t", "bytes"}, {"b64", b64([]byte(v))}}
		}
		return obj{{"t", "string"}, {"v", v}}
	case time.Time:
		return obj{{"t", "timestamp"}, {"v", v.Format(time.RFC3339Nano)}}
	case []interface{}:
		items := []obj{}
		for _, e := range v {
			items = append(items, dumpValue(e))
		}
		return obj{{"t", "seq"}, {"v", items}}
	case map[string]interface{}:
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		entries := [][]interface{}{}
		for _, k := range keys {
			entries = append(entries, []interface{}{text(k), dumpValue(v[k])})
		}
		return obj{{"t", "map"}, {"v", entries}}
	case map[interface{}]interface{}:
		type entry struct {
			key  string
			pair []interface{}
		}
		var es []entry
		for k, e := range v {
			kd := dumpValue(k)
			kj, err := compactJSON(kd)
			if err != nil {
				panic(err)
			}
			es = append(es, entry{string(kj), []interface{}{kd, dumpValue(e)}})
		}
		sort.Slice(es, func(i, j int) bool { return es[i].key < es[j].key })
		entries := [][]interface{}{}
		for _, e := range es {
			entries = append(entries, e.pair)
		}
		return obj{{"t", "mapany"}, {"v", entries}}
	}
	panic(fmt.Sprintf("unexpected decoded value %T", v))
}

// ---- Parse goldens ----

// parseGolden records every yaml.v3 view of one input (Appendix C.golden).
// source is the generated value's dump for differential cases, else nil.
func parseGolden(name string, in []byte, source obj) obj {
	g := obj{{"case", name}, {"input_b64", b64(in)}}
	if source != nil {
		g = append(g, kv{"source", source})
	}

	var node yaml.Node
	err := protect(func() error { return yaml.Unmarshal(in, &node) })
	var nodeDump, marshalB64, marshalErr, indent2 interface{}
	if err == nil {
		if node.Kind != 0 {
			nodeDump = dumpNode(&node)
		}
		var out []byte
		if merr := protect(func() (e error) { out, e = yaml.Marshal(&node); return e }); merr != nil {
			marshalErr = text(merr.Error())
		} else {
			marshalB64 = b64(out)
		}
		if out, ierr := encodeAll([]*yaml.Node{&node}, 2); ierr == nil {
			indent2 = b64(out)
		}
	}
	g = append(g,
		kv{"node", nodeDump},
		kv{"error", errText(err)},
		kv{"marshal_b64", marshalB64},
		kv{"marshal_error", marshalErr},
		kv{"indent2_b64", indent2},
	)

	dec := yaml.NewDecoder(bytes.NewReader(in))
	var nodes []*yaml.Node
	var serr error
	for len(nodes) < 1000 {
		n := new(yaml.Node)
		e := protect(func() error { return dec.Decode(n) })
		if errors.Is(e, io.EOF) {
			break
		}
		if e != nil {
			serr = e
			break
		}
		nodes = append(nodes, n)
	}
	dumps := []obj{}
	for _, n := range nodes {
		dumps = append(dumps, dumpNode(n))
	}
	var streamOut interface{}
	if out, e := encodeAll(nodes, 0); e == nil {
		streamOut = b64(out)
	}
	g = append(g, kv{"stream", obj{
		{"nodes", dumps},
		{"error", errText(serr)},
		{"marshal_b64", streamOut},
	}})

	var v interface{}
	var value, valueErr interface{}
	if verr := protect(func() error { return yaml.Unmarshal(in, &v) }); verr != nil {
		valueErr = text(verr.Error())
	} else {
		value = dumpValue(v)
	}
	g = append(g, kv{"value", value}, kv{"value_error", valueErr})
	return g
}

// encodeAll encodes nodes with one Encoder (indent 0 = the default) and
// closes it.
func encodeAll(nodes []*yaml.Node, indent int) ([]byte, error) {
	var buf bytes.Buffer
	err := protect(func() error {
		enc := yaml.NewEncoder(&buf)
		if indent != 0 {
			enc.SetIndent(indent)
		}
		for _, n := range nodes {
			if err := enc.Encode(n); err != nil {
				return err
			}
		}
		return enc.Close()
	})
	return buf.Bytes(), err
}

// ---- Serde goldens ----

func serdeGolden(s shape, c shapeCase) obj {
	v := s.newValue()
	var input, decodeErr interface{}
	if c.input != nil {
		input = b64([]byte(*c.input))
		decodeErr = errText(protect(func() error { return yaml.Unmarshal([]byte(*c.input), v) }))
	}
	var marshalB64, marshalErr interface{}
	var out []byte
	if err := protect(func() (e error) { out, e = yaml.Marshal(v); return e }); err != nil {
		marshalErr = text(err.Error())
	} else {
		marshalB64 = b64(out)
	}
	var node yaml.Node
	var nodeDump interface{}
	if err := protect(func() error { return node.Encode(v) }); err == nil {
		nodeDump = dumpNode(&node)
	}
	return obj{
		{"shape", s.name},
		{"case", c.name},
		{"input_b64", input},
		{"decode_error", decodeErr},
		{"marshal_b64", marshalB64},
		{"marshal_error", marshalErr},
		{"node", nodeDump},
	}
}

// ---- Differential corpus (spec R12; SCENARIO.md "Differential") ----

const diffSpecial = "#:-'\"0123456789?,[]{}&*!|>%@` \n\t."

var diffWords = []string{
	"true", "false", "null", "~", "yes", "no", "on", "off", "1", "-1",
	"0x1F", "0o17", "1e3", "1.5", ".inf", "-.inf", ".nan", "<<", "-", "---",
	"...", "2001-12-14", "2001-12-14t21:59:43.10-05:00", "1:20", "1_000", "", "  ", ":", "#", "'",
	"\"",
}

func diffScalar(r *rand.Rand) string {
	if r.Intn(4) == 0 {
		return diffWords[r.Intn(len(diffWords))]
	}
	b := make([]byte, r.Intn(12))
	for i := range b {
		if r.Intn(2) == 0 {
			b[i] = byte(0x20 + r.Intn(95))
		} else {
			b[i] = diffSpecial[r.Intn(len(diffSpecial))]
		}
	}
	return string(b)
}

func diffTree(r *rand.Rand, scalars []string, depth int) interface{} {
	if depth > 0 && (depth == 3 || r.Intn(3) == 0) {
		return scalars[r.Intn(len(scalars))]
	}
	var width int
	if depth == 0 {
		width = 1 + r.Intn(3)
	} else {
		width = r.Intn(4)
	}
	if r.Intn(2) == 0 {
		seq := []interface{}{}
		for i := 0; i < width; i++ {
			seq = append(seq, diffTree(r, scalars, depth+1))
		}
		return seq
	}
	m := map[string]interface{}{}
	for i := 0; i < width; i++ {
		k := scalars[r.Intn(len(scalars))]
		m[k] = diffTree(r, scalars, depth+1)
	}
	return m
}

// diffValues is one seed's 20 values: 16 scalar strings, then 4 trees.
func diffValues(seed int64) []interface{} {
	r := rand.New(rand.NewSource(seed))
	scalars := make([]string, 16)
	for i := range scalars {
		scalars[i] = diffScalar(r)
	}
	values := make([]interface{}, 0, 20)
	for _, s := range scalars {
		values = append(values, s)
	}
	for i := 0; i < 4; i++ {
		values = append(values, diffTree(r, scalars, 0))
	}
	return values
}

// ---- FormatFloat table (spec R9) ----

func floatTable() []obj {
	var table []obj
	add64 := func(f float64) {
		table = append(table, obj{
			{"bits", 64},
			{"x", fmt.Sprintf("0x%016x", math.Float64bits(f))},
			{"out", strconv.FormatFloat(f, 'g', -1, 64)},
		})
	}
	add32 := func(f float32) {
		table = append(table, obj{
			{"bits", 32},
			{"x", fmt.Sprintf("0x%08x", math.Float32bits(f))},
			{"out", strconv.FormatFloat(float64(f), 'g', -1, 32)},
		})
	}
	negZero := math.Copysign(0, -1)
	fixed := []float64{
		0, negZero, math.Inf(1), math.Inf(-1), math.NaN(), math.Float64frombits(0xfff8000000000000),
		1e-5, 1e-4, 1e5, 1e6, 1e20, 1e21, 1e23, 0.1, 0.2, 0.3, 1.0 / 3, 2.0 / 3, 1.5, -2.25, 2.5,
		100000, 123456, 999999, 999999.9, 1234567, 0.000123, 0.0001234, 9.999e-5,
		math.MaxFloat64, -math.MaxFloat64, math.SmallestNonzeroFloat64,
		2.2250738585072014e-308, math.Float64frombits(0x000fffffffffffff), math.Float64frombits(0x0000000000000002),
		141016749166520.625, -783388490710125.25, 9007199254740993, 1 << 53, 1 << 62, 123456789012345678,
		0.7, 3.4028234663852886e38, 1e-7, 6.8523015e+5, 5e-324,
	}
	for _, f := range fixed {
		add64(f)
	}
	fixed32 := []float32{
		0, float32(negZero), float32(math.Inf(1)), float32(math.Inf(-1)), float32(math.NaN()),
		0.7, 3.4028235e38, -3.4028235e38, math.SmallestNonzeroFloat32, 1.17549435e-38,
		math.Float32frombits(0x007fffff), math.Float32frombits(0x00000002),
		1e-5, 1e-4, 1e5, 1e6, 1e20, 1e21, 0.1, 0.2, 0.3, 1.0 / 3, 2.0 / 3, 1.5, -2.25, 2.5,
		100000, 123456, 999999, 1234567, 16777216, 16777217, 0.000123, 1e-7, 1e-45, 33554436,
	}
	for _, f := range fixed32 {
		add32(f)
	}
	// Powers of ten around the %e/%f switch, and their neighbours.
	for e := -8; e <= 22; e++ {
		p := math.Pow(10, float64(e))
		for _, f := range []float64{p, 1.5 * p, math.Nextafter(p, 0), math.Nextafter(p, math.Inf(1))} {
			add64(f)
			f32 := float32(f)
			add32(f32)
			add32(math.Nextafter32(f32, 0))
		}
	}
	// Every power of two: the round-trip interval is asymmetric there, so the
	// nearest decimal with the shortest digit count can fall outside it
	// (2^-1017 is 7.120236347223045e-307, not ...044e-307).
	for e := -1074; e <= 1023; e++ {
		add64(math.Ldexp(1, e))
	}
	for e := -149; e <= 127; e++ {
		add32(float32(math.Ldexp(1, e)))
	}
	r := rand.New(rand.NewSource(20261112))
	for i := 0; i < 300; i++ {
		add64(math.Float64frombits(r.Uint64()))
	}
	for i := 0; i < 300; i++ {
		add32(math.Float32frombits(r.Uint32()))
	}
	// Short decimals, as configs and recordings hold them.
	for i := 0; i < 100; i++ {
		f := float64(r.Int63n(2000001)-1000000) / math.Pow(10, float64(r.Intn(12)))
		add64(f)
		add32(float32(f))
	}
	// Binary fractions in [2^49, 2^50): 18 significant decimal digits, so
	// the shortest form rounds, sometimes at an exact tie.
	for i := 0; i < 100; i++ {
		add64(float64(r.Int63n(1<<52)+(1<<52)) / 8)
	}
	return table
}
