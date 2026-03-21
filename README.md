# tty.app

standard issue terminal emulator

The terminal emulator should be as simple as possible because
tmux gives you everything else you need: session management,
tabs (windows/panes), scrollback buffer, selection, clipboard,
search. Of course, we still implement basic amenities in case
tmux is not present.

- Apple Silicon only — Rust + Metal compute shader
- ~200 KB binary, zero runtime dependencies
- 8-byte Cell is the GPU format � dirty rows memcpy'd directly to Metal buffer
- Ring buffer grid — O(1) full-screen scroll
- SIMD-accelerated VT parser (three-layer: NEON → CSI fast path → state machine)
- xterm-256color subset sufficient for tmux, vim, htop
- Single-threaded: non-blocking PTY I/O with kqueue, no mutexes

### Design

The 8-byte Cell (`#[repr(C)]`: codepoint, flags, fg, bg, atlas_x, atlas_y) is resolved completely at parse time — glyph atlas coordinates are looked up when a character is printed, not when it's rendered. This means:

- **Render = memcpy.** Dirty rows are `copy_nonoverlapping`'d to the Metal buffer. No per-cell conversion, no packing loop, no shader-side glyph lookup.
- **Scroll = pointer bump.** The grid is a ring buffer. Full-screen scroll increments an offset and clears one row — O(cols), not O(rows × cols).
- **Idle = idle.** Only dirty rows are uploaded. Double-buffered: CPU writes one buffer while GPU reads the other. When nothing changes, nothing runs.
- **Parse once, allocate never.** ASCII runs write directly into the Cell vec using a precomputed 128-entry atlas table. Steady-state scrollback is zero-alloc (ring buffer reuses existing row vecs). The PTY read buffer is reused across frames.

The entire event loop — PTY drain, AppKit polling, coalesced rendering, kqueue sleep — is ~150 lines of straight-line code in `main()`. No async runtime, no threads, no channels.

### Performance

The hot paths are at or near hardware limits on Apple Silicon:

- **SIMD scanner: 56 GiB/s** — NEON classifies 64 bytes/iteration, bottlenecked by load-to-result latency, not memory bandwidth. Three-layer fallback (SIMD → CSI fast path → state machine) means the parser spends almost all time in the widest path.
- **Cell writes: ~3.5 cycles/cell** — the inner loop is 1 atlas load + 5 stores (hardware-coalesced by Apple Silicon's store buffer). Per-byte atlas lookup serializes the work; there is no wider path without a gather instruction ARM doesn't have.
- **GPU upload: 6.9 GB/s** — dirty rows are `copy_nonoverlapping`'d to the Metal shared buffer. The Cell *is* the GPU vertex format, so there's no conversion — just memcpy.
- **Zero steady-state allocation** — grid cells are pre-allocated, scrollback reuses ring buffer rows, PTY buffer is reused across frames. Only grid construction allocates (4 allocs, 22.5 KiB for 80x24).

Measured end-to-end: ASCII throughput 386 MiB/s, colored output (256-color SGR) 400 MiB/s, fullscreen TUI redraws at 710 MiB/s. The bottleneck is the cell write loop, not parsing — the parser outruns the grid by 10x.

### Deliberate tradeoffs

The fixed-size 8-byte Cell is the foundation of the entire architecture — it enables zero-copy GPU upload, per-dirty-row memcpy rendering, and simple ring buffer scrolling. Everything that doesn't fit in 8 bytes is intentionally omitted:

- **No truecolor** — RGB values are mapped to the nearest 256-color palette index at parse time. There is no room for 24-bit color in the Cell without breaking the zero-copy GPU upload.
- **No combining marks** — each cell holds one codepoint. Diacritics, emoji ZWJ sequences, and flag sequences are dropped. Supporting them would require variable-length cell storage.
- **No runtime config** — font, colors, and padding are compile-time constants in `src/config.rs`. Edit and recompile.
- **No scrollback search or scrollback selection** — scrollback is stored as raw Cell rows for zero-copy rendering, not as searchable text.

These are accepted limitations, not planned features.

## Custom Install

Edit `src/config.rs` and run `make install`

`brew install --cask font-hack`
