# Performance and memory design

tty is a macOS terminal emulator. The primary performance goal is responsive
interactive rendering; the primary resource goal is modest memory use. Avoid
trading substantial resident memory or complicated bookkeeping for small,
workload-sensitive speedups. Fast math is preferable to extra memory traffic
and data structures when the visual result is unchanged.

## Current architecture

- Terminal state lives on the main thread and PTY I/O is non-blocking.
- Cells are 8-byte CPU/GPU values.
- The Metal renderer uses cell-tiled compute rendering: one threadgroup owns
  one terminal cell and loads its cell/style state once.
- Dirty rows reduce CPU cell-buffer uploads. The production renderer renders
  the complete terminal surface into the drawable; retained-surface rendering
  remains benchmark-only research infrastructure.
- ASCII regular and bold glyphs use direct atlas tables. Other glyphs use the
  atlas cache and are rasterized on demand.
- The atlas is a 2048×2048 R8Unorm texture. Each cache slot reserves two cell
  columns so narrow and wide glyphs share one simple coordinate contract.

## Measured baseline

Measurements below were collected on an Apple M1 Max at 148×44 logical cells,
2880×1800 physical pixels, using the repository's deterministic tmux/less
replays. GPU timings vary with scheduler load, so individual samples are
directional; repeated Criterion runs are preferred for decisions.

The full cell-tiled path has measured GPU times around 1.2–1.7 ms per replay
frame. The reference full-frame shader is commonly around 0.6–1.2 ms. CPU row
preparation and cell upload are generally microseconds and are not the current
dominant cost.

The benchmark suite now emphasizes representative workloads rather than
isolated operations:

- canonical 148×44 terminal size, matching the primary development machine;
- full-screen redraws, one-row updates, and sparse multi-row updates;
- selection changes, resize, synchronized full redraw, and scroll workloads;
- bold/Unicode/wide-character work through the existing end-to-end fixtures;
- renderer wall time, GPU time, command encoding, upload bytes, allocations,
  and headless resource estimates.

The old parser, SIMD, grid, pipeline, scrollback, hash-table, and atlas-LRU
microbenchmarks are no longer Criterion targets. They were too narrow and
noisy to guide renderer or memory decisions; the representative workloads and
allocation audit retain the useful end-to-end signal.

The release startup benchmark currently reports approximately:

```text
font/rasterizer setup:  ~4.9 ms
regular + bold atlas:   ~4.6 ms
grid setup:              ~0.01 ms
```

The current timestamp-based atlas lookup benchmark reports approximately 38 µs
for 10,000 cached lookups. An eviction/insertion cycle is about 1.5 µs,
including the small benchmark glyph upload.

The direct comparison measured 86.7 µs for 10,000 intrusive-LRU hits versus
23.4 µs for timestamp hits. The intrusive links therefore cost about 63 µs per
10,000 hits, or roughly 6 ns per cached glyph. Intrusive eviction measured
about 724 ns versus 1.06 µs for the timestamp model. That eviction comparison
is directional, but eviction is still a cold path compared with cache hits.

Startup has a hard 40 ms budget. Both `--startup-bench` and the Criterion
startup benchmark fail if the measured cold setup reaches or exceeds it. The
latest release measurement is 35.476 ms, leaving about 4.5 ms of headroom. The
Criterion run measured 3.995 ms after the Metal device was already initialized,
so the release startup command is the authoritative cold-start number.

## Memory evidence

At 2880×1800, one BGRA8 retained framebuffer is:

```text
2880 × 1800 × 4 = 20,736,000 bytes ≈ 19.8 MiB
```

The headless renderer resource estimate was approximately 25.0 MiB before a
retained surface and 45.7 MiB with the retained-surface experiment. The
retained active-cell path therefore adds about 20.7 MiB before allocator and
driver overhead, plus a final surface copy and additional lifecycle state.
Its benchmark result was workload-sensitive: active-cell dispatch reduced
thread count substantially in some traces, but sparse end-to-end wall/GPU
time was not consistently better.

