#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# Regenerates crates/gadk-agentanalytics/src/storagepb/google.*.rs from the vendored protos in
# crates/gadk-agentanalytics/proto (7b-2).
#
# Usage: scripts/gen-storagepb/run.sh [OUT_DIR]
#   OUT_DIR defaults to crates/gadk-agentanalytics/src/storagepb. Its google.*.rs files are
#   replaced as a whole once generation succeeds; the hand-written mod.rs is left alone.
#   The generator builds with this directory's own Cargo.lock (--locked) into
#   $GEN_STORAGEPB_TARGET_DIR (default: target/ here); protoc comes from protoc-bin-vendored.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="${1:-$repo/crates/gadk-agentanalytics/src/storagepb}"
target="${GEN_STORAGEPB_TARGET_DIR:-$here/target}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo build --quiet --locked --release --manifest-path "$here/Cargo.toml" --target-dir "$target"
"$target/release/gen-storagepb" "$repo/crates/gadk-agentanalytics/proto" "$tmp/gen"
mkdir -p "$out"
rm -f "$out"/google.*.rs
cp "$tmp"/gen/google.*.rs "$out/"
