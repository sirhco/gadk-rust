# gen-go-goyaml scenario

`scripts/gen-go-goyaml` records how `gopkg.in/yaml.v3` v3.0.1 (the version ADK-Go pins, the
probe's only dependency) parses, decodes and emits YAML, as JSON goldens for `gadk-goyaml`
(sub-project 6c-1, spec R9, R11, R12), plus Go's `strconv.FormatFloat(f, 'g', -1, 32|64)` table
for `gadk_platform::go_format_float_g`.

Regenerate:

```
cd scripts/gen-go-goyaml && go run .
```

It prints
`wrote corpus/parse=625 golden/parse=625 golden/diff=640 golden/serde=77 to ../../crates/gadk-goyaml/testdata; float=3729 to ../../crates/gadk-platform/tests/testdata/go_format_float_g.json`.
Flags: `-testdata DIR` (default `../../crates/gadk-goyaml/testdata`; reads
`DIR/corpus/differential.json`, deletes and rewrites `DIR/corpus/parse/` and `DIR/golden/`) and
`-float FILE`. The run sets `TZ=UTC`, uses no clock, network or host state, and fixed seeds, so
run to run every output is byte-identical; `scripts/check-fixtures.sh` reruns it into a temp dir
and requires an empty `diff -r`. yaml.v3's test tables are read from its module source:
`$GOYAML_DIR`, else `$(go env GOMODCACHE)/gopkg.in/yaml.v3@v3.0.1` (present once `go run` has
built the probe).

## Outputs

All JSON: 2-space indent, keys in the order below, no HTML escaping, trailing newline, nesting
at most 127 (the probe refuses deeper output, so serde_json's default recursion limit loads every
file). Byte strings are standard base64 (`*_b64`); absent is `null`.

- `crates/gadk-goyaml/testdata/corpus/parse/<case>.yaml` — 625 parse inputs (written by the
  probe from the tables below; never edited by hand).
- `crates/gadk-goyaml/testdata/golden/parse/<case>.json` — 625, one per input.
- `crates/gadk-goyaml/testdata/golden/diff/<seed>-<n>.json` — 640 (32 seeds × 20).
- `crates/gadk-goyaml/testdata/golden/serde/<shape>-<case>.json` — 77.
- `crates/gadk-platform/tests/testdata/go_format_float_g.json` — 3729 rows.

### Node dump

`{"kind", "style", "tag", "value", "anchor", "alias", "head", "line_comment", "foot", "line",
"column", "content"}`: `kind` is `document|sequence|mapping|scalar|alias`; `style` lists the set
`yaml.Style` flags in the order `tagged, double, single, literal, folded, flow`; `alias` is
`null` or the index path (into `content` arrays) from the dumped root to the first node in
pre-order whose pointer is the alias target; `content` holds the children's dumps. An alias may
point at an ancestor (`edge-anchor-self-map`: `Unmarshal` into a `yaml.Node` accepts it).

### Value dump

The Go `interface{}` `yaml.Unmarshal` produces: `{"t":"null"}`, `{"t":"bool","v":b}`,
`{"t":"int","v":"<decimal>"}` (`int`), `{"t":"uint","v":"<decimal>"}` (`uint64`),
`{"t":"float","v":"<FormatFloat(v,'g',-1,64)>"}`, `{"t":"string","v":s}` (valid UTF-8) or
`{"t":"bytes","b64":…}` (a Go string that is not UTF-8, from `!!binary`),
`{"t":"timestamp","v":"<RFC3339Nano>"}`, `{"t":"seq","v":[…]}`,
`{"t":"map","v":[[key, dump]…]}` (`map[string]interface{}`, sorted by key) and
`{"t":"mapany","v":[[keydump, dump]…]}` (`map[interface{}]interface{}`, sorted by the compact
JSON text of the key dump).

### Parse golden

`{"case", "input_b64", "node", "error", "marshal_b64", "marshal_error", "indent2_b64",
"stream": {"nodes", "error", "marshal_b64"}, "value", "value_error"}`:

- `node`/`error`: `var node yaml.Node; err := yaml.Unmarshal(in, &node)`; `node` is the
  `DocumentNode` dump, `null` when `Unmarshal` leaves the node zero (empty or comment-only input)
  or fails.
- `marshal_b64`/`marshal_error`: `yaml.Marshal(&node)`, only when `Unmarshal` succeeded.
- `indent2_b64`: an `Encoder` with `SetIndent(2)` encoding `&node`, then `Close`; only when
  `Unmarshal` succeeded and the encoder did not fail.
- `stream`: a `Decoder` over the input decoding `yaml.Node`s until `io.EOF` (at most 1000);
  `nodes` are the decoded documents, `error` the first non-EOF error (decoding stops there);
  `marshal_b64` is one `Encoder` (default indent) encoding every decoded node, then `Close`
  (`null` if that fails).
- `value`/`value_error`: `var v interface{}; yaml.Unmarshal(in, &v)`.

A yaml.v3 call that panics with a non-yaml panic would be recorded as an error text
`panic: <value>` and reported on stderr; v3.0.1 panics on no corpus input.

### Differential golden

Same keys as a parse golden with `"source"` (the value dump of the generated Go value) inserted
after `input_b64`; the input is `yaml.Marshal` of the generated value; case name `<seed>-<n>`.

### Serde golden

`{"shape", "case", "input_b64", "decode_error", "marshal_b64", "marshal_error", "node"}`:
`v := new(Shape)`; when the case has an input, `decode_error` is
`yaml.Unmarshal(input, v)`'s error (`v` keeps whatever Go filled in, as Go returns it alongside a
`*TypeError`); then `marshal_b64` = `yaml.Marshal(v)` and `node` = the dump of the node
`node.Encode(v)` fills (not a document node; `textless`, so lines and columns are 0). Input
`null` is the shape's zero value.

