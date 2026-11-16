# gadk-goyaml

Internal crate (`publish = false`): a pure-Rust port of
[`gopkg.in/yaml.v3`](https://github.com/go-yaml/yaml) v3.0.1, the YAML library ADK-Go pins, so
gadk reads and writes YAML byte-for-byte as ADK-Go does (sub-project 6c-1). Its only dependent is
the internal `gadk-configurable` (6c-2: configurable agents and the conformance plugins), and the
`gadk` façade never re-exports it.

- One Rust module per Go file, Go's function names in snake_case and in Go's order:
  `yamlh.rs` (`yamlh.go` + `yamlprivateh.go`), `api.rs` (`apic.go`), `reader.rs`, `scanner.rs`,
  `parser.rs`, `emitter.rs`, `writer.rs` (the libyaml-derived `*c.go` files), `resolve.rs`,
  `sorter.rs`, `encode.rs`, `node.rs` (`Node` and the `decode.go` composer), `de.rs` / `value.rs`
  (`decode.go`), `ser.rs` (`encode.go`'s reflection path), `lib.rs` (`yaml.go`).
- Public API: `unmarshal_node` / `marshal_node` over the `Document` node arena, serde
  `unmarshal` / `marshal` / `from_node` / `from_node_partial` / `to_node` with `Options` key
  naming, `Encoder` / `Decoder`, `GoValue` (Go's `interface{}`), `InlineMap` (Go's `,inline`
  map).
- A struct variant's keys are looked up as `Options::key_for(<variant name>, field)` on both
  encode and decode. Untagged, internally tagged and adjacently tagged enums decode through
  serde's `deserialize_any` (raw YAML keys, no `key_for`), so their struct variants round-trip
  only when the field names already are Go-default keys (lowercase, no `_`) or the variant is in
  `verbatim_structs`; prefer externally tagged enums.
- Node trees, error texts and emitted bytes match yaml.v3 v3.0.1 except for the accepted
  divergences, each annotated `// Divergence (ledger 6c-1)` in the source.

## Nesting depth

Parsing accepts yaml.v3's limit (`exceeded max depth of 10000`): the composer runs on an
explicit stack. Every recursive walk — `marshal_node` / `Encoder::encode_node`, the serde
`Serializer` and `Deserializer`, the `GoValue` decoder and encoder — stops past 127 nested
collections (alias expansions count as a level on decode) with
`yaml: exceeded max depth of 127` instead of overflowing the thread's stack. Go has no such
limit below 10000; this is the workspace-wide 127-level nesting policy (serde_json's limit) and
a ledgered divergence.

## Tests and goldens

- Unit tests per module, and ports of yaml.v3's test files in `src/parity_tests/` (6c-1).
- `tests/` replays the goldens in `testdata/golden/` (parse, emit, `interface{}` values, serde
  encode/decode/round trip, and the seeded differential corpus).
- The goldens were recorded from yaml.v3 v3.0.1 by a Go probe during the port; they are
  committed and not regenerated.

## License

`Apache-2.0 AND MIT`: the files ported from yaml.v3's libyaml-derived Go files
(`yamlh.rs`, `api.rs`, `reader.rs`, `scanner.rs`, `parser.rs`, `emitter.rs`, `writer.rs`) keep
libyaml's MIT license; the rest is Apache-2.0. yaml.v3's license and notice are in
`LICENSE-yaml-v3` and `NOTICE-yaml-v3`.
