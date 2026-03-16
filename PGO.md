# Profile-Guided Optimization

PGO compiles an instrumented binary, runs it on realistic workloads to record
branch/call patterns, then recompiles using those profiles. Typical gains:
10-20% on real-world programs.

## Quick start

```bash
make release-pgo
```

This runs the benchmark suite under instrumentation, then recompiles with the
gathered profiles. For better coverage, add a manual session first (see below).

## How it works

### Automatic profiles (benches)

`make release-pgo` runs the criterion benchmarks under an instrumented build.
This exercises the parser hot path (SIMD scanner, CSI fast path, state machine,
grid mutations) with realistic byte patterns.

### Manual session capture

Benchmarks miss the render path, PTY I/O, and interactive patterns. For fuller
coverage, record a manual session:

```bash
# Record raw PTY output
script -q sessions/interactive.raw
# ... use the terminal: cat large files, run ls --color, use vim, scroll
# ... then exit

# Record a heavy-output session
ls --color=always -laR / > /dev/null 2>&1  # or any noisy command
```

The `sessions/` directory holds raw byte streams that can be replayed through
the parser+grid pipeline without a window or PTY.

### Suggested recordings

Keep a few recordings covering different workloads:

| File | Workload | Exercises |
|---|---|---|
| `sessions/bulk-ascii.raw` | `cat` a large source file | SIMD scanner, bulk ASCII path |
| `sessions/heavy-sgr.raw` | `ls --color=always -laR` of a big dir | SGR parsing, 256-color |
| `sessions/interactive.raw` | vim/tmux session | cursor movement, alt screen, scroll regions |

### Replay harness

To use recorded sessions for PGO, add a replay benchmark or test that feeds
the raw bytes through the parser:

```rust
let data = std::fs::read("sessions/bulk-ascii.raw").unwrap();
let mut grid = Grid::new(cols, rows);
let mut parser = Parser::new();
// ... set up scrollback, performer

for chunk in data.chunks(65536) {
    parser.parse(chunk, &mut performer);
}
```

This exercises the real hot path with real byte patterns, fully automatable,
no display needed.

## Full PGO + BOLT workflow

For maximum optimization (additional 2-5% on top of PGO):

```bash
cargo install cargo-pgo

# 1. PGO profiling
cargo pgo bench

# 2. (Optional) manual session profiling — profiles accumulate
cargo pgo build
./target/release/tty  # use it, then exit

# 3. PGO + BOLT
cargo pgo bolt build --with-pgo
./target/release/tty-bolt-instrumented  # use it, then exit
cargo pgo bolt optimize --with-pgo
```

## Notes

- Profiles accumulate in `target/pgo-profiles/` — multiple runs merge automatically.
- Instrumented binaries are slow — budget extra time for profiling runs.
- Stale profiles are worse than none — regenerate after significant code changes.
- BOLT instrumentation mode works in CI (no hardware perf counters needed).
