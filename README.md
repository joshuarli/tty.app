# tty.app

The terminal emulator should be as simple as possible because
tmux gives you everything else you need: session management,
tabs (windows/panes), scrollback buffer, selection, clipboard,
search. Of course, we still implement basic amenities in case
tmux is not present.

- Apple Silicon only — Rust + native Metal compute shader
- 8-byte Cell is the Metal compute-buffer format — dirty rows are copied directly
- Ring buffer grid — full-screen scroll is O(cols) after the ring-offset update
- SIMD-accelerated VT parser (three-layer: NEON → CSI fast path → state machine)
- xterm-256color subset sufficient for tmux, vim, htop
- Single-threaded: non-blocking PTY I/O with kqueue, no mutexes

## Install

Edit `src/config.rs` and run `make install`.

### Design

The 8-byte Cell (`#[repr(C)]`: codepoint, flags, fg, bg, atlas_x, atlas_y) carries the data needed by the Metal shader. Glyph atlas coordinates are resolved when a character is written, not by a CPU-side conversion pass during rendering. ASCII uses a preloaded lookup table; uncached Unicode glyphs can still require rasterization and atlas-cache work.

- **CPU upload = memcpy.** Dirty rows are `copy_nonoverlapping`'d into a Metal shared buffer. There is no per-cell conversion or packing loop. The compute shader still processes the output pixels and samples the atlas.
- **Scroll = ring-offset update plus clearing.** Full-screen scroll avoids an O(rows × cols) memmove: it updates the ring offset and clears the newly exposed row, which is O(cols) for one line. Pushing that line into scrollback also copies the row.
- **Idle = no Metal dispatch.** Only dirty rows, cursor changes, or deferred frames cause a render dispatch. AppKit and kqueue polling still run while the application is idle.
- **Steady state can be allocation-free.** ASCII runs write directly into the Cell vec using a precomputed 128-entry atlas table. Scrollback rows reuse their allocations after the ring is full, and the PTY read buffer is reused. Initial scrollback growth and first-use Unicode glyphs can allocate.

The event loop is a manual single-threaded loop in `main()`: it drains PTY output, performs one 500µs coalescing poll when data arrived, drains AppKit events, handles input, renders, and sleeps on kqueue when idle. There is no async runtime or I/O thread; Metal command buffers execute asynchronously.

### Performance

The repository contains Criterion microbenchmarks for the parser, grid, scrollback, allocation behavior, and CPU-side cell copies. They are useful for tracking regressions, but they are not an end-to-end Metal or frame-rate benchmark: the parser benchmarks use a simplified performer, and the cell-copy benchmark copies into ordinary process memory rather than measuring a GPU transfer.

- **SIMD scanner:** NEON examines four 16-byte chunks per iteration on AArch64, with scalar handling for tails and unusual input.
- **Cell writes:** ASCII writes use the direct atlas table and fixed-size Cell fields; this is the common CPU hot path.
- **CPU-side upload:** dirty rows are raw copies into Metal shared storage. This removes packing work, but the current benchmark does not measure Metal GPU time.
- **Allocation behavior:** `Grid::new(80, 24)` currently reports four allocations totaling about 22.5 KiB in the allocation audit. Parsing output that fills scrollback and discovering glyphs are separate allocation cases.

Any throughput number should be reported with the Apple Silicon model, macOS version, compiler/profile, benchmark command, workload, and variance. The existing harness does not justify calling parser/grid numbers “end-to-end” or calling CPU memory-copy throughput “GPU upload.”

### Deliberate tradeoffs

The fixed-size 8-byte Cell is the foundation of the entire architecture — it enables direct-format copies into Metal shared storage, per-dirty-row uploads, and simple ring buffer scrolling. Everything that doesn't fit in 8 bytes is intentionally omitted:

- **No truecolor in cells** — RGB values are mapped to the nearest 256-color palette index at parse time. The PTY advertises `COLORTERM=truecolor`, but the renderer stores palette indices.
- **No combining marks or grapheme shaping** — each cell represents one codepoint. Standalone non-BMP characters use a parallel character store, while combining marks and ZWJ composition are ignored.
- **No runtime config** — font choice, colors, and padding are compile-time constants in `src/config.rs`. Hack is embedded, with CoreText fallback fonts for missing glyphs, but changing the configured font still requires recompilation.
- **No scrollback search or scrollback selection** — scrollback is stored as raw Cell rows for display, not as searchable text.

These are accepted limitations, not planned features.
