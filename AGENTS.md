# tty — Code Architecture

A GPU-accelerated terminal emulator for macOS. Rust + a native Metal compute renderer, a single-threaded application loop, and an AArch64-optimized VT parser.

## Source Layout

```
src/
├── main.rs                 # Event loop, App struct, multi-window kqueue orchestration
├── app_render.rs           # Synchronized-output timeout and renderer coordination
├── performer.rs            # TermPerformer — Perform trait impl bridging parser → grid
├── perform_shared.rs       # Shared parser actions used by the production and test performers
├── unicode.rs              # is_wide() / is_zero_width() codepoint classification
├── lib.rs                  # Public module re-exports and renderer traits
├── config.rs               # Compile-time constants (font, palette, padding)
├── clipboard.rs            # macOS pasteboard access + base64 decode (OSC 52)
├── input.rs                # Key/mouse events → VT byte sequences
├── window.rs               # Native macOS windowing (objc2 AppKit), event polling, fullscreen
├── renderer_trait.rs        # Renderer interface used by App and mock renderers
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
│   ├── metal.rs            # Metal device, shared buffers, per-dirty-row upload, cell-tiled dispatch
│   ├── core.rs             # Reusable headless/onscreen Metal device, pipeline, queue, palette setup
│   ├── shader.metal        # Per-pixel and cell-tiled compute kernels
│   ├── atlas.rs            # 2048×2048 R8Unorm glyph atlas with pinned ASCII and runtime eviction
│   ├── font.rs             # Embedded Hack + CoreText fallback rasterization → 8-bit alpha bitmaps
│   └── mod.rs
└── terminal/
    ├── cell.rs             # Cell struct (repr(C), 8 bytes) — matches the Metal compute-buffer ABI
    ├── grid.rs             # Ring buffer grid, dirty BitVec, scroll, alt-screen, cursor helpers
    ├── scrollback.rs       # Ring buffer of evicted rows
    └── mod.rs
```

## Data Flow

```
PTY fd (non-blocking read, reusable 64KB buffer, 256KB per-frame budget)
  │  drain until WouldBlock, then one 500µs kqueue coalescing poll
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
  Full-screen scroll: bumps ring_offset, O(cols) to clear one newly exposed row
  Partial scroll (within region): copy_within for affected rows
  │
  ▼
MetalRenderer.render_frame()
  1. Merge grid dirty bits → per-buffer pending row bitsets (both buffers)
  2. Skip frame if no dirty rows and no deferred render
  3. If GPU still reading current buffer → defer to next frame (non-blocking)
  4. Per dirty row: ptr::copy_nonoverlapping(grid.row_slice() → shared cell buffer)
     Cell matches the compute-buffer ABI — no CPU conversion or packing loop
  5. Acquire next drawable (retry next frame if None)
  6. Dispatch the cell-tiled compute shader, one threadgroup per terminal cell
  7. add_completed_handler → set buffer_ready = true
  8. commit() and waitUntilScheduled() — submission returns after scheduling; GPU work completes asynchronously
  9. Swap to other cell buffer for next frame
```

## Key Design Decisions

### Cell = Metal compute-buffer ABI (8 bytes)

`Cell` is `#[repr(C)]`, exactly 8 bytes, and matches the Metal shader's `CellData` structure directly:

```
offset  type   field
0       u16    codepoint
2       u16    flags (CellFlags bitfield)
4       u8     fg_index   (palette index 0-255)
5       u8     bg_index   (palette index 0-255)
6       u8     atlas_x    (glyph position in atlas grid)
7       u8     atlas_y
```

Atlas coordinates are resolved at write time — when a character is printed, the atlas position is looked up or the glyph is rasterized, then stored in the Cell. The CPU upload path is therefore a row-wise memcpy with no packing loop. The cell-tiled compute shader samples the atlas for normal glyphs and draws decorations procedurally. Bold brightness remapping (`palette 0-7 → 8-15`) and hidden attribute (`fg = bg`) are handled in the shader, not CPU-side.

**The 8-byte Cell is a deliberate constraint.** Everything that doesn't fit is intentionally omitted rather than worked around:

- **No truecolor in cells**: RGB values are mapped to the nearest 256-color palette index via `rgb_to_palette()` at parse time. Storing 24-bit color would widen Cell and remove the direct ABI match.
- **No combining marks or grapheme shaping**: Each cell represents one codepoint. BMP codepoints live in Cell; non-BMP codepoints use a parallel `Vec<char>` sentinel path. Combining marks and ZWJ composition are ignored.
- **No runtime config**: Font choice, colors, and padding are compile-time constants. Hack is embedded, with CoreText fallback fonts for missing glyphs, but changing the configured font still requires recompilation.
- **No scrollback search/selection**: Scrollback stores raw Cell rows for display, not searchable text.

