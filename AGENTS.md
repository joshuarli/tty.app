# tty — Code Architecture

A GPU-accelerated terminal emulator for macOS. Rust + Metal compute shaders, single-threaded event loop, SIMD-accelerated VT parser.

## Source Layout

```
src/
├── main.rs                 # Event loop, App struct, multi-window kqueue orchestration
├── performer.rs            # TermPerformer — Perform trait impl bridging parser → grid
├── unicode.rs              # is_wide() / is_zero_width() codepoint classification
├── lib.rs                  # Re-exports config, parser, terminal as public modules
├── config.rs               # Compile-time constants (font, palette, padding)
├── clipboard.rs            # macOS pasteboard access + base64 decode (OSC 52)
├── input.rs                # Key/mouse events → VT byte sequences
├── window.rs               # Native macOS windowing (objc2 AppKit), event polling, fullscreen
├── parser/
│   ├── mod.rs              # Three-layer parser: SIMD → CSI fast → state machine
│   ├── simd.rs             # NEON vectorized printable ASCII scanner
│   ├── csi_fast.rs         # Inline parser + shared dispatch table for CSI sequences
│   ├── state_machine.rs    # Full Paul Williams VT500 state machine
│   ├── table.rs            # Byte classification + state transition lookup table
│   ├── perform.rs          # Perform trait — parser-to-grid interface
│   ├── charset.rs          # DEC Special Graphics character mapping
│   └── utf8.rs             # Multi-byte UTF-8 decoder with cross-boundary buffering
├── pty/
│   └── mod.rs              # forkpty, non-blocking read/write, TIOCSWINSZ resize
├── renderer/
│   ├── metal.rs            # Metal device, double-buffered per-dirty-row upload, compute dispatch
│   ├── shader.metal        # Per-pixel compute kernel: bg → glyph → decoration → cursor
│   ├── atlas.rs            # 2048×2048 R8Unorm glyph atlas with LRU eviction
│   ├── font.rs             # CoreText rasterization → 8-bit alpha bitmaps
│   └── mod.rs
└── terminal/
    ├── cell.rs             # Cell struct (repr(C), 8 bytes) — IS the GPU format directly
    ├── grid.rs             # Ring buffer grid, dirty BitVec, scroll, alt-screen, cursor helpers
    ├── scrollback.rs       # Ring buffer of evicted rows
    └── mod.rs
```

## Data Flow

```
PTY fd (non-blocking read, 64KB buffer)
  │  drain until WouldBlock, then 500µs kqueue coalesce loop
  │
  ▼
Parser.parse()
  ├─ Layer 1: SIMD scanner — finds runs of printable ASCII (0x20..0x7E)
  │   64 bytes/iter on ARM NEON, scalar tail for remainder
  ├─ Layer 2: CSI fast path — handles ESC[ sequences inline, no state machine
  │   Returns None on anything unusual → falls through to layer 3
  └─ Layer 3: State machine — full VT500 (table.rs transition table)
      Handles DCS, OSC, ESC dispatch, CSI fallback (csi_dispatch delegates to fast path)
  │
  ▼
TermPerformer (implements Perform trait)
  Bridges parser actions → Grid mutations
  Resolves atlas coords at write time (not render time):
    - ASCII runs: Grid's internal ascii_atlas[128] table → O(1) per byte
    - Non-ASCII: atlas.get_or_insert() → rasterize if needed, return (x, y)
  Also handles: scrollback pushes, response buffer
  │
  ▼
Grid (ring buffer)
  cells: Vec<Cell> — ring_offset maps logical → physical rows via modular arithmetic
  dirty: BitVec — one bit per row, set on any mutation
  Full-screen scroll: bumps ring_offset, O(cols) to clear one row
  Partial scroll (within region): copy_within for affected rows
  │
  ▼
MetalRenderer.render_frame()
  1. Merge grid dirty bits → per-buffer pending bitsets (both buffers)
  2. Skip frame if no dirty rows and no deferred render
  3. If GPU still reading current buffer → defer to next frame (non-blocking)
  4. Per dirty row: ptr::copy_nonoverlapping(grid.row_slice() → cell_buffer)
     Cell IS the GPU format — no conversion, no packing loop
  5. Acquire next drawable (retry next frame if None)
  6. Dispatch compute shader (one thread per output pixel, 16×16 threadgroups)
  7. add_completed_handler → set buffer_ready = true
  8. commit() — returns immediately, GPU works async
  9. Swap to other cell buffer for next frame
```

