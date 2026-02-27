# Etch — Code Architecture

A GPU-accelerated terminal emulator for macOS. Rust + Metal compute shaders, single-threaded event loop, SIMD-accelerated VT parser.

## Source Layout

```
src/
├── main.rs                 # Event loop, App struct, TermPerformer (Perform impl)
├── config.rs               # Compile-time constants (font, palette, padding)
├── input.rs                # Winit key events → VT byte sequences
├── parser/
│   ├── mod.rs              # Three-layer parser: SIMD → CSI fast → state machine
│   ├── simd.rs             # NEON vectorized printable ASCII scanner
│   ├── csi_fast.rs         # Inline parser for ~15 common CSI sequences
│   ├── state_machine.rs    # Full Paul Williams VT500 state machine
│   ├── table.rs            # Byte classification + state transition lookup table
│   ├── perform.rs          # Perform trait — parser-to-grid interface
│   ├── charset.rs          # DEC Special Graphics character mapping
│   └── utf8.rs             # Multi-byte UTF-8 decoder
├── pty/
│   └── mod.rs              # forkpty, non-blocking read/write, TIOCSWINSZ resize
├── renderer/
│   ├── metal.rs            # Metal device, double-buffered cell upload, compute dispatch
│   ├── shader.metal        # Per-pixel compute kernel: bg → glyph → decoration → cursor
│   ├── atlas.rs            # 2048×2048 R8Unorm glyph atlas with LRU eviction
│   ├── font.rs             # CoreText rasterization → 8-bit alpha bitmaps
│   └── mod.rs
└── terminal/
    ├── cell.rs             # Cell struct (repr(C), 16 bytes), CellFlags bitfield
    ├── grid.rs             # Grid (flat Vec<Cell>), dirty BitVec, scroll, alt-screen
    ├── scrollback.rs       # Ring buffer of evicted rows
    └── mod.rs
```

## Data Flow

```
PTY fd (non-blocking read, 64KB buffer)
  │
  ▼
Parser.parse()
  ├─ Layer 1: SIMD scanner — finds runs of printable ASCII (0x20..0x7E)
  │   64 bytes/iter on ARM NEON, scalar tail for remainder
  ├─ Layer 2: CSI fast path — handles ESC[ sequences inline, no state machine
  │   Returns None on anything unusual → falls through to layer 3
  └─ Layer 3: State machine — full VT500 (table.rs transition table)
      Handles DCS, OSC, ESC dispatch, general CSI fallback
  │
  ▼
TermPerformer (implements Perform trait)
  Bridges parser actions → Grid mutations
  Also handles: atlas glyph insertion, scrollback pushes, response buffer
  │
  ▼
Grid.cells: Vec<Cell>  (row-major, flat)
  dirty: BitVec — one bit per row, set on any mutation
  │
  ▼
MetalRenderer.render_frame()
  1. Check dirty.any() — skip frame if nothing changed
  2. Spin-wait for GPU to release current buffer (AtomicBool)
  3. ptr::copy_nonoverlapping(grid.cells → cell_buffer)  ← bulk memcpy, ~5μs
  4. Dispatch compute shader (one thread per output pixel, 16×16 threadgroups)
  5. add_completed_handler → set buffer_ready = true
  6. commit() — returns immediately, GPU works async
  7. Swap to other cell buffer for next frame
```

## Key Design Decisions

### Cell ≡ CellData (zero-copy GPU upload)

`Cell` (CPU) and `CellData` (GPU) are both `#[repr(C)]`, 16 bytes, identical layout:

```
offset  type   field
0       u16    codepoint
2       u16    flags (bitflags)
4       u8     fg_index    (0xFF = use fg_rgb)
5       u8     bg_index    (0xFF = use bg_rgb)
6       u8     atlas_x     (glyph position in atlas grid)
7       u8     atlas_y
8       u32    fg_rgb      (0x00RRGGBB)
12      u32    bg_rgb
```

This allows the entire grid to be uploaded with a single memcpy. Bold brightness remapping (`palette 0-7 → 8-15`) and hidden attribute (`fg = bg`) are handled in the shader, not CPU-side, to preserve layout identity.

### Double-buffered async GPU

Two cell buffers alternate. CPU writes to buffer A while GPU reads buffer B. An `AtomicBool` per buffer is set by a Metal completed handler when the GPU finishes. The render path spin-waits if the target buffer is still in flight (rare in practice — GPU is at most one frame behind).

### Synchronized output (Mode 2026)

When an application sets Mode 2026 (e.g., tmux during pane switch), `render()` returns early and dirty bits accumulate. When the mode is cleared, the next frame renders all accumulated changes atomically. 100ms timeout prevents display freeze from misbehaving apps.