### FormatFloat table

`[{"bits": 32|64, "x": "0x%08x" | "0x%016x" of the IEEE bits, "out": FormatFloat(f, 'g', -1, bits)}…]`
(32-bit rows format `float64(f32)`). Rows, in order: fixed 64-bit values (±0, ±Inf, two NaNs,
the powers of ten around the `%e` switch, min/max, subnormals, ties), fixed 32-bit values; for
each power of ten 1e-8…1e22 the value, ×1.5 and both neighbours (64-bit) and its float32 and the
float32 below it; every power of two (2^-1074…2^1023 and, 32-bit, 2^-149…2^127, where the
nearest decimal at the shortest length can fall outside the round-trip interval); 300 random
64-bit and 300 random 32-bit bit patterns (`rand.NewSource(20261112)`); 100 short decimals at both
sizes; 100 binary fractions in [2^49, 2^50) that end in exact decimal ties.

## Parse corpus (625)

Case names: `<prefix>-<table>-<NNN>` for table rows (NNN = row index, three digits),
`<prefix>-<var>` for string vars, then the hand-written `edge-*`, `limit-*`, `alias-*` and
`recordings-*` cases from `edge.go`.

From yaml.v3's own test files (read with `go/ast`, never retyped; string constants, `+` and
`strings.Repeat` are evaluated):

| Source | Cases |
|---|---|
| `decode_test.go` `unmarshalTests` (`data`) | 172 |
| `decode_test.go` `decoderTests` (`data`) | 4 |
| `decode_test.go` `unmarshalErrorTests` (`data`) | 17 |
| `decode_test.go` `unmarshalerTests` (`data`) | 8 |
| `decode_test.go` `mergeTests`, `mergeTestsNested` (string vars) | 2 |
| `decode_test.go` `unmarshalNullTests` (`input`) | 9 |
| `decode_test.go` `unmarshalStrictTests` (`data`) | 6 |
| `decode_test.go` `TestFuzzCrashers` local `cases` | 15 |
| `encode_test.go` `marshalTests` (`data`, the expected YAML) | 88 |
| `encode_test.go` `marshalerTests` (`data`) | 5 |
| `node_test.go` `nodeTests` (`yaml`, `[encode]`/`[decode]` marker stripped as `TestNodeRoundtrip` does) | 74 |
| `node_test.go` `setStringTests` (`yaml`) | 6 |
| `node_test.go` `nodeEncodeDecodeTests` (`yaml`) | 5 |
| `example_embedded_test.go` `data` | 1 |

