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

The intrusive LRU result does not justify its extra metadata: it made the hot
lookup path about 3.7× slower to save less than a microsecond on a rare
full-atlas eviction. The timestamp policy is the better memory-conscious
default.

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
