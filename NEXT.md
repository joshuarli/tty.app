## Current assessment

Phase 1 establishes a healthy baseline, not an urgent bottleneck. At 148×44
and 2880×1800, the warm headless replay spends about 1.3 ms per frame on the
GPU, while cell upload and command encoding are only a few microseconds. The
sparse workload has essentially the same per-frame full-screen cost as the
two-pane workload because the current kernel shades the whole framebuffer.

The remaining phases are therefore experiments with explicit stop conditions,
not an assumed production roadmap. The most promising possible unlock is
damage-proportional GPU work for sparse updates; row indirection is less
compelling because the existing grid ring already makes a full-screen scroll
an O(cols) operation and dirty-row upload is already cheap.

## Phase 0: Freeze the contract

Define the invariants before changing production rendering:

- Cell pixels must remain identical.
- Cursor, selection, inverse, bold, wide glyphs, box drawing, arrows, padding, and resize must behave identically.
- No additional per-frame allocations after warmup.
- The current renderer remains available as the reference implementation.

Workloads:

- Canonical realistic replay: tmux at 148×44 with two panes, each running
  `less Cargo.lock`, followed by a deterministic scroll sequence.
- Sparse-update variant: scroll one `less` pane while leaving the other pane
  static.
- One-cell update.
- One dirty row.
- 5% dirty rows.
- Full-screen redraw.
- Repeated one-line scrolling.
- Cursor movement/blink.
- Selection changes.
- Scrollback viewport changes.
- Resize.

Use one canonical fullscreen geometry for performance work. The current
fullscreen shell reports:

```sh
stty size
# 44 148
```

Use `nrows = 44` and `ncols = 148` for the benchmark. Use a tiny 1×1 or 2×2
fixture only for fast shader correctness tests; do not use multiple terminal
sizes as a performance matrix. The fullscreen drawable's physical pixel
dimensions should also be recorded separately, since `stty size` reports
logical rows and columns only.

Gate: all workloads have deterministic inputs and a reference pixel output.
The tmux workload is recorded as a replayable PTY byte stream and replayed with
fixed 32 KiB chunk boundaries, so benchmark runs do not depend on the installed
tmux or less version.

## Phase 1: Build the headless baseline

Extract the reusable GPU portion from `MetalRenderer` into something like:

```text
MetalCore
  device
  pipeline
  command queue
  palette buffer

MetalBaseline
  atlas
  cell buffer
  uniform buffer
  offscreen output texture
  full-frame encode/dispatch

MetalRenderer
  CAMetalLayer
  drawable acquisition
  presentation
```

The existing [headless_metal.rs](</Users/josh/d/tty.app/tests/headless_metal.rs:31>) already provides the foundation.

The `metal_baseline` group in `benches/bench.rs` measures the reusable headless path; timing assertions remain outside the correctness tests.

Measure separately:

- CPU cell-copy bytes and time.
- Command encoding time.
- GPU execution time.
- Number of dispatched threads.
- Allocations.
- Retained GPU memory.

Compile pipelines and allocate textures once. Warm up first. Batch many frames before synchronizing with `waitUntilCompleted`; otherwise synchronization dominates.

Gate: baseline numbers are recorded on the target Mac with device name, macOS version, compiler profile, `nrows=44`, `ncols=148`, physical drawable size, workload, median, and variance. The baseline includes both tmux panes scrolling and one active pane with one static pane.

Phase 1 baseline recorded on 2026-07-12 with `cargo bench --bench bench -- metal_baseline --noplot --sample-size 10 --warm-up-time 0.2 --measurement-time 0.5`:

