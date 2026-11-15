#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# R20 (1b) / R17 (2c) / R18 (3a): every httprr fixture copied under
# crates/gadk-engine/tests/testdata/, crates/gadk/tests/testdata/,
# crates/gadk-anthropic/tests/testdata/ or crates/gadk-a2a/tests/testdata/ (5b) is byte-identical
# to its ADK-Go original (same relative path under $ADK_GO_DIR), and there are exactly 48 of them
# (30 + 11 + 3 + 4).
# 5a: the telemetry-schema goldens copied from ADK-Go's
# internal/telemetry/functionaltest/testdata/ are byte-identical to it (80), and the wire probe
# (scripts/gen-go-adkrest) wrote exactly 82 fixtures under crates/gadk-server/testdata/wire/
# (main 67, strict 11, custom 4). crates/gadk-server/testdata/oidc/ holds test-only keys, not
# fixtures, and is not counted.
# 5b: the A2A wire probe (scripts/gen-go-a2a) wrote exactly 7 fixtures under
# crates/gadk-a2a/testdata/wire/; the replay's declared deltas (*.deltas.json) are not counted.
# 6: the vendored web UI bundle crates/gadk-launcher/webui/distr/ holds exactly 106 files, they
# match crates/gadk-launcher/webui/SHA256SUMS (106 lines), and, when ADK-Go is present, they are
# byte-identical to its cmd/launcher/web/webui/distr/ (scripts/sync-webui regenerates all three);
# the launcher probe (scripts/gen-go-launcher) wrote exactly 70 web, 15 console and 34 CLI fixtures
# under crates/gadk-launcher/testdata/{wire,console,cli}/; the replay's declared deltas
# (*.deltas.json) are not counted.
# 5c: the Agent Engine probe (scripts/gen-go-agentengine) wrote exactly 30 server fixtures under
# crates/gadk-server/testdata/agentengine/ and 13 launcher fixtures under
# crates/gadk-launcher/testdata/agentengine/ (declared deltas, *.deltas.json, not counted); when
# go and ADK-Go are present, its --shapes generator reproduces
# crates/gadk-server/src/agentengine/encode_shapes.rs byte for byte.
# 6c-1: the yaml.v3 probe (scripts/gen-go-goyaml) wrote exactly 625 parse inputs under
# crates/gadk-goyaml/testdata/corpus/parse/ and 625 parse, 640 diff and 77 serde goldens under
# crates/gadk-goyaml/testdata/golden/{parse,diff,serde}/ (1342 goldens), and, counted separately,
# the 3729-row strconv.FormatFloat table crates/gadk-platform/tests/testdata/go_format_float_g.json;
# when go is present, rerunning the probe into a temp dir reproduces all of them (empty diff -r).
# 6c-2: the configurable probe (scripts/gen-go-configurable) wrote exactly 44 recording cases under
# crates/gadk-configurable/tests/fixtures/recordings/<case>/ (44 input.json, 44 replay.json, 44
# generated-recordings[-sse].yaml, 6 existing.yaml), fixups.json and the 40-row
# canonicalize.json; when go and ADK-Go are present, rerunning the probe into a temp dir reproduces
# all of them (empty diff -r) and every declaration the probe vendors from ADK-Go's internal/
# packages is byte-identical to ADK-Go's (the probe's -vendored adk and -vendored probe outputs
# match). The 2 Go-written integration goldens under
# crates/gadk-configurable/tests/fixtures/integration/ (an overlay probe in ADK-Go's conformance
# package, not part of gen-go-configurable: scripts/gen-go-configurable/overlay/
# zz_t12probe_test.go.txt, whose header has the regeneration command) are counted and left out of
# the rerun diff.
# 7b-1/7b-2/7b-3a: the agentanalytics probe (scripts/gen-go-agentanalytics: an overlay test run in
# a temp copy of ADK-Go's plugin/agentanalytics) wrote exactly 140 fixtures under
# crates/gadk-agentanalytics/tests/fixtures/ (7b-1: arrow 9, config 3, formatter 8, redaction 2,
# schema 3, views 2, wire 41; 7b-2: append_rows 12, config +1, gapic 3, gcs 12, helpers 11,
# record_batch 27; 7b-3a: plugin 6); when go and ADK-Go are present, rerunning it into a temp dir
# reproduces them (empty diff -r).
# 7b-2: the generated crates/gadk-agentanalytics/src/storagepb/google.*.rs are what
# scripts/gen-storagepb/run.sh writes from the vendored protos (empty diff -r into a temp dir,
# the hand-written mod.rs aside). The generator builds with its own Cargo.lock into
# $GEN_STORAGEPB_TARGET_DIR (default: target/gen-storagepb in the repo); the check is skipped with
# a note when cargo is missing or the generator's crates cannot be fetched (offline).
# 8a: every probe was re-run at ADK-Go 0990f9e2 (docs/parity/ADK_GO_COMMIT); no count changed
# (the changed goldens and their causes are in docs/parity/8a-bug-report.md, "Go output changed"), and
# encode_shapes.rs grew from 78 structs / 349 fields to 88 / 435.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
go="${ADK_GO_DIR:-$repo/../adk-go}"
bases=("$repo/crates/gadk-engine/tests/testdata" "$repo/crates/gadk/tests/testdata"
  "$repo/crates/gadk-anthropic/tests/testdata" "$repo/crates/gadk-a2a/tests/testdata")
