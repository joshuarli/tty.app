# tty.app

standard issue terminal emulator

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
