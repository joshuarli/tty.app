#!/usr/bin/env bash

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PGO_DIR="$ROOT/target/pgo-profiles"
RUN_DIR="$ROOT/target/profiling/latest"
SYSROOT=$(rustc --print sysroot)
HOST=$(rustc -vV | sed -n 's/^host: //p')
LLVM_PROFDATA="$SYSROOT/lib/rustlib/$HOST/bin/llvm-profdata"

if [ ! -x "$LLVM_PROFDATA" ]; then
  echo "llvm-profdata not found at $LLVM_PROFDATA" >&2
  echo "Install the llvm-tools component for the active Rust toolchain." >&2
  exit 1
fi

rm -rf "$PGO_DIR" "$RUN_DIR"
mkdir -p "$PGO_DIR" "$RUN_DIR"

export CARGO_INCREMENTAL=0
export LLVM_PROFILE_FILE="$PGO_DIR/%m-%p.profraw"
export RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=$PGO_DIR"

{
  echo "started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "commit=$(git -C "$ROOT" rev-parse HEAD)"
  echo "rustc=$(rustc --version)"
  echo "host=$HOST"
  echo "tests=cargo test --workspace"
  echo "benchmarks=cargo bench --workspace --bench bench -- --noplot"
  echo "pgo_dir=$PGO_DIR"
  echo "run_dir=$RUN_DIR"
} > "$RUN_DIR/manifest.txt"

echo "==> Running the test suite with LLVM instrumentation"
(cd "$ROOT" && cargo test --workspace 2>&1 | tee "$RUN_DIR/tests.log")

echo "==> Running all Criterion benchmarks with LLVM instrumentation"
(cd "$ROOT" && cargo bench --workspace --bench bench -- --noplot 2>&1 | tee "$RUN_DIR/bench.log")

shopt -s nullglob
profiles=("$PGO_DIR"/*.profraw)
shopt -u nullglob
if [ "${#profiles[@]}" -eq 0 ]; then
  echo "No LLVM profiles were generated." >&2
  exit 1
fi

"$LLVM_PROFDATA" merge -sparse -o "$PGO_DIR/merged.profdata" "${profiles[@]}"

{
  echo "raw_profiles=${#profiles[@]}"
  echo "merged_profile=$PGO_DIR/merged.profdata"
  echo "finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >> "$RUN_DIR/manifest.txt"

echo "==> PGO profile ready: $PGO_DIR/merged.profdata"
echo "==> Logs and manifest: $RUN_DIR"