These are accepted limitations of the fixed-size Cell design, not planned features.

### Ring buffer grid

The grid's `ring_offset` maps logical row 0 to a physical row in the flat `Vec<Cell>`. Full-screen scroll (the most common case — every newline at the bottom) increments `ring_offset` and clears one newly exposed row, avoiding an O(rows×cols) memmove. The clear is O(cols), and pushing the evicted row into scrollback copies the row. `row_start()` and `row_slice()` handle the modular arithmetic transparently.

Partial scrolls within a scroll region still use `copy_within` since only a subset of rows move.

### Per-dirty-row GPU upload

Each frame, only rows marked dirty are copied into Metal shared storage. The renderer maintains per-buffer pending bitsets (one per double-buffer slot) so that a row dirtied while the GPU reads buffer A is also queued for buffer B. The copy is `copy_nonoverlapping` per row — no per-cell packing loop or CPU format conversion. This is a CPU-side copy into shared GPU-visible memory, not a measured GPU transfer.

### ASCII atlas table

Grid stores `ascii_atlas: [[u8; 2]; 128]` — atlas (x, y) for every ASCII codepoint. It is set once after atlas preload. `write_ascii_run()` indexes this table per byte to fill atlas_x/atlas_y, avoiding HashMap lookups for the common ASCII path.

### Embedded Hack font

Hack Regular is vendored in `vendor/hack/Hack-Regular.ttf` with its license and embedded with `include_bytes!` in `renderer/font.rs`. `FontRasterizer` creates a `CGFont` from those in-memory TTF bytes, so users do not need Hack installed. Glyphs are still rasterized through CoreText/CoreGraphics into the runtime atlas: ASCII is preloaded at startup and non-ASCII glyphs are inserted on first use. System fallback fonts are checked only when embedded Hack does not contain the codepoint, preserving support for glyphs outside Hack's coverage without putting fallback fonts in the binary.

### Double-buffered async GPU

Two cell buffers alternate. CPU writes to buffer A while GPU reads buffer B. An `AtomicBool` per buffer is set by a Metal completed handler when the GPU finishes. The render path skips the frame (non-blocking) if the target buffer is still in flight, setting `needs_render` to retry next iteration.

### Synchronized output (Mode 2026)

When an application sets Mode 2026 (e.g., tmux during pane switch), `render()` returns early and dirty bits accumulate. When the mode is cleared, the next frame renders all accumulated changes atomically. 100ms timeout prevents display freeze from misbehaving apps.

### Parser layering

The three layers are a performance hierarchy. Layer 1 (SIMD) handles long printable runs in 64-byte AArch64 batches. Layer 2 (CSI fast) avoids state-machine overhead for common complete CSI sequences. Layer 3 (state machine) handles split, unusual, and otherwise unsupported fast-path input. Throughput claims belong in benchmark results with workload and machine details; the checked-in benchmarks do not measure the complete PTY-to-Metal path.

**Cross-boundary correctness**: The parser maintains state across `parse()` calls. When a CSI sequence spans two PTY reads (e.g., `ESC[` arrives in one read, `5;10H` in the next), the fast path cannot parse it (returns None). The state machine accumulates the bytes and dispatches the complete sequence via `csi_dispatch()`, which delegates to `CsiFastParser::dispatch()` for standard sequences. The UTF-8 assembler similarly buffers incomplete multi-byte sequences (2-4 bytes) across parse() boundaries and completes them on the next call.

### Pending wrap (DECAWM)

When the cursor reaches the last column, the wrap is *deferred* (`cursor_pending_wrap = true`). The next printable character triggers the actual wrap. This matches VT100 behavior and is critical for applications that write to the last column without wanting a wrap (e.g., status bars).

## Threading Model

The application logic is single-threaded. A manual `loop` in `main()` drives everything (no winit — raw `objc2` AppKit via `window.rs`):

```
loop {
    1. app.process_pty_output()   — drain PTY until WouldBlock
    2. PTY read coalescing        — one 500µs kqueue poll for more data
    3. AppKit event drain         — route key, mouse, resize, focus, and close events
    4. app.handle_event()         — translate events → PTY writes / state changes
    5. app.render()               — upload dirty rows, dispatch GPU
    6. if idle → kqueue wait      — block on PTY fd with 8ms timeout
}
```