## Key Design Decisions

### Cell = GPU format (8 bytes, zero-copy upload)

`Cell` is `#[repr(C)]`, 8 bytes, and IS the Metal shader's `CellData` format directly:

```
offset  type   field
0       u16    codepoint
2       u16    flags (CellFlags bitfield)
4       u8     fg_index   (palette index 0-255)
5       u8     bg_index   (palette index 0-255)
6       u8     atlas_x    (glyph position in atlas grid)
7       u8     atlas_y
```

Atlas coordinates are resolved once at write time — when a character is printed, the atlas position is looked up (or the glyph is rasterized) and stored directly in the Cell. This means the render path is pure memcpy with no per-cell work. Bold brightness remapping (`palette 0-7 → 8-15`) and hidden attribute (`fg = bg`) are handled in the shader, not CPU-side.

**The 8-byte Cell is a deliberate constraint.** Everything that doesn't fit is intentionally omitted rather than worked around:

- **No truecolor**: RGB values are mapped to nearest 256-color palette index via `rgb_to_palette()` at parse time. Storing 24-bit color would require widening Cell, breaking zero-copy GPU upload.
- **No combining marks**: Each cell holds one codepoint (u16 for BMP, sentinel + parallel vec for non-BMP). Diacritics and emoji ZWJ sequences are dropped. Variable-length cell storage would break the flat Vec<Cell> layout.
- **No runtime config**: Font, colors, and padding are compile-time constants. Avoids runtime config parsing and keeps Cell layout fixed.
- **No scrollback search/selection**: Scrollback stores raw Cell rows for zero-copy rendering, not searchable text.

These are accepted limitations of the fixed-size Cell design, not planned features.

### Ring buffer grid

The grid's `ring_offset` maps logical row 0 to a physical row in the flat `Vec<Cell>`. Full-screen scroll (the most common case — every newline at the bottom) just increments `ring_offset` and clears one row, avoiding an O(rows×cols) memmove. `row_start()` and `row_slice()` handle the modular arithmetic transparently.

Partial scrolls within a scroll region still use `copy_within` since only a subset of rows move.

### Per-dirty-row GPU upload

Each frame, only rows marked dirty are copied to the GPU buffer. The renderer maintains per-buffer pending bitsets (one per double-buffer slot) so that a row dirtied while the GPU reads buffer A is also queued for buffer B. Upload is `copy_nonoverlapping` per row — no per-cell packing loop, no format conversion.

### ASCII atlas table

Grid stores `ascii_atlas: [[u8; 2]; 128]` — atlas (x, y) for every ASCII codepoint. Set once after atlas preload. `write_ascii_run()` indexes this table per byte to fill atlas_x/atlas_y, avoiding HashMap lookups for the common case (printable ASCII is ~95% of terminal traffic).

### Double-buffered async GPU

Two cell buffers alternate. CPU writes to buffer A while GPU reads buffer B. An `AtomicBool` per buffer is set by a Metal completed handler when the GPU finishes. The render path skips the frame (non-blocking) if the target buffer is still in flight, setting `needs_render` to retry next iteration.

### Synchronized output (Mode 2026)

When an application sets Mode 2026 (e.g., tmux during pane switch), `render()` returns early and dirty bits accumulate. When the mode is cleared, the next frame renders all accumulated changes atomically. 100ms timeout prevents display freeze from misbehaving apps.

### Parser layering

The three layers are a performance hierarchy. Layer 1 (SIMD) handles bulk text at ~16 GB/s on Apple Silicon. Layer 2 (CSI fast) avoids state machine overhead for the ~25 sequences that account for >95% of CSI traffic. Layer 3 (state machine) handles everything else faithfully.

**Cross-boundary correctness**: The parser maintains state across `parse()` calls. When a CSI sequence spans two PTY reads (e.g., `ESC[` arrives in one read, `5;10H` in the next), the fast path cannot parse it (returns None). The state machine accumulates the bytes and dispatches the complete sequence via `csi_dispatch()`, which delegates to `CsiFastParser::dispatch()` for standard sequences. The UTF-8 assembler similarly buffers incomplete multi-byte sequences (2-4 bytes) across parse() boundaries and completes them on the next call.

