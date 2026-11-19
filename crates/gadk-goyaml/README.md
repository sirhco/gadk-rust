# gadk-goyaml

Internal crate (`publish = false`): a pure-Rust YAML library that reproduces
[`gopkg.in/yaml.v3`](https://github.com/go-yaml/yaml) v3.0.1 byte for byte, so gadk reads and
writes YAML exactly as ADK does. Its only dependent is the internal `gadk-configurable`
(configurable agents and the conformance plugins), and the `gadk` façade never re-exports it.

- Module layout: `yamlh.rs` (token, event and parser/emitter types), `api.rs` (parser and emitter
  construction and handlers), `reader.rs`, `scanner.rs`, `parser.rs`, `emitter.rs` and
  `writer.rs` (the libyaml-derived pipeline), `resolve.rs` (scalar tag resolution), `sorter.rs`
  (key ordering), `encode.rs` (the node encoder), `node.rs` (`Node` and the composer),
  `de.rs` / `value.rs` (decoding), `ser.rs` (encoding of serde values), `lib.rs` (the public
  API).
- Public API: `unmarshal_node` / `marshal_node` over the `Document` node arena, serde
  `unmarshal` / `marshal` / `from_node` / `from_node_partial` / `to_node` with `Options` key
  naming, `Encoder` / `Decoder`, `GoValue` (a dynamically typed YAML value), `InlineMap` (a catch-all
  map for the keys no other field matched).
- A struct variant's keys are looked up as `Options::key_for(<variant name>, field)` on both
  encode and decode. Untagged, internally tagged and adjacently tagged enums decode through
  serde's `deserialize_any` (raw YAML keys, no `key_for`), so their struct variants round-trip
  only when the field names already are yaml.v3's default keys (lowercase, no `_`) or the variant is in
  `verbatim_structs`; prefer externally tagged enums.
- Node trees, error texts and emitted bytes match yaml.v3 v3.0.1 except for the accepted
  deviations, each explained in a comment in the source. Error texts name Rust types where
  yaml.v3 names Go ones: type errors say `into i64`, `into String`, `into Vec<u8>`, the serde
  struct name, `into a sequence` or `into a mapping`, and `invalid map key` shows the key as
  `["a", "b"]` / `{"k": null}`.

## Nesting depth

Parsing accepts yaml.v3's limit (`exceeded max depth of 10000`): the composer runs on an
explicit stack. Every recursive walk — `marshal_node` / `Encoder::encode_node`, the serde
`Serializer` and `Deserializer`, the `GoValue` decoder and encoder — stops past 127 nested
collections (alias expansions count as a level on decode) with
`yaml: exceeded max depth of 127` instead of overflowing the thread's stack. yaml.v3 has no
such limit below 10000; this is the workspace-wide 127-level nesting policy (serde_json's limit)
and a deliberate deviation.

## Tests and goldens

- Unit tests per module, and ports of yaml.v3's test files in `src/parity_tests/`.
- `tests/` replays the goldens in `testdata/golden/` (parse, emit, `interface{}` values, serde
  encode/decode/round trip, and the seeded differential corpus).
- The goldens were recorded from yaml.v3 v3.0.1 when the crate was written; they are
  committed and not regenerated. The `decode_error` / `value_error` texts that named Go types
  were rewritten once to the Rust names above.

## License

`Apache-2.0 AND MIT`: the files derived from yaml.v3's libyaml-based sources
(`yamlh.rs`, `api.rs`, `reader.rs`, `scanner.rs`, `parser.rs`, `emitter.rs`, `writer.rs`) keep
libyaml's MIT license; the rest is Apache-2.0. yaml.v3's license and notice are in
`LICENSE-yaml-v3` and `NOTICE-yaml-v3`.
