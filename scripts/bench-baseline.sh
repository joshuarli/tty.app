#!/usr/bin/env bash
# Benchmark baseline management using criterion + critcmp.
#
# Usage:
#   ./scripts/bench-baseline.sh              # Run benchmarks, compare against saved baseline
#   ./scripts/bench-baseline.sh --update     # Run benchmarks and update baseline
#   ./scripts/bench-baseline.sh --compare    # Compare last run against baseline (no bench run)
#
# Baselines:
#   target/criterion/*/baseline/   — criterion's named baseline (local, not committed)
#   benchmarks/baseline.json       — critcmp export (committed, tracks perf across commits)
#
# Requires: critcmp (cargo install critcmp)

set -euo pipefail

BASELINE_NAME="baseline"
EXPORT_FILE="benchmarks/baseline.json"
ALLOC_STDERR=$(mktemp)
UPDATE=false
COMPARE_ONLY=false

# PGO profile path — benchmarks use the same profiles as the release build.
PGO_MERGED="$(cd "$(dirname "$0")/.." && pwd)/target/pgo-profiles/merged.profdata"

for arg in "$@"; do
  case "$arg" in
    --update)  UPDATE=true ;;
    --compare) COMPARE_ONLY=true ;;
    *) echo "Unknown arg: $arg"; exit 1 ;;
  esac
done

# Build PGO profiles if they don't exist yet.
if [ ! -f "$PGO_MERGED" ]; then
  echo "No PGO profiles found, collecting..."
  make -C "$(dirname "$0")/.." pgo-profile
fi

BENCH_CMD="cargo bench --bench bench"
if [ -f "$PGO_MERGED" ]; then
  export RUSTFLAGS="-Cprofile-use=$PGO_MERGED"
fi

if [ "$COMPARE_ONLY" = false ]; then
  echo "Running cargo bench (PGO)..."
  if [ "$UPDATE" = true ]; then
    # Save as the baseline
    $BENCH_CMD -- --save-baseline "$BASELINE_NAME" 2>"$ALLOC_STDERR"
  else
    # Compare against baseline without overwriting
    $BENCH_CMD -- --baseline "$BASELINE_NAME" 2>"$ALLOC_STDERR"
  fi
  echo ""

  # Print alloc audit from stderr
  if [ -s "$ALLOC_STDERR" ]; then
    grep -E '^\s*(──|(\[alloc\]))' "$ALLOC_STDERR" || true
    echo ""
  fi
fi

# Show critcmp comparison
if [ "$COMPARE_ONLY" = true ]; then
  # Compare exported baseline against last criterion run
  if [ -f "$EXPORT_FILE" ]; then
    echo "Comparing exported baseline against last run:"
    critcmp "$EXPORT_FILE" || echo "(no data to compare)"
  else
    echo "No exported baseline found. Run with --update first."
  fi
elif [ "$UPDATE" = true ]; then
  # Export the baseline as committable JSON
  mkdir -p "$(dirname "$EXPORT_FILE")"
  critcmp --export "$BASELINE_NAME" > "$EXPORT_FILE"

  commit=$(git rev-parse --short HEAD)
  msg=$(git log --oneline -1 | cut -d' ' -f2-)
  echo "Baseline saved: $EXPORT_FILE"
  echo "  commit: $commit ($msg)"
  echo "  To commit: git add $EXPORT_FILE && git commit"
else
  echo "Comparison against baseline (use --update to save new baseline):"
  echo ""
fi

rm -f "$ALLOC_STDERR"