### Pending wrap (DECAWM)

When the cursor reaches the last column, the wrap is *deferred* (`cursor_pending_wrap = true`). The next printable character triggers the actual wrap. This matches VT100 behavior and is critical for applications that write to the last column without wanting a wrap (e.g., status bars).

## Threading Model

Single-threaded. A manual `loop` in `main()` drives everything (no winit — raw `objc2` AppKit via `window.rs`):

```
loop {
    1. app.process_pty_output()   — drain PTY until WouldBlock
    2. PTY read coalescing        — 500µs kqueue poll loop for more data
    3. win.poll_events()          — drain AppKit events (keys, mouse, resize, focus)
    4. app.handle_event()         — translate events → PTY writes / state changes
    5. app.render()               — upload dirty rows, dispatch GPU
    6. if idle → kqueue wait      — block on PTY fd with 8ms timeout
}
```

The PTY fd is set to `O_NONBLOCK`. Reads happen synchronously in the event loop, returning `WouldBlock` when empty. GPU work is the only thing that runs asynchronously (via Metal command buffer).

When idle (no PTY data, no AppKit events, no GPU dispatch), the loop blocks on a kqueue watching the PTY fd with an 8ms timeout. This gives near-zero latency for shell output while still polling AppKit events at ~120Hz.

### PTY read coalescing (draw timeout)

After draining PTY data, the loop does a brief kqueue poll (500µs) before rendering. If more data arrives within that window, it's drained too, and the poll repeats. This coalesces rapid split writes from programs like tmux that hide the cursor, draw pane content, then show the cursor in separate `write()` calls. Without coalescing, the event loop (especially in release builds) is fast enough to render intermediate states — e.g., a frame with cursor hidden before the cursor-show arrives — causing visible flicker.

This is the same principle as alacritty's "draw timeout" but implemented differently:
- **Alacritty**: separate I/O thread for PTY reads, main thread renders on a scheduler timer. More complex (threads, channels, scheduler) but decouples I/O from rendering.
- **tty.app**: single-threaded with inline kqueue poll. Simpler, but the 500µs coalesce blocks the entire loop (no AppKit events processed during that window). Acceptable because 500µs is well under one frame at 120Hz (~8ms).

Mode 2026 (synchronized output) provides an additional layer: when BSU/ESU pairs wrap the output, rendering is deferred regardless of coalescing. The two mechanisms are complementary — mode 2026 handles apps that opt in, coalescing handles everything else.

## Module Details

### window.rs

Native macOS windowing via `objc2` / `objc2-app-kit` (no winit). `NativeWindow` owns an `NSApplication`, `NSWindow`, and a `TtyView` (minimal NSView subclass). Launches into native fullscreen with suppressed animation. Detects safe area insets for notch. `poll_events()` drains the AppKit event queue and returns a `Vec<Event>` covering: `KeyDown`, `ModifiersChanged`, `MouseDown/Up/Dragged`, `ScrollWheel`, `Resized`, `FocusIn/Out`, `Closed`. Key translation maps macOS virtual key codes to `NamedKey` variants and uses `charactersIgnoringModifiers` when Ctrl/Alt/Cmd are held.

### main.rs

`App` struct owns all state. `SharedState` groups the grid, scrollback, response buffer, and alive flag. Multi-window support: a `Vec<Terminal>` holds one `App` + `NativeWindow` per window, with a kqueue watching all PTY fds. Event routing matches NSEvents to windows by pointer identity. Cmd+N and dock menu spawn new terminals; dead terminals are reaped each frame.

### performer.rs