```text
device: Apple M1
macOS: 27.0 (26A5378j)
rustc: 1.97.0-nightly, aarch64-apple-darwin
logical: 148×44
physical: 2880×1800
atlas: 2048×2048 R8Unorm
headless resources: 24,984,496 bytes

tmux_less_both:
  trace: 425,984 bytes, 13 replay frames
  CPU wall: 21.489 ms, GPU: 17.001 ms
  upload: 663,040 bytes in 0.022 ms
  encode: 0.073 ms, dispatched pixels: 67,392,000
  steady-state Rust allocations: 0
  Criterion: 22.526 ms [22.420, 22.608]

tmux_less_sparse:
  trace: 241,664 bytes, 8 replay frames
  CPU wall: 13.723 ms, GPU: 10.433 ms
  upload: 408,480 bytes in 0.013 ms
  encode: 0.047 ms, dispatched pixels: 41,472,000
  steady-state Rust allocations: 0
  Criterion: 13.804 ms [13.635, 13.939]
```

These are full-frame render baselines: every replay frame dispatches the entire
2880×1800 output. The trace files are generated under `target/` by
`scripts/record-tmux-less.sh` and are intentionally not part of the source
tree. The recorder pins the PTY geometry and uses this repository's `Cargo.lock`;
the benchmark replays the captured output without launching tmux or less.

## Phase 1.5: Measure the live parser and grid path

The Phase 1 benchmark precomputes parser/grid snapshots before timing Metal.
Add an end-to-end replay measurement that includes PTY chunk replay, parsing,
`TermPerformer` mutations, visible-grid snapshot preparation, cell upload,
encoding, and GPU completion. Keep the isolated Phase 1 numbers alongside it,
so CPU terminal work is not confused with rendering work.

The `metal_replay` group now performs this measurement using the production
`TermPerformer` and the same fixed 32 KiB chunk boundaries. Parser, performer,
and Grid mutations are timed together because the parser invokes the performer
inline; row preparation, upload, encoding, and GPU completion are measured as
separate stages. The replay state resets in place between samples so warm
allocation counts exclude terminal-state construction and atlas population.

Report separately:

- Parser, performer, and Grid mutation time.
- Visible-grid row preparation time.
- Cell-copy/upload, encoding, and GPU time.
- Allocations after warmup.

Phase 1.5 recorded on 2026-07-12 with the same device, geometry, and benchmark
configuration as Phase 1:

```text
metal_replay tmux_less_both:
  13 chunks, 13 rendered frames, 425,984 input bytes
  parser + performer + Grid: 0.898 ms
  row preparation: 0.001 ms
  upload: 663,040 bytes in 0.019 ms
  encode: 0.082 ms, GPU: 17.031 ms
  replay wall time: 21.299 ms
  warm allocations: 2, 128 bytes

metal_replay tmux_less_sparse:
  8 chunks, 8 rendered frames, 241,664 input bytes
  parser + performer + Grid: 0.437 ms
  row preparation: 0.001 ms
  upload: 408,480 bytes in 0.010 ms
  encode: 0.045 ms, GPU: 12.679 ms
  replay wall time: 15.479 ms
  warm allocations: 2, 128 bytes
```

The measurement confirms that the current GPU full-frame dispatch is the
dominant measured stage for this workload. CPU parsing, Grid mutation, row
preparation, and cell upload are not presently large enough to justify row
indirection. The two warm allocations per replay should be investigated only
if a later gate requires strict zero-allocation replay; they do not change the
current architectural conclusion.

Gate: only pursue a structural CPU change if Grid mutation or row preparation
is a material part of end-to-end frame time. If it is not, leave the existing
ring-buffer and dirty-row design unchanged.

## Phase 2: Evaluate row indirection conditionally

Keep cells in physical ring order instead of repacking them into visible logical order.

Add a GPU-visible mapping:

```text
logical screen row → physical Cell row
```

A full-screen scroll then becomes:

- Advance the row base or mapping.
- Clear one newly exposed physical row.
- Upload only that row.
- Update a small uniform or row-map buffer.

Initially, limit this to the live screen. Keep the existing scrollback path as a fallback until the live path is proven.

Gate:

- Pixel output matches the reference after randomized writes, wraps, scrolls, and cursor movement.
- The end-to-end measurement shows that row preparation or logical-to-physical mapping is a material cost, and the prototype reduces that cost substantially.
- Full redraw performance does not regress materially.
- No new steady-state allocations.

