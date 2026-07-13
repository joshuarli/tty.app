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
completed independently of the earlier presentation-pipeline experiments.

Implementation status, 2026-07-12: the fixed idle timeout has been replaced by
Core Foundation run-loop sources for PTY readiness. AppKit and PTY activity now
wake the same main-thread loop, PTY output continues to drain while a window is
unfocused, and rendering is skipped for unfocused windows. Focus-in marks the
grid dirty so the next focused iteration repaints the current frame. Runtime
wakeup and energy measurements remain to be collected; no production I/O thread
was added.

## Phase 5: Prototype cell-tiled compute rendering

Earlier retained-damage and presentation experiments did not pass their
performance gates. This prototype keeps the compute renderer but changes its
work organization: one Metal threadgroup owns one terminal cell, loads the 8-byte
Cell and resolves its palette/style state once into threadgroup memory, then
shades that cell's pixels. The existing full-frame compute kernel remains the
reference implementation.

The tiled kernel shades the cell interiors and explicitly paints every border
pixel, including the leftover pixels after the final row or column. This is
required because a CAMetal drawable is not guaranteed to preserve its previous
contents. No retained framebuffer or additional per-frame allocation was
introduced.

The canonical replay validation matched the reference pixel-for-pixel after
every frame. The focused headless visual test also covers the tiled path for
padding, box drawing, arrows, bold, inverse, hidden, selection, decorations,
wide cells, and cursor inversion; recorded replays cover atlas glyph sampling.

Representative warm headless measurements on the Apple M1 at 148×44 and
2880×1800 were:

```text
metal_tiled tmux_less_both:
  13 frames, 61,121,632 tiled pixels
  GPU: 11.786 ms, wall: 14.861 ms

metal_tiled tmux_less_sparse:
  8 frames, 37,613,312 tiled pixels
  GPU: 9.154 ms, wall: 11.067 ms
```

The corresponding same-run full-frame compute sample was 17.430 ms GPU and
22.055 ms wall for the two-pane replay, and 11.101 ms GPU and 13.275 ms wall
for the sparse replay. Repeated GPU timings vary with the device scheduler,
but the result is materially better than the earlier GPU experiments: roughly
30% GPU reduction for the two-pane replay and 15–20% for the sparse replay, with zero
steady-state Rust allocations. The tiled dispatch covers about 9% fewer pixels
because it omits static padding; the larger gain comes from eliminating
repeated per-pixel cell/style work.

Decision: this is the first active-rendering prototype that merits a guarded
production integration experiment. Keep the full-frame kernel as the fallback
until the remaining drawable, resize, double-buffering, synchronized output,
scrollback, selection, cursor, and manual visual gates pass.

## Phase 5.5: Prototype retained masked tiled rendering

The full tiled path still shades every cell on every frame. This prototype
retains the output surface, compares dirty rows against the resident GPU cell
state, and builds a compact list of cells whose content or cursor overlay must
be redrawn. One Metal dispatch processes that active-cell list; unchanged cells
are neither uploaded nor shaded. The first frame is still a complete tiled
render, and the retained surface is not yet connected to `MetalRenderer`.

The replay matched the full-frame reference pixel-for-pixel after every frame
for both tmux/less traces. On the Apple M1 at 148×44 and 2880×1800, the latest
same-run sample was:

```text
metal_tiled_damage tmux_less_both:
  13 frames, 22,899 active cells of 84,656 total
  183,120 uploaded bytes, 16,533,078 active-cell pixels
  GPU: 10.689 ms versus 18.285 ms full-frame

metal_tiled_damage tmux_less_sparse:
  8 frames, 16,731 active cells of 52,096 total
  133,800 uploaded bytes, 12,079,782 active-cell pixels
  GPU: 8.141 ms versus 11.219 ms full-frame
```

The result is promising but below the hoped-for 50–70% reduction on this
short workload: roughly 42% for the two-pane trace and 27% for sparse in this
sample. GPU timestamps vary materially between runs. CPU-side cell comparison
and compaction are also additional work, although the retained prototype uses
no per-frame heap allocation in its warmed benchmark path.