expected=48
n=0
fail=0
for base in "${bases[@]}"; do
  while IFS= read -r -d '' f; do
    rel="${f#"$base"/}"
    if ! cmp -s "$f" "$go/$rel"; then
      echo "fixture differs from ADK-Go (or is missing there): $rel" >&2
      fail=1
    fi
    n=$((n + 1))
  done < <(find "$base" -type f -name '*.httprr' -print0)
done
if [ "$n" -ne "$expected" ]; then
  echo "expected $expected fixtures under ${bases[*]}, found $n" >&2
  fail=1
fi

schema="$repo/crates/gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema/testdata"
schema_go="$go/internal/telemetry/functionaltest/testdata"
schema_expected=80
schema_n=0
while IFS= read -r -d '' f; do
  rel="${f#"$schema"/}"
  if ! cmp -s "$f" "$schema_go/$rel"; then
    echo "telemetry golden differs from ADK-Go (or is missing there): $rel" >&2
    fail=1
  fi
  schema_n=$((schema_n + 1))
done < <(find "$schema" -type f -print0)
if [ "$schema_n" -ne "$schema_expected" ]; then
  echo "expected $schema_expected telemetry goldens under $schema, found $schema_n" >&2
  fail=1
fi

wire="$repo/crates/gadk-server/testdata/wire"
wire_expected=82
wire_n="$(find "$wire" -type f -name '*.json' | wc -l | tr -d ' ')"
if [ "$wire_n" -ne "$wire_expected" ]; then
  echo "expected $wire_expected wire fixtures under $wire, found $wire_n" >&2
  fail=1
fi

a2a_wire="$repo/crates/gadk-a2a/testdata/wire"
a2a_wire_expected=7
a2a_wire_n="$(find "$a2a_wire" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
if [ "$a2a_wire_n" -ne "$a2a_wire_expected" ]; then
  echo "expected $a2a_wire_expected A2A wire fixtures under $a2a_wire, found $a2a_wire_n" >&2
  fail=1
fi

webui="$repo/crates/gadk-launcher/webui"
webui_go="$go/cmd/launcher/web/webui/distr"
webui_expected=106
webui_n="$(find "$webui/distr" -type f | wc -l | tr -d ' ')"
if [ "$webui_n" -ne "$webui_expected" ]; then
  echo "expected $webui_expected web UI files under $webui/distr, found $webui_n" >&2
  fail=1
fi
webui_sums_n="$(wc -l <"$webui/SHA256SUMS" | tr -d ' ')"
if [ "$webui_sums_n" -ne "$webui_expected" ]; then
  echo "expected $webui_expected lines in $webui/SHA256SUMS, found $webui_sums_n" >&2
  fail=1
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256_check=(sha256sum -c)
else
  sha256_check=(shasum -a 256 -c)