If the Phase 1.5 measurement does not meet the first condition, stop here.
The existing ring buffer already removes the expensive rows×columns scroll
copy, so this is not justified by theoretical asymptotics alone.

## Phase 3: Prototype retained damage rendering conditionally

Create a persistent offscreen framebuffer texture.

Instead of rendering every output pixel for every update:

- Keep the last complete frame in the texture.
- Dispatch compute work only for dirty rectangles or contiguous dirty-row runs.
- Acquire the CAMetal drawable afterward.
- Blit the retained texture to the drawable and present it.

For the first version, use contiguous dirty-row runs as damage regions. That avoids complicated rectangle merging while still providing proportional behavior.

The damage kernel should receive an origin and render a local dispatch region:

```text
damage origin + local thread position → framebuffer pixel
```

Do not dispatch a full-screen kernel with an early-exit damage check; that would reduce visual work but not dispatched work.

Gate:

- Exact pixel equivalence against a fresh full-frame render.
- Partial updates dispatch only their damage area, allowing for threadgroup padding.
- The sparse tmux/less replay achieves at least a 2× reduction in GPU time, or
  an equally clear reduction in measured render energy, without changing pixels.
- One-row and one-cell updates show a material GPU-time reduction rather than
  only a lower thread count.
- Full redraws remain within a 10% regression budget.
- Resize and first-frame initialization force a complete damage pass.
- Retained texture synchronization is correct across consecutive command buffers.

Use a small headless prototype to establish these numbers before touching
`MetalRenderer`. If the sparse win is below the gate, or the full-redraw cost
exceeds the gate, stop and retain the current full-frame renderer. A final
blit, command-buffer overhead, and drawable synchronization can otherwise
erase the theoretical savings.

Phase 3 headless prototype result, recorded on 2026-07-12:

```text
metal_damage tmux_less_both:
  13 frames, 13 damage regions, 61,655,040 dispatched pixels
  upload: 663,040 bytes in 0.017 ms
  encode: 0.075 ms, GPU: 17.623 ms, wall: 22.072 ms

metal_damage tmux_less_sparse:
  8 frames, 8 damage regions, 38,125,440 dispatched pixels
  upload: 408,480 bytes in 0.012 ms
  encode: 0.056 ms, GPU: 12.726 ms, wall: 16.210 ms
```

The damage replay was compared against the full-frame replay after every
render using frame hashes, followed by an exact final-pixel comparison; both
workloads matched. However, contiguous dirty-row damage reduced dispatched
pixels by only 8–9%. GPU time increased by about 6% for the two-pane workload
and 25% for the sparse workload; wall time increased similarly for sparse
replay. The current workload therefore fails the 2× sparse GPU-time gate.

Decision: stop the damage prototype here and do not integrate it into
`MetalRenderer`. The retained surface and damage-origin shader support may be
kept as benchmark infrastructure, but Phase 5 is deferred unless a future
workload produces much larger, stable damage regions or a separate energy
measurement demonstrates a worthwhile gain.

## Phase 3.5: Prototype scroll-aware retained rendering

The generic damage result suggested testing a more specialized path for the
terminal's most structured large update: scrolling. The headless prototype now
does the following:

- Records semantic scroll hints from the grid, including scroll-region bounds
  and direction.
- Retains a second Metal texture as a scratch surface.
- Copies the preserved pixel rows with a dedicated Metal compute kernel.
- Re-renders newly exposed rows, the scroll boundary row, genuinely dirty rows,
  and old/current cursor rows.
- Uses exact frame hashes and final pixel comparison against a fresh full-frame
  replay.

The synthetic 148×44 workload initialized the screen and then performed one
41-line full-screen scroll. The result matched the full replay exactly:

```text
full replay:   10,368,000 dispatched pixels, 104,192 uploaded bytes
scroll replay:  9,780,480 dispatched pixels, 101,824 uploaded bytes
                 41,310,720 retained-surface copy bytes
                 7.26 ms median benchmark time
```