Decision: keep the compact-list retained path as headless benchmark
infrastructure, but do not integrate it onscreen yet. A longer replay and a
scroll-aware retained path are the next ways to test whether the larger target
is real. If repeated measurements stay below the gate, retain the simpler
full-frame tiled renderer.

## Phase 5.6: Combine semantic scroll retention with active-cell rendering

The retained active-cell replay now consumes `ScrollHint` events. For a valid
hint it shifts the resident cell rows in lockstep with the retained GPU
surface, copies preserved pixels with the existing `scroll_copy` kernel, and
dispatches only exposed cells, changed cells, and cursor cells. Full-screen
scrolls render directly into the copy destination before swapping surfaces, so
they do not require a mirror pass; partial regions retain the synchronization
fallback. When no hint is available, the replay uses the existing active-cell
path unchanged.

The combined path matched the full renderer's frame hashes and final pixels
for both recorded traces and the repeated synthetic scroll replay. The
recorded traces still produced no semantic hints, so their active-cell work
was unchanged:

```text
tmux_less_both:   0 hints, 22,899 active cells, 16,533,078 dispatched pixels
tmux_less_sparse: 0 hints, 16,731 active cells, 12,079,782 dispatched pixels
```

The synthetic replay was framed at one-line-sized chunks to exercise repeated
scrolling: 82 frames, 38 hints, and 41 total scroll lines. In a same-run sample
the active-cell baseline took 5.298 ms GPU and 35.787 ms wall; the
scroll-retained path took 8.450 ms GPU and 80.527 ms wall, with 734,722,560
retained-surface copy bytes. The copy traffic outweighed the saved cell
shading, increasing GPU time by about 60% and wall time by about 125%.

Decision: keep semantic scroll retention as validated benchmark
infrastructure, but do not integrate it into the renderer. The current
2880×1800 workload does not meet the performance gate; a future attempt needs
cheaper surface movement or a workload where the copied region is materially
smaller than the avoided active-cell shading.

## Phase 6: Integrate tiled compute only if the headless gates win

The headless full tiled-compute gate passes. The retained masked prototype does
not yet meet the 50–70% reduction target, so generic damage and scroll-aware
retained rendering remain benchmark infrastructure only. The production switch
is still guarded and defaults to the full-frame compute path.

The guarded integration is selected with `TTY_TILED_RENDER=1` when launching
the app. It preserves the current double-buffering and GPU-in-flight behavior,
handles drawable misses through the existing retry path, and keeps the
full-frame compute path available as the default and comparison reference.

Remaining onscreen checks:

- Validate scrollback viewport rendering, selection, synchronized output, and resize.
- Exercise focus transitions and drawable misses while output is active.
- Compare the same workload against the default path for visual artifacts and
  repeated GPU timing.

Gate:

```text
cargo test --all-targets
headless correctness suite
tiled-compute benchmark
```

No default production switch until the remaining onscreen checks and repeated
GPU/energy measurements pass. If they do not, remove the switch and leave the
current renderer as the production path.

## Phase 7: Decide whether to extend the architecture

Only after the Phase 4 through Phase 6 experiments establish a real bottleneck:

- If CPU upload remains significant, make scrollback GPU-resident or paged.
- If GPU compute remains significant for sparse updates, improve retained cell
  compaction or GPU-resident damage masks.
- If input latency suffers under sustained PTY output, consider an SPSC parser thread.
- If fidelity becomes the priority, add a style table so truecolor can be supported without widening `Cell`.

The key acceptance criterion is not “the benchmark is faster.” It is:

> CPU upload, GPU shading, and memory traffic must scale with changed terminal content rather than total screen area, while matching the reference renderer pixel-for-pixel.

For the current measured workload, this is an optimization opportunity rather
than a demonstrated necessity. Phase 4 remains independent and should still
receive runtime energy measurements. Phase 5 is the first active-rendering
prototype with enough measured benefit to justify a guarded onscreen
integration experiment.