fi
if ! webui_sums_out="$(cd "$webui/distr" && "${sha256_check[@]}" ../SHA256SUMS 2>&1)"; then
  printf '%s\n' "$webui_sums_out" | grep -v ': OK$' >&2 || true
  echo "web UI bundle does not match $webui/SHA256SUMS" >&2
  fail=1
fi
if [ -d "$webui_go" ] && ! diff -rq "$webui/distr" "$webui_go" >&2; then
  echo "web UI bundle differs from ADK-Go's $webui_go" >&2
  fail=1
fi

launcher_counts=""
for spec in wire:70 console:15 cli:34; do
  dir="$repo/crates/gadk-launcher/testdata/${spec%%:*}"
  want="${spec#*:}"
  got="$(find "$dir" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want launcher fixtures under $dir, found $got" >&2
    fail=1
  fi
  launcher_counts="$launcher_counts ${spec%%:*}=$got"
done

agentengine_counts=""
for spec in gadk-server:30 gadk-launcher:13; do
  dir="$repo/crates/${spec%%:*}/testdata/agentengine"
  want="${spec#*:}"
  got="$(find "$dir" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want agentengine fixtures under $dir, found $got" >&2
    fail=1
  fi
  agentengine_counts="$agentengine_counts ${spec%%:*}=$got"
done

shapes="$repo/crates/gadk-server/src/agentengine/encode_shapes.rs"
shapes_status="not checked"
if ! command -v go >/dev/null 2>&1 || [ ! -d "$go/server/agentengine" ]; then
  echo "skipping the encode_shapes.rs check: needs go and ADK-Go at $go (set ADK_GO_DIR)" >&2
else
  go_abs="$(cd "$go" && pwd)"
  shapes_tmp="$(mktemp -d)"
  sed "s#=> ../../../adk-go#=> $go_abs#" "$repo/scripts/gen-go-agentengine/go.mod" >"$shapes_tmp/go.mod"
  cp "$repo/scripts/gen-go-agentengine/go.sum" "$shapes_tmp/go.sum"
  if ! (cd "$repo/scripts/gen-go-agentengine" && ADK_GO_DIR="$go_abs" \
    go run -modfile="$shapes_tmp/go.mod" . --shapes --out "$shapes_tmp/encode_shapes.rs" >/dev/null); then
    echo "scripts/gen-go-agentengine --shapes failed" >&2
    fail=1
  elif ! cmp -s "$shapes_tmp/encode_shapes.rs" "$shapes"; then
    diff -u "$shapes" "$shapes_tmp/encode_shapes.rs" | head -40 >&2 || true
    echo "$shapes differs from scripts/gen-go-agentengine --shapes output" >&2
    fail=1
  else
    shapes_status="matches the generator"
  fi
  rm -rf "$shapes_tmp"
fi

goyaml="$repo/crates/gadk-goyaml/testdata"
float_table="$repo/crates/gadk-platform/tests/testdata/go_format_float_g.json"
goyaml_counts=""
for spec in corpus/parse:yaml:625 golden/parse:json:625 golden/diff:json:640 golden/serde:json:77; do
  rest="${spec#*:}"
  dir="$goyaml/${spec%%:*}"
  want="${rest#*:}"
  got="$(find "$dir" -type f -name "*.${rest%%:*}" | wc -l | tr -d ' ')"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want goyaml files under $dir, found $got" >&2
    fail=1
  fi
  goyaml_counts="$goyaml_counts ${spec%%:*}=$got"
done
float_want=3729
float_n="$(grep -c '"bits":' "$float_table" || true)"
if [ "$float_n" -ne "$float_want" ]; then
  echo "expected $float_want rows in $float_table, found $float_n" >&2
  fail=1
fi
goyaml_status="not rerun"
if ! command -v go >/dev/null 2>&1; then
  echo "skipping the gen-go-goyaml rerun: go is not on PATH" >&2