`TermPerformer` is a short-lived borrow of grid + scrollback + atlas + rasterizer + response_buf, created per PTY read chunk. Its `Perform` trait implementation covers:
- Character printing (ASCII runs via Grid's ascii_atlas + single Unicode chars with atlas lookup at write time)
- All cursor movement (relative, absolute, save/restore)
- Erase operations (display, line, characters)
- Scrolling (up/down within scroll region)
- Line/character insert/delete
- SGR parsing (8-color, 256-color, 24-bit RGB degraded to 256-color; bold/dim/italic/underline/inverse/hidden/strike). `sgr()` delegates simple codes to `sgr_single()` and only handles multi-param 38/48 extended color sequences itself.
- SGR colon sub-parameters (underline styles `4:N`, `38:2::R:G:B`, `48:2::R:G:B`)
- Mode set/reset (DECSET/DECRST: cursor keys, origin mode, autowrap, cursor visible, alt-screen, mouse, focus events, bracketed paste, sync output)
- OSC dispatch (window title, clipboard via OSC 52)
- ESC dispatch (charset selection, save/restore cursor, RIS, IND, NEL, RI, tab stop set)
- Device status report (cursor position response, DA1, DA2, DECRQM)
- `csi_dispatch()` handles only DA1, DA2, DECRQM, and private mode sequences directly; all other no-intermediate CSI sequences delegate to `CsiFastParser::dispatch()` (the shared dispatch table)

### parser/table.rs

256-entry byte class table maps each byte to one of ~24 equivalence classes. The state transition table is `[14 states][24 classes]` of packed bytes where `(action << 4) | next_state`. This gives O(1) lookup per byte with no branching.

### parser/csi_fast.rs

Parses CSI parameters inline (up to 16 semicolon-separated u16 values). Colon sub-parameters are handled inline for SGR sequences (dispatched to `performer.sgr_colon()` with the raw parameter bytes). Bails to state machine on: intermediate bytes (`0x20..0x2F`), unrecognized final bytes, incomplete sequences (buffer ends mid-sequence). The `?` prefix for private modes is detected and passed through.

**CSI dispatch architecture**: `CsiFastParser::dispatch()` is the single shared dispatch table for standard CSI sequences. It is called directly by the fast path for complete sequences, and also by `TermPerformer::csi_dispatch()` for split sequences that went through the state machine. `csi_dispatch()` handles only sequences that need `response_buf` access (DA1, DA2, DECRQM) or intermediate bytes (`?`, `>`, `$`), then delegates `([], _)` to `CsiFastParser::dispatch()`. This eliminates the previous duplication where the same dispatch table existed in both `csi_fast.rs` and the performer.

### terminal/grid.rs

The grid is a flat `Vec<Cell>` with ring buffer addressing: `row_start(logical_row)` computes `((logical_row + ring_offset) % rows) * cols`. Full-screen scroll increments `ring_offset` and clears the new bottom row. Partial scrolls within a scroll region use `copy_within` (memmove). Alt-screen swap is a `std::mem::swap` of the cell vectors and ring offsets. Resize flattens the ring buffer (copies to a new vec), preserves content where possible, and rebuilds tab stops. Common cursor operations — `backspace()`, `linefeed()`, `carriage_return()` — are Grid methods to centralize the cursor mutation + pending-wrap-clear pattern.

`TermMode` bitflags track all DECSET modes. `SavedCursor` captures cursor position + SGR attributes + charset state for DECSC/DECRC and alt-screen transitions.

### renderer/atlas.rs

Grid-based packing in a 2048×2048 texture. Each slot is one cell wide (`cell_width` pixels); wide CJK glyphs overflow into the adjacent slot's pixel space. ASCII glyphs (0x20-0x7E) are pre-loaded and pinned (never evicted). Non-ASCII glyphs use LRU eviction (the `frame` counter tracks last-access; `evict_lru()` finds the minimum). Font fallback uses `CTFontCreateForString` to find system fonts for missing glyphs. `ascii_table_raw()` exports the preloaded ASCII positions for Grid's `ascii_atlas` table.

### renderer/shader.metal

The compute kernel processes every pixel in the framebuffer:
1. Map pixel → grid cell via integer division
2. Padding region → default background
3. Wide continuation cells → sample right half of owner's glyph
4. Bold → remap palette index 0-7 to 8-15
5. Resolve fg/bg colors from 256-entry palette
6. Hidden → fg = bg; Inverse → swap fg/bg; Selected → swap fg/bg
7. Box drawing (U+2500..U+257F) → procedural from lookup table, not atlas
8. Normal glyphs → alpha blend from atlas texture
9. Underline/strikethrough → 1px horizontal lines
10. Cursor → full color inversion

### config.rs

All compile-time constants. The 256-color palette is computed in a `const` block (ANSI 0-15, 6×6×6 cube, 24-step grayscale). No runtime config file.

### pty/mod.rs

Uses `libc::forkpty()`. Child process execs the user's `$SHELL` (or `/bin/zsh`) as a login shell. Sets `TERM=xterm-256color`. Master fd is `O_NONBLOCK`. Drop sends `SIGHUP` to the child.