### Parser layering

The three layers are a performance hierarchy. Layer 1 (SIMD) handles bulk text at ~16 GB/s on Apple Silicon. Layer 2 (CSI fast) avoids state machine overhead for the ~15 sequences that account for >95% of CSI traffic. Layer 3 (state machine) handles everything else faithfully. The parser maintains state across `parse()` calls — partial sequences at buffer boundaries resume correctly.

### Pending wrap (DECAWM)

When the cursor reaches the last column, the wrap is *deferred* (`cursor_pending_wrap = true`). The next printable character triggers the actual wrap. This matches VT100 behavior and is critical for applications that write to the last column without wanting a wrap (e.g., status bars).

## Threading Model

Single-threaded. The winit event loop runs everything:

- `new_events()` → `process_pty_output()` — drains PTY, feeds parser
- `RedrawRequested` → `render()` — drains PTY again, then uploads grid, dispatches GPU
- `about_to_wait()` → `request_redraw()` — continuous polling at vsync

The PTY fd is set to `O_NONBLOCK`. Reads happen synchronously in the event loop, returning `WouldBlock` when empty. PTY data is drained both in `new_events()` and at the top of `render()` — the second drain catches data that arrived between the two calls, which is critical for apps that don't use synchronized updates (mode 2026) like htop and tmux. GPU work is the only thing that runs asynchronously (via Metal command buffer).

## Module Details

### main.rs

`App` struct owns all state. `SharedState` groups the grid, scrollback, response buffer, and alive flag. `TermPerformer` is a short-lived borrow of these components, created per PTY read chunk.

The `Perform` trait implementation in `TermPerformer` covers:
- Character printing (ASCII runs + single Unicode chars with charset translation)
- All cursor movement (relative, absolute, save/restore)
- Erase operations (display, line, characters)
- Scrolling (up/down within scroll region)
- Line/character insert/delete
- SGR parsing (8-color, 256-color, 24-bit RGB; bold/dim/italic/underline/inverse/hidden/strike)
- Mode set/reset (DECSET/DECRST: autowrap, cursor visible, alt-screen, mouse, bracketed paste, sync output)
- OSC dispatch (window title, clipboard via OSC 52)
- ESC dispatch (charset selection, save/restore cursor, RIS)
- Device status report (cursor position response)

### parser/table.rs

256-entry byte class table maps each byte to one of ~24 equivalence classes. The state transition table is `[14 states][24 classes]` of packed bytes where `(action << 4) | next_state`. This gives O(1) lookup per byte with no branching.

### parser/csi_fast.rs

Parses CSI parameters inline (up to 16 semicolon-separated u16 values). Bails to state machine on: colon sub-parameters, intermediate bytes, unrecognized final bytes. The `?` prefix for private modes is detected and passed through.

### terminal/grid.rs

The grid is a flat `Vec<Cell>` indexed as `row * cols + col`. Scroll operations use `copy_within` (memmove). Alt-screen swap is a `std::mem::swap` of the cell vectors. Resize preserves content where possible and rebuilds tab stops.

`TermMode` bitflags track all DECSET modes. `SavedCursor` captures cursor position + SGR attributes + charset state for DECSC/DECRC and alt-screen transitions.

### renderer/atlas.rs

Grid-based packing in a 2048×2048 texture. ASCII glyphs (0x20-0x7E) are pre-loaded and pinned (never evicted). Non-ASCII glyphs use LRU eviction based on a frame counter. Each slot holds one cell-sized glyph (or two cells wide for CJK).

### renderer/shader.metal

The compute kernel processes every pixel in the framebuffer:
1. Map pixel → grid cell via integer division
2. Padding region → default background
3. Wide continuation cells → sample right half of owner's glyph
4. Bold → remap palette index 0-7 to 8-15
5. Resolve fg/bg colors (palette lookup or RGB unpack)
6. Hidden → fg = bg; Inverse → swap fg/bg; Selected → swap fg/bg
7. Box drawing (U+2500..U+257F) → procedural from lookup table, not atlas
8. Normal glyphs → alpha blend from atlas texture
9. Underline/strikethrough → 1px horizontal lines
10. Cursor → full color inversion

### config.rs

All compile-time constants. The 256-color palette is computed in a `const` block (ANSI 0-15, 6×6×6 cube, 24-step grayscale). No runtime config file.

### pty/mod.rs

Uses `nix::pty::forkpty()`. Child process execs the user's `$SHELL` (or `/bin/zsh`) as a login shell. Sets `TERM=xterm-256color`. Master fd is `O_NONBLOCK`. Drop sends `SIGHUP` to the child.