This is only a 5.7% dispatch reduction and a 2.3% upload reduction, while the
headless resource estimate grows to about 45.7 MiB because of the second
2880×1800 framebuffer. The recorded tmux/less traces contained no semantic
scroll events, so they fell back to the generic damage path and provided no
evidence for production scroll integration.

Decision: keep the scroll-aware path as validated headless research
infrastructure, but do not proceed to Phase 5. The measured saving is too small
for the extra retained surface, synchronization, and copy traffic. A future
attempt should first eliminate the second full-size surface or demonstrate a
workload with substantially larger stable unchanged regions.

## Phase 4: Measure and reduce idle power

Idle behavior is a separate optimization target from active rendering. The
renderer submits no Metal work when there is no terminal activity. Before this
phase, the single-threaded loop called `kevent()` with an 8 ms timeout whenever
it was idle, creating up to 125 periodic CPU wakeups per second even when the
terminal was completely quiet.

Establish a quiet-session baseline before changing the loop:

- Release build, fullscreen 148×44 terminal, no PTY output, input, mouse, or
  resize activity for at least 60 seconds.
- Record timeout wakeups, actual AppKit/PTY events, render calls, Metal command
  buffer submissions, process CPU time, and package/GPU power where the Mac
  exposes stable counters.
- Repeat with the current app as the control and keep display, power source,
  thermal state, and background workload fixed.
- Use `powermetrics` and/or an Instruments Energy Log for system-level energy;
  use internal counters for wakeups and renderer activity.

The first implementation experiment should replace periodic idle polling with
an event-driven wait that can be woken by either AppKit or PTY readiness. A
longer adaptive timeout is an acceptable fallback only if it preserves the
latency gate.

Gate:

- No Metal command buffers are submitted during a quiet 60-second session.
- Periodic timeout wakeups disappear or fall to an insignificant background
  level rather than remaining at the current 8 ms cadence.
- PTY-output-to-render and key-to-PTY-write latency remain within the current
  approximately one-frame budget.
- Process CPU and measured system/package energy show a repeatable reduction
  beyond measurement noise.
- If the event-driven design produces no repeatable energy improvement, or
  harms responsiveness, stop and retain the simpler current loop.

This phase is independent of the GPU rendering experiments. It should be
completed even if the instanced renderer does not pass its performance gate.

Implementation status, 2026-07-12: the fixed idle timeout has been replaced by
Core Foundation run-loop sources for PTY readiness. AppKit and PTY activity now
wake the same main-thread loop, PTY output continues to drain while a window is
unfocused, and rendering is skipped for unfocused windows. Focus-in marks the
grid dirty so the next focused iteration repaints the current frame. Runtime
wakeup and energy measurements remain to be collected; no production I/O thread
was added.

## Phase 5: Prototype instanced cell rendering

The next architectural experiment should attack the dominant cost directly:
the current compute kernel launches one thread per output pixel and repeats
cell-coordinate math, palette selection, decoration branches, and atlas lookup
for every pixel. Damage and scroll retention do not remove enough work to
justify their surface-copy overhead.

Build a headless Metal prototype that renders the screen as instanced cell
quads:

- One instance per visible terminal cell.
- Vertex data maps each instance to its cell rectangle.
- The fragment shader samples the glyph atlas and resolves palette and cell
  attributes.
- Preserve procedural box drawing, arrows, decorations, cursor, wide cells,
  inverse, hidden, and selection behavior.
- Start with a full-frame render; do not add retained-surface damage as part of
  this experiment.
- Reuse the existing `Cell` ABI and atlas wherever possible.

Measure on the canonical 148×44, 2880×1800 geometry using the tmux/less
two-pane and sparse traces, plus deterministic one-cell, one-row, full-redraw,
cursor, and scroll workloads. Record GPU time, wall time, command encoding,
CPU upload, allocations, GPU memory, and energy when a stable Metal counter is
available.

