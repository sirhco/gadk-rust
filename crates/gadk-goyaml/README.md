# gadk-goyaml

Internal crate (`publish = false`): a pure-Rust port of
[`gopkg.in/yaml.v3`](https://github.com/go-yaml/yaml) v3.0.1, the YAML library ADK-Go pins, so
gadk reads and writes YAML byte-for-byte as ADK-Go does (sub-project 6c-1).

- One Rust module per Go file, Go's function names in snake_case and in Go's order:
  `yamlh.rs` (`yamlh.go` + `yamlprivateh.go`), `api.rs` (`apic.go`), `reader.rs`, `scanner.rs`,
  `parser.rs`, `emitter.rs`, `writer.rs` (the libyaml-derived `*c.go` files), `resolve.rs`,
  `sorter.rs`, `encode.rs`, `node.rs` (`Node` and the `decode.go` composer), `de.rs` / `value.rs`
  (`decode.go`), `ser.rs` (`encode.go`'s reflection path), `lib.rs` (`yaml.go`).
- Public API: `unmarshal_node` / `marshal_node` over the `Document` node arena, serde
  `unmarshal` / `marshal` / `from_node` / `to_node` with `Options` key naming, `Encoder` /
  `Decoder`, `GoValue`, `InlineMap`.
- Tests: unit tests per module, ports of yaml.v3's test files in `src/parity_tests/`
  (ledger `docs/parity/6c-1.md`), and replays of the Go probe's goldens
  (`scripts/gen-go-goyaml`, `testdata/golden/`) in `tests/`.

## License

`Apache-2.0 AND MIT`: the files ported from yaml.v3's libyaml-derived Go files
(`yamlh.rs`, `api.rs`, `reader.rs`, `scanner.rs`, `parser.rs`, `emitter.rs`, `writer.rs`) keep
libyaml's MIT license; the rest is Apache-2.0. yaml.v3's license and notice are in
`LICENSE-yaml-v3` and `NOTICE-yaml-v3`.