The PTY fd is set to `O_NONBLOCK`. Reads happen synchronously in the event loop, returning `WouldBlock` when empty. `Arc<Pty>` provides shared ownership for window/app lifecycle; there is no PTY I/O thread. Metal command buffers and their completion handlers execute asynchronously.

When idle (no PTY data, no AppKit events, no GPU dispatch), the loop blocks on a kqueue watching the PTY fd with an 8ms timeout. This gives near-zero latency for shell output while still polling AppKit events at ~120Hz.

### PTY read coalescing (draw timeout)

After draining PTY data, the loop does one brief kqueue poll (500µs) before rendering. If more data arrives within that window, it drains one more budgeted batch. This coalesces rapid split writes from programs like tmux that hide the cursor, draw pane content, then show the cursor in separate `write()` calls. Without coalescing, the event loop (especially in release builds) is fast enough to render intermediate states — e.g., a frame with cursor hidden before the cursor-show arrives — causing visible flicker.

This is the same principle as alacritty's "draw timeout" but implemented differently:
- **Alacritty**: separate I/O thread for PTY reads, main thread renders on a scheduler timer. More complex (threads, channels, scheduler) but decouples I/O from rendering.
- **tty.app**: single-threaded with inline kqueue poll. Simpler, but the 500µs coalesce blocks the entire loop (no AppKit events processed during that window). Acceptable because 500µs is well under one frame at 120Hz (~8ms).

Mode 2026 (synchronized output) provides an additional layer: when BSU/ESU pairs wrap the output, rendering is deferred regardless of coalescing. The two mechanisms are complementary — mode 2026 handles apps that opt in, coalescing handles everything else.

## Module Details

### window.rs

Native macOS windowing via `objc2` / `objc2-app-kit` (no winit). `NativeWindow` owns an `NSApplication`, `NSWindow`, and a `TtyView` (minimal NSView subclass). Launches into native fullscreen with suppressed animation and detects safe-area insets for the notch. The main loop drains NSEvents directly, routes events to the owning window, and asks each window to report resize, focus, and close state changes. Key translation maps macOS virtual key codes to `NamedKey` variants and uses `charactersIgnoringModifiers` when Ctrl/Alt/Cmd are held.

### main.rs

`App` struct owns terminal state. `SharedState` groups the grid, scrollback, response buffer, and child-alive flag. The reusable PTY buffer is 64 KiB; each frame applies a 256 KiB read budget so continuous output cannot starve input and rendering. Multi-window support: a `Vec<Terminal>` holds one `App` + `NativeWindow` per window, with a kqueue watching all PTY fds. Event routing matches NSEvents to windows by pointer identity. Cmd+N and dock menu spawn new terminals; dead terminals are reaped each frame.

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

Grid-based packing in a 2048×2048 texture. Each slot is one cell wide (`cell_width` pixels); wide CJK glyphs overflow into the adjacent slot's pixel space. ASCII glyphs (0x20-0x7E) are pre-loaded and pinned at startup. Runtime rasterized non-ASCII glyphs are stored in a hash map with pinned-ASCII protection and eviction bookkeeping. `ascii_table_raw()` exports the preloaded ASCII positions for Grid's `ascii_atlas` table.

### renderer/shader.metal

The production cell-tiled kernel uses one threadgroup per terminal cell and one
thread per cell pixel. The full-frame `render` kernel remains the reference path;
the active-cell-list and surface-copy kernels are benchmark-only prototype
infrastructure.

The cell shading logic:
1. Map pixel → grid cell via integer division
2. Padding region → default background
3. Wide continuation cells → sample right half of owner's glyph
4. Bold → remap palette index 0-7 to 8-15
5. Resolve fg/bg colors from 256-entry palette
6. Hidden → fg = bg; Inverse → swap fg/bg; Selected → swap fg/bg
7. Box drawing (U+2500..U+257F) → procedural from a lookup table, not atlas
8. Arrows (U+2190..U+2195) → procedural geometry
9. Normal glyphs → alpha blend from atlas texture
10. Underline/strikethrough → 1px horizontal lines
11. Cursor → RGB color inversion

### config.rs

All compile-time constants. Hack is the embedded default font, and CoreText fallback fonts cover missing glyphs. The 256-color palette is computed in a `const` block (ANSI 0-15, 6×6×6 cube, 24-step grayscale). There is no runtime config file.

### pty/mod.rs

Uses `libc::forkpty()`. Child process execs the user's `$SHELL` (or `/bin/zsh`) as a login shell. Sets `TERM=xterm-256color`. Master fd is `O_NONBLOCK`. Drop sends `SIGHUP` to the child.