Gate:

- Exact pixel equivalence against the current compute renderer for every
  workload and supported visual feature.
- At least a 2× GPU-time reduction on a representative workload, or an equally
  clear energy reduction.
- Full redraw wall time does not regress by more than 10%.
- No additional full-size framebuffer or steady-state per-frame allocations.
- Headless and production Metal pipeline creation remain deterministic.

If this gate fails, keep the current compute renderer and stop pursuing GPU
damage architecture for this workload. If it passes, validate the instanced
pipeline onscreen before considering any retained-surface optimization.

Phase 5 headless prototype result, recorded on 2026-07-12 with the same Apple
M1, 148×44 logical geometry, and 2880×1800 physical drawable as the earlier
baselines:

```text
metal_instanced tmux_less_both:
  13 frames, 84,656 cell instances, 663,040 uploaded bytes
  encode: 0.140 ms, GPU: 15.688 ms, wall: 19.665 ms
  steady-state Rust allocations: 0

metal_instanced tmux_less_sparse:
  8 frames, 52,096 cell instances, 408,480 uploaded bytes
  encode: 0.086 ms, GPU: 9.848 ms, wall: 12.198 ms
  steady-state Rust allocations: 0
```

The instanced path uses one four-vertex triangle strip per cell, the existing
8-byte Cell ABI, the existing atlas, and one render pass into the same-size
BGRA8 output texture. It adds no framebuffer and no per-frame allocation. The
recorded replays matched the compute renderer pixel-for-pixel after every
frame. A dedicated headless feature test also covers padding, atlas glyphs,
wide cells, box drawing, arrows, bold, inverse, hidden, selection,
underline, strikethrough, and cursor inversion.

Relative to the same-run full-frame compute baseline, GPU time improved by
about 5% for the two-pane replay and 3% for the sparse replay. Command
encoding was slightly more expensive, and the wall-time change was within
noise. This fails the 2× GPU-time or equally clear energy-reduction gate.

Decision: retain the instanced pipeline as headless benchmark and correctness
infrastructure, but do not integrate it into `MetalRenderer`. The production
renderer remains the compute path. Phase 6 is therefore not justified by the
current workload; the next architectural work should wait for a workload or
measurement that demonstrates a larger stable GPU or energy bottleneck.

## Phase 6: Integrate production presentation only if Phase 5 wins

Currently deferred until the Phase 5 instanced-rendering gate passes. Generic
damage and scroll-aware retained rendering remain benchmark infrastructure only.

Once the headless core passes:

- Replace the direct drawable compute path with the winning instanced pipeline.
- Preserve the current double-buffering and GPU-in-flight behavior.
- Handle drawable misses without losing the current frame.
- Validate scrollback viewport rendering, selection, synchronized output, and resize.
- Keep a debug switch to compare legacy and instanced renderers during development.

Gate:

```text
cargo test --all-targets
headless correctness suite
instanced-cell benchmark
```

No production switch until all pass and the Phase 5 performance gates are met.
If they are not met, document the prototype result and leave the current
renderer as the production path.

## Phase 7: Decide whether to extend the architecture

Only after the Phase 4 through Phase 6 experiments establish a real bottleneck:

- If CPU upload remains significant, make scrollback GPU-resident or paged.
- If GPU compute remains significant for sparse updates, improve damage-region batching.
- If input latency suffers under sustained PTY output, consider an SPSC parser thread.
- If fidelity becomes the priority, add a style table so truecolor can be supported without widening `Cell`.

The key acceptance criterion is not “the benchmark is faster.” It is:

> CPU upload, GPU shading, and memory traffic must scale with changed terminal content rather than total screen area, while matching the reference renderer pixel-for-pixel.

For the current measured workload, this is an optimization opportunity rather
than a demonstrated necessity. Phase 4 is the next experiment; it should be
completed independently, while Phase 5 should be abandoned if it cannot
produce a large, exact, low-memory win over the current full-frame compute
renderer.