else
  goyaml_tmp="$(mktemp -d)"
  mkdir -p "$goyaml_tmp/corpus"
  cp "$goyaml/corpus/differential.json" "$goyaml_tmp/corpus/"
  if ! (cd "$repo/scripts/gen-go-goyaml" &&
    go run . -testdata "$goyaml_tmp" -float "$goyaml_tmp/go_format_float_g.json" >/dev/null); then
    echo "scripts/gen-go-goyaml failed" >&2
    fail=1
  elif ! diff -r "$goyaml/corpus" "$goyaml_tmp/corpus" >&2 ||
    ! diff -r "$goyaml/golden" "$goyaml_tmp/golden" >&2 ||
    ! diff "$float_table" "$goyaml_tmp/go_format_float_g.json" >&2; then
    echo "gen-go-goyaml output differs from the checked-in corpus/goldens (rerun the probe)" >&2
    fail=1
  else
    goyaml_status="reproduced by the probe"
  fi
  rm -rf "$goyaml_tmp"
fi

configurable="$repo/crates/gadk-configurable/tests/fixtures"
configurable_counts=""
for spec in input.json:44 replay.json:44 'generated-recordings*.yaml':44 existing.yaml:6; do
  want="${spec##*:}"
  got="$(find "$configurable/recordings" -type f -name "${spec%:*}" 2>/dev/null | wc -l | tr -d ' ' || true)"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want ${spec%:*} files under $configurable/recordings, found $got" >&2
    fail=1
  fi
  configurable_counts="$configurable_counts ${spec%:*}=$got"
done
integration_n="$(find "$configurable/integration" -type f -name 'generated-recordings*.yaml' 2>/dev/null | wc -l | tr -d ' ' || true)"
if [ "$integration_n" -ne 2 ]; then
  echo "expected 2 generated-recordings*.yaml files under $configurable/integration, found $integration_n" >&2
  fail=1
fi
configurable_counts="$configurable_counts integration=$integration_n"
if [ ! -f "$configurable/fixups.json" ]; then
  echo "missing $configurable/fixups.json" >&2
  fail=1
fi
canon_want=40
canon_n="$(grep -c '"input":' "$configurable/canonicalize.json" 2>/dev/null || true)"
canon_n="${canon_n:-0}"
if [ "$canon_n" -ne "$canon_want" ]; then
  echo "expected $canon_want rows in $configurable/canonicalize.json, found $canon_n" >&2
  fail=1
fi
configurable_status="not rerun"
if ! command -v go >/dev/null 2>&1 || [ ! -d "$go/internal/configurable/conformance" ]; then
  echo "skipping the gen-go-configurable rerun and vendored-code check: needs go and ADK-Go at $go (set ADK_GO_DIR)" >&2
else
  go_abs="$(cd "$go" && pwd)"
  configurable_tmp="$(mktemp -d)"
  sed "s#=> ../../../adk-go#=> $go_abs#" "$repo/scripts/gen-go-configurable/go.mod" >"$configurable_tmp/go.mod"
  cp "$repo/scripts/gen-go-configurable/go.sum" "$configurable_tmp/go.sum"
  if ! (cd "$repo/scripts/gen-go-configurable" &&
    go run -modfile="$configurable_tmp/go.mod" . -fixtures "$configurable_tmp/fixtures" >/dev/null); then
    echo "scripts/gen-go-configurable failed" >&2
    fail=1
  elif ! diff -r -x integration "$configurable" "$configurable_tmp/fixtures" >&2; then
    echo "gen-go-configurable output differs from the checked-in fixtures (rerun the probe)" >&2
    fail=1
  elif ! (cd "$repo/scripts/gen-go-configurable" &&
    ADK_GO_DIR="$go_abs" go run -modfile="$configurable_tmp/go.mod" . -vendored adk >"$configurable_tmp/adk.txt" &&
    go run -modfile="$configurable_tmp/go.mod" . -vendored probe >"$configurable_tmp/probe.txt"); then
    echo "scripts/gen-go-configurable -vendored failed" >&2
    fail=1
  elif ! diff -u "$configurable_tmp/adk.txt" "$configurable_tmp/probe.txt" >&2; then
    echo "scripts/gen-go-configurable's vendored declarations differ from ADK-Go's (recopy them)" >&2
    fail=1
  else
    configurable_status="reproduced by the probe, vendored code matches ADK-Go"
  fi
  rm -rf "$configurable_tmp"
fi

