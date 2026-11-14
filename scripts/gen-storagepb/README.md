# gen-storagepb

Regenerates `crates/gadk-agentanalytics/src/storagepb/google.*.rs`, the prost messages and tonic
stubs of the BigQuery Storage API (`google.cloud.bigquery.storage.v1`), from the protos vendored
in `crates/gadk-agentanalytics/proto` (sources and sha256s in `proto/SOURCES`).

```sh
scripts/gen-storagepb/run.sh            # rewrite the checked-in files
scripts/gen-storagepb/run.sh /tmp/out   # write elsewhere, e.g. to diff against the checked-in files
```

This is a standalone Cargo project (its own `[workspace]` table and `Cargo.lock`), so
`tonic-prost-build`, `prost-build` and `protoc-bin-vendored` never enter the main workspace. The
versions are pinned exactly because the generated text depends on them; protoc is the binary
shipped by `protoc-bin-vendored` (protoc 36.2), and only the vendored protos are on the include
path.

Output, per package (`compile_well_known_types`, so no `prost-types`):

- `google.cloud.bigquery.storage.v1.rs`: messages plus the `BigQueryWrite` / `BigQueryRead`
  tonic clients (no `connect` helper: `build_transport(false)`);
- `google.cloud.bigquery.storage.v1.server.rs`: the tonic servers, split out so the
  hand-written `storagepb/mod.rs` can include them only under
  `cfg(any(test, feature = "testutil"))`;
- `google.protobuf.rs`, `google.rpc.rs`: the well-known and `google.rpc.Status` types used.

The `google.api` protos are imported only for annotations, which prost does not model; they are
dropped from the descriptor set before generation. Each file starts with a header naming the
generator versions and the googleapis / protobuf commits; `mod.rs` is never touched.

To move to a newer googleapis commit: re-fetch the files listed in `proto/SOURCES` at the new
commit (plus any new imports), update `SOURCES`, `GOOGLEAPIS_COMMIT` in `src/main.rs` and
`NOTICE` if needed, then run `run.sh` and build `gadk-agentanalytics`.