The retained-surface experiment stored one full BGRA framebuffer. It was
removed from production after measurement.

The timestamp atlas LRU stores one `u64` access timestamp per possible fixed
slot. For an 8×16 font cell size the atlas has 16,384 fixed slots, or roughly
128 KiB before allocator overhead. The intrusive alternative used two `u32`
link arrays and an `Option<GlyphKey>` array, roughly 256 KiB at that size.
Eviction is a cold path, so the extra memory and hot-path work were not
justified by O(1) eviction.

The regular/bold ASCII tables add only a few hundred bytes of CPU state. The
additional bold glyph pixels are small relative to the fixed 4 MiB atlas
texture and remove a hash lookup from the common bold ASCII path.

Variable-width packing did not reduce the fixed 4 MiB texture allocation. It
improved capacity but added occupancy and width bookkeeping, so it was removed
in favor of the fixed two-cell slot contract.

## Reversion measurements

After removing the retained framebuffer and variable-width packing, the
release startup benchmark reported:

```text
                         retained/packed   current fixed/full   delta
total startup                 38.884 ms          26.670 ms      -12.214 ms
Metal setup                  26.394 ms          15.821 ms      -10.573 ms
atlas preload                 4.779 ms           4.560 ms       -0.219 ms
```

The retained framebuffer's exact allocation at 2880×1800 was 20,736,000
bytes (19.8 MiB). The active-cell buffers and blit state were additional, but
small by comparison. The fixed-slot reversion removes the occupancy and width
arrays: at an 8×16 cell size this is 65,536 bytes; the fixed atlas texture
remains 4 MiB either way.

These numbers are startup/resource measurements, not a claim that every GPU
frame is 10 ms faster. The retained path was workload-sensitive and did not
produce a stable end-to-end win in the existing replay benchmarks. The simpler
full cell-tiled path is now the production choice.

The post-revert full cell-tiled Criterion samples were 1.155 ms for the dense
tmux replay and 1.130 ms for the sparse replay. These timings include the
shader's one-cell load optimization and exclude the retained-surface blit.
The retained active-cell numbers remain in the benchmark suite for comparison,
but are not used by the production renderer.

The current representative Metal workload run measured, for three-frame
sequences at 148×44, approximately 156 KiB uploaded for a full redraw, 3.5 KiB
for a one-row update, 20.8 KiB for sparse multi-row updates, and 17.3 KiB for a
selection update. GPU and wall times vary with scheduler load, so these values
are baselines for regression comparison rather than fixed performance claims.

The full Criterion run shows the current bottleneck clearly:

```text
canonical CPU parse                 16.25 µs
cell-tiled dense frame               1.085 ms
cell-tiled sparse frame              1.085 ms
reference full-frame shader          0.603 ms
cell-tiled damage research path      1.099 ms
```

The practical three-frame renderer cases are roughly 3.25–3.5 ms per frame,
while upload preparation remains around microseconds. Sparse updates therefore
do not currently reduce the dominant GPU cost: production still renders the
whole 148×44 surface. The next optimization target should be the cell-tiled
compute path and its threadgroup/branching overhead, measured against the
reference shader for pixel equivalence. More CPU upload bookkeeping is not
worth pursuing until it changes end-to-end GPU or wall time.

A focused experiment changed each cell team to 16×16 pixel subgroups while
preserving the same output and border behavior. Pixel-equivalence tests passed,
but canonical 148×44 tiled time regressed from about 1.085 ms to 1.500 ms per
frame, a 38% increase, for both dense and sparse replays. The experiment was
reverted. The larger font-sized threadgroups are therefore the current choice
on this GPU; smaller groups should not be revisited without a different kernel
design or hardware target.

The intrusive LRU result does not justify its extra metadata: it made the hot
lookup path about 3.7× slower to save less than a microsecond on a rare
full-atlas eviction. The timestamp policy is the better memory-conscious
default.