`limit_test.go`'s `limitTests` are 0.5–1 MB inputs; the corpus has reduced variants instead
(below). `marshalErrorTests` hold Go values, not YAML, and are ported by Task 11 directly.

Hand-written (`edge.go`), 213 cases:

- `edge-plain-NN` (65): `k: <s>` for each quoting-sensitive string `s` (booleans and nulls in
  every case, YAML 1.1 bools, leading/trailing spaces, indicators, `#`, `: `, numbers in every
  base, floats, `.inf`/`.nan`, timestamps, base-60, `<<`, document markers, quotes, backslash,
  tab); `edge-quote-{double,single,flow,keys}` (all of them double- and single-quoted, in flow,
  as keys).
- `edge-multiline-*` (8): literal/folded with chomping and indentation indicators, multi-line
  plain/double/single, trailing spaces, block scalars in sequences and as a document.
- `edge-unicode*`, `edge-utf16{le,be}-bom` (6): non-ASCII text, every escape, BOM, NEL/LS/PS,
  UTF-16 input.
- `edge-nonutf8-*`, `edge-control-*`, `edge-binary-*` (10): invalid UTF-8 (reader errors),
  control characters, `!!binary` decoding to non-UTF-8 and UTF-8 bytes, bad base64.
- `edge-long-*` (7): 10 kB lines in every scalar style and in flow (default width is unlimited;
  `indent2_b64` shows the `Encoder` output is width-independent), simple-key length 1000 vs 1100.
- `edge-comment-*` (14): head/line/foot comments on documents, keys, values, sequences, flow
  collections, block-scalar indicators, document markers, nested foot comments, blank lines.
- `edge-anchor-*`, `edge-merge-*`, `edge-alias-in-seq` (16): anchors on scalars, collections,
  keys and documents, redefinition, self-containing anchors in maps and sequences, unknown
  aliases, merges (single, list, inline, invalid, scalar, `!!merge`-tagged).
- `edge-flow-*`, `edge-block-*`, `edge-complex-key` (15): nesting, trailing commas, single-pair
  flow mappings, explicit keys, multi-line flow, empty values, unindented sequences, depth 20,
  indent 3.
- `edge-stream-*`, `edge-doc-*` (10): multi-document streams, `...`, empty documents,
  `%YAML`/`%TAG` directives, an error in the second document, comments between documents.
- `edge-tag*`, `edge-ints`, `edge-floats`, `edge-bools-nulls`, `edge-timestamps` (9): explicit
  tags (good and bad), int/uint overflow boundaries, float forms incl. base-60 and `1e400`, the
  resolve table, timestamp forms.
- `edge-crlf`, `edge-cr-only`, `edge-no-final-newline`, `edge-trailing-spaces`,
  `edge-only-whitespace`, `edge-empty` (6).
- `edge-duplicate-key*`, `edge-mixed-keys`, `edge-null-key-only` (4).
- `edge-invalid-*` (25): scanner and parser errors (tabs, unclosed quotes and flow collections,
  bad indentation, nested mapping on one line, bad escapes, reserved indicators, directive errors,
  empty anchors/aliases, block in flow, bad block-scalar indicator, empty verbatim tag, content
  after `...`).
- `limit-*` (5): `limit-1kb-of-maps` (the `1kb of maps` row verbatim), the depth limits one past
  10000 (`limit-deep-{slices,maps,indents}-10001`) and `limit-100-aliases` (the 100-aliases row
  with a 31-element list).