agentanalytics="$repo/crates/gadk-agentanalytics/tests/fixtures"
agentanalytics_counts=""
agentanalytics_n=0
for spec in append_rows:12 arrow:9 config:4 formatter:8 gapic:3 gcs:12 helpers:11 plugin:6 \
  record_batch:27 redaction:2 schema:3 views:2 wire:41; do
  dir="$agentanalytics/${spec%%:*}"
  want="${spec#*:}"
  got="$(find "$dir" -type f 2>/dev/null | wc -l | tr -d ' ' || true)"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want agentanalytics fixtures under $dir, found $got" >&2
    fail=1
  fi
  agentanalytics_counts="$agentanalytics_counts ${spec%%:*}=$got"
  agentanalytics_n=$((agentanalytics_n + got))
done
agentanalytics_total="$(find "$agentanalytics" -type f 2>/dev/null | wc -l | tr -d ' ' || true)"
if [ "$agentanalytics_total" -ne 140 ] || [ "$agentanalytics_n" -ne 140 ]; then
  echo "expected 140 agentanalytics fixtures under $agentanalytics, found $agentanalytics_total" >&2
  fail=1
fi
agentanalytics_status="not rerun"
if ! command -v go >/dev/null 2>&1 || [ ! -d "$go/plugin/agentanalytics" ]; then
  echo "skipping the gen-go-agentanalytics rerun: needs go and ADK-Go at $go (set ADK_GO_DIR)" >&2
else
  agentanalytics_tmp="$(mktemp -d)"
  if ! ADK_GO_DIR="$(cd "$go" && pwd)" "$repo/scripts/gen-go-agentanalytics/run.sh" \
    "$agentanalytics_tmp/fixtures" >/dev/null; then
    echo "scripts/gen-go-agentanalytics failed" >&2
    fail=1
  elif ! diff -rq "$agentanalytics" "$agentanalytics_tmp/fixtures" >&2; then
    echo "gen-go-agentanalytics output differs from the checked-in fixtures (rerun scripts/gen-go-agentanalytics/run.sh)" >&2
    fail=1
  else
    agentanalytics_status="reproduced by the probe"
  fi
  rm -rf "$agentanalytics_tmp"
fi

storagepb="$repo/crates/gadk-agentanalytics/src/storagepb"
storagepb_status="not regenerated"
storagepb_manifest="$repo/scripts/gen-storagepb/Cargo.toml"
if ! command -v cargo >/dev/null 2>&1; then
  echo "skipping the storagepb drift check: cargo is not on PATH" >&2
elif ! cargo fetch --quiet --locked --manifest-path "$storagepb_manifest" >/dev/null 2>&1; then
  echo "skipping the storagepb drift check: cannot fetch scripts/gen-storagepb's crates (offline?)" >&2
else
  storagepb_tmp="$(mktemp -d)"
  if ! GEN_STORAGEPB_TARGET_DIR="${GEN_STORAGEPB_TARGET_DIR:-$repo/target/gen-storagepb}" \
    "$repo/scripts/gen-storagepb/run.sh" "$storagepb_tmp/out" >/dev/null; then
    echo "scripts/gen-storagepb failed" >&2
    fail=1
  elif ! diff -rq -x mod.rs "$storagepb_tmp/out" "$storagepb" >&2; then
    echo "generated storagepb differs from the checked-in code (rerun scripts/gen-storagepb/run.sh)" >&2
    fail=1
  else
    storagepb_status="regenerated identically"
  fi
  rm -rf "$storagepb_tmp"
fi

if [ "$fail" -eq 0 ]; then
  echo "ok: $n fixtures and $schema_n telemetry goldens match ADK-Go; $wire_n wire fixtures;" \
    "$a2a_wire_n A2A wire fixtures; $webui_n web UI files; launcher fixtures$launcher_counts;" \
    "agentengine fixtures$agentengine_counts; encode_shapes.rs $shapes_status;" \
    "goyaml$goyaml_counts; float table $float_n rows; goyaml $goyaml_status;" \
    "configurable$configurable_counts; canonicalize $canon_n rows; configurable $configurable_status;" \
    "agentanalytics $agentanalytics_total fixtures$agentanalytics_counts; agentanalytics $agentanalytics_status;" \
    "storagepb $storagepb_status"
fi
exit "$fail"