## PTY offthread (commit 9a42892)

PTY reading and parsing was moved from the main thread to a dedicated worker
thread. The worker now owns its own parser, grid, and scrollback; a
`WakePipe` (non-blocking pipe pair) signals the main thread when data is
available. The main thread copies the worker state into the render grid each
frame via `sync_worker`, then resolves atlas positions for non-ASCII glyphs
on the main thread.

The worker uses a no-op `GlyphAtlas` implementation (no atlas lookups or GPU
access) and a no-op `Rasterize` implementation. This keeps the worker
entirely offline from Metal and the renderer; only the main thread touches
the GPU.

The `GlyphAtlas` trait extracted from `Atlas` allows this split without
conditionally compiling the performer. The trait is lightweight and does not
add measurable overhead to the existing atlas hot paths.

### Binary size

```text
before (19a8b35):  1,307,008 bytes
after  (9a42892):  1,334,176 bytes  (+27,168 bytes, +2.1%)
```

### Startup bench (does not include worker spawn)

The `--startup-bench` measurement is unchanged within noise — it does not
exercise the worker path. The production startup adds an additional grid, an
additional scrollback, a WakePipe, and a thread spawn.

### Additional allocations (production startup, 151×47)

```text
duplicate grid cells:  ~57 KiB  (7,097 × 8 bytes)
duplicate grid chars:  ~28 KiB  (7,097 × 4 bytes)
duplicate scrollback:  lazy-allocated (minimal at startup)
WakePipe:              2 fds
```

Total additional resident memory is approximately 85 KiB plus thread-kernel
overhead. It is not expected to grow meaningfully with larger terminal sizes.

### Criterion benchmarks

Parser, grid, SIMD, and metal-replay benchmarks are unchanged — they exercise
the same direct parsing paths as before. The metal replay suite now includes
an `assert_worker_handoff_matches_direct` correctness check that verifies
pixel equivalence between the direct and worker-handoff paths, but this does
not affect the benchmark timing.

### Main loop impact

The worker reduces main-thread work: parsing and PTY I/O no longer compete
with rendering and event handling. The per-frame cost is a mutex lock on the
worker state, a combined grid+scrollback copy, dirty-state merge, and a
single pass of glyph atlas resolution for non-ASCII cells. Dirty-row
coalescing from the main grid survives the handoff: rows that were already
dirty before `sync_worker` remain dirty after. This avoids re-uploading cells
that would have been uploaded anyway.

## Design decisions

Keep:

- one threadgroup-level cell load in the Metal shader;
- direct regular and bold ASCII lookup tables;
- on-demand glyph rasterization and atlas caching;
- dirty-row CPU uploads;
- simple data layouts and no per-frame heap allocation after warmup.

Prefer to remove or avoid:

- a retained full-size framebuffer in the production renderer;
- generic dirty-cell dispatch that requires a retained surface and final blit;
- intrusive or heap-based LRU structures when eviction is rare;
- extra packing metadata unless atlas exhaustion is observed.

The preferred production renderer is therefore the simple full cell-tiled
path, with direct ASCII tables and a compact timestamp-based eviction policy.
The retained active-cell path remains useful as benchmark and research
infrastructure, but it is not automatically a production win.

## Future work

Any future optimization should include:

1. a deterministic replay and pixel-equivalence check;
2. CPU upload, command encoding, GPU time, wall time, allocation, and resident
   memory measurements;
3. a comparison against the full cell-tiled renderer on dense, sparse, one-row,
   one-cell, cursor, resize, scrollback, and full-redraw workloads;
4. an explicit memory budget and a stop condition for added state.

Potential low-memory improvements include reducing redundant shader memory
accesses, keeping the atlas metadata compact, and improving dirty-row
coalescing without retaining another full-size surface. A retained-surface
design should only return if a longer representative workload demonstrates a
repeatable benefit that exceeds its memory and copy costs.