- `alias-wWW-lLL` (7): a ladder of `LL` levels, each a flow sequence of `WW` aliases to the level
  below, on both sides of `decode.go`'s `allowedAliasRatio` (`document contains excessive
  aliasing` from `Unmarshal` into `interface{}`; a `yaml.Node` parse never expands aliases).
- `recordings-llm`, `recordings-tools` (2): conformance-style `recordings:` files shaped like
  ADK-Go's record plugin output (LLM and tool recordings, nested contents/parts, function
  calls, usage metadata, a block scalar, flow collections, comments, `~`, a long line).

## Differential corpus (640)

`crates/gadk-goyaml/testdata/corpus/differential.json` holds the 32 seeds (`{"seeds": [1 … 32]}`;
the probe requires exactly 32). For each seed, `r := rand.New(rand.NewSource(seed))` draws, in
order:

1. 16 scalar strings (`diffScalar`): with probability 1/4 a word from `diffWords` (YAML-special
   words: bools, nulls, numbers in every base, `.inf`, `<<`, document markers, timestamps, `""`,
   spaces, `:`, `#`, quotes), else 0–11 bytes, each printable ASCII (1/2) or one of
   ``#:-'"0123456789?,[]{}&*!|>%@` `` plus space, `\n`, `\t`, `.` (1/2);
2. 4 trees (`diffTree`) of those scalars: a root collection of width 1–3, inner collections of
   width 0–3, depth ≤ 3 (level 3 is always a scalar; below that, a scalar with probability 1/3),
   sequences and `map[string]interface{}` with probability 1/2 each.

Case `<seed>-<n>`: n = 0–15 the scalars, 16–19 the trees. The golden's input is
`yaml.Marshal(value)`; everything else is recorded as for a parse golden. yaml.v3 v3.0.1 cannot
re-parse 5 of its own outputs (`17-1`, `23-19`, `25-16`, `5-16`, `9-16`: literal block scalars
whose text starts with a space or tab, emitted with an indentation indicator such as `|4-` but
not indented to match); those goldens carry the parse error, which the port must reproduce.

## Serde shapes (77)

`shapes.go` holds the Go structs, each field tagged `yaml:"<key>[,omitempty]"` as spec R6
derives it from the Rust mirror in `crates/gadk-goyaml/tests/support/shapes.rs` (Task 5); sized
integer types throughout so Go's `TypeError` type names (`int8`, `uint64`, …) are the ones the
serde entry points give. Every shape's first case is `zero` (no input: the zero value is
encoded).

| Shape | Go type | Covers | Cases |
|---|---|---|---|
| `scalars` | `Scalars` | bool, int8…int64, uint8…uint64, float32, float64, string: conversions, overflow, mismatches, aliases, merges, duplicate keys, non-mapping roots, syntax error | 30 |
| `nested` | `Nested` (`Inner`) | struct, `*Inner` with omitempty, `[]Inner`, nulls, aliases to structs | 8 |
| `optionals` | `Optionals` | pointers (Rust `Option`) with and without omitempty | 6 |
| `collections` | `Collections` | `[]byte` (int sequence / `!!binary`), slices, `map[string]int64`, `map[string]interface{}`, `[]interface{}`, sorted map keys | 11 |
| `inline` | `WithInline` | `,inline` `map[string]interface{}` catch-all (Rust `InlineMap`) | 8 |
| `node` | `WithNode` | `yaml.Node` and `[]yaml.Node` fields (Rust `Document`, `Vec<Document>`) | 6 |
| `godefaults` | `GoDefaults` | untagged fields (Go's lowercased key), `yaml:"-"` | 4 |
| `recordings` | `Recordings` (`Recording`, `LLMRecording`, `ToolRecording`) | the recordings files decoded into ADK-Go-like recording structs and re-encoded | 4 |
