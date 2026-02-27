# tty.app: A Minimalist Terminal Emulator for macOS

## Context

Build a terminal emulator that outperforms Alacritty on all three axes: input latency, throughput, and power efficiency. Inspired by foot's damage tracking philosophy and Ghostty's SIMD parser, but with a novel compute-shader-only Metal renderer (no vertex pipeline). macOS-only, targeting Apple Silicon.

**Key insight from research**: Foot proves damage tracking matters. Ghostty proves SIMD parsing matters. But no existing terminal combines a compute-shader renderer (Zutty-style) with SIMD parsing and Apple Silicon unified memory. That's the gap.

## Decisions Made

- **Language**: Rust
- **Platform**: macOS only (Apple Silicon primary, Intel secondary)
- **Renderer**: Metal compute shader (one thread per pixel, no vertex pipeline)
- **VT Parser**: Custom 3-layer SIMD parser from scratch (NEON scanner + CSI fast-path + scalar state machine). No `vte` crate dependency.
- **Font rendering**: CoreText
- **Windowing**: winit (pragmatic, handles DPI/keyboard/lifecycle; drop to raw objc for CAMetalLayer)
- **v1 scope**: Bare minimum — xterm-256color, basic mouse, no images/tabs/splits

## Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                  Main Thread                     │
│  winit event loop → keyboard/mouse → PTY write  │
│  CAMetalLayer ← present drawable                 │
└────────┬──────────────────────────┬──────────────┘
         │                          │
    ┌────▼────┐              ┌──────▼──────┐
    │ I/O     │              │ Render      │
    │ Thread  │              │ Thread      │
    │         │   Grid+Dirty │             │
    │ PTY read├──────────────► Upload dirty│
    │ SIMD VT │   (mutex)    │ rows to GPU │
    │ parse   │              │ Dispatch    │
    │ Grid    │              │ compute     │
    │ mutate  │              │ Present     │
    └─────────┘              └─────────────┘
```

### Threading Model

- **Main thread**: winit event loop, AppKit, keyboard/mouse input → PTY write
- **I/O thread**: PTY read (kqueue), SIMD VT parsing, grid state mutation, dirty flag setting
- **Render thread**: Metal rendering — lock grid, upload dirty rows to GPU buffer, dispatch compute, present

## Project Structure

```
tty.app/
├── Cargo.toml
├── build.rs                    # Metal shader compilation + width table generation
├── src/
│   ├── main.rs                 # Entry point, winit bootstrap
│   ├── terminal/
│   │   ├── mod.rs
│   │   ├── grid.rs             # Cell grid, dirty tracking bitset
│   │   ├── cell.rs             # Cell struct (must match Metal CellData)
│   │   └── scrollback.rs       # Scrollback ring buffer
│   ├── parser/
│   │   ├── mod.rs              # Parser public API, 3-layer dispatch
│   │   ├── simd.rs             # NEON byte classifier + ASCII run scanner
│   │   ├── csi_fast.rs         # Optimistic CSI parser (SGR, cursor, erase)
│   │   ├── state_machine.rs    # Full VT state machine (Paul Williams)
│   │   ├── table.rs            # Packed transition table (375 bytes)
│   │   ├── perform.rs          # Perform trait (parser → grid interface)
│   │   ├── utf8.rs             # UTF-8 codepoint assembler
│   │   └── width.rs            # 3-stage trie codepoint width lookup
│   ├── renderer/
│   │   ├── mod.rs
│   │   ├── metal.rs            # Device, queue, pipeline, triple-buffer, dispatch
│   │   ├── atlas.rs            # Glyph texture atlas (shelf packing, LRU)
│   │   ├── font.rs             # CoreText rasterization → R8 alpha bitmaps
│   │   └── shader.metal        # THE compute shader
│   ├── pty/
│   │   └── mod.rs              # forkpty + kqueue non-blocking I/O
│   └── input.rs                # Key event → VT byte sequence translation
└── benches/
    ├── parser_bench.rs         # Isolated parser throughput (criterion)
    └── render_bench.rs         # Frame time measurement
```

## Phase 1: Metal Window + Colored Rectangle

**Goal**: winit window with CAMetalLayer, compute shader that fills the drawable with a solid color.

**Files**: `main.rs`, `renderer/metal.rs`, `renderer/shader.metal`, `build.rs`

**Crates**:
```toml
[dependencies]
metal = "0.32"               # Metal bindings
objc = "0.2"                 # Raw ObjC for CAMetalLayer setup
cocoa = "0.26"               # NSView
winit = "0.30"               # Event loop + window
raw-window-metal = "1.0"     # NSView → CAMetalLayer
bytemuck = { version = "1", features = ["derive"] }
bitflags = "2"
bitvec = "1"
```

**build.rs** — compile shader:
```rust
// xcrun -sdk macosx metal -c shader.metal -o shader.air -ffast-math -std=metal3.0
// xcrun -sdk macosx metallib shader.air -o shader.metallib
// Embed via include_bytes!(concat!(env!("OUT_DIR"), "/shader.metallib"))
```

**Key API sequence**: `Device::system_default()` → `MetalLayer::new()` → set pixel format BGRA8Unorm → set `StorageModeShared` → attach to NSView → `new_compute_pipeline_state_with_function()` → render loop

## Phase 2: Font Rasterization + Glyph Atlas

**Goal**: CoreText renders ASCII glyphs into a 2048x2048 R8Unorm texture atlas.

**Files**: `renderer/font.rs`, `renderer/atlas.rs`

- Rasterize via `CTFont` → `CGBitmapContext` → extract grayscale alpha channel
- Atlas is a flat grid of cells (monospace = uniform slot size, no complex packing)
- Slot count: (2048/cell_w) * (2048/cell_h) ≈ 8192 slots at Retina sizes
- Pre-rasterize at startup: ASCII 0x20-0x7E × 4 variants (regular/bold/italic/bold-italic) + box-drawing = ~700 glyphs, ~10ms on Apple Silicon
- LRU eviction for rare glyphs, ASCII permanently pinned
- `MTLStorageMode::Shared` — zero-copy CPU write, GPU read on Apple Silicon
- Upload via `replace_region()` for individual slot updates

## Phase 3: Cell Grid + Compute Shader Rendering

**Goal**: Render a static grid of colored text to screen.

**Files**: `terminal/cell.rs`, `terminal/grid.rs`, `renderer/shader.metal`

### CellData (16 bytes, GPU-side, must match Rust struct exactly)

```metal
struct CellData {
    ushort codepoint;      // Unicode BMP (or glyph key)
    ushort flags;          // [0:1] variant, [2] underline, [3] strike, [4] inverse, [5] cursor, [6] wide, [7] wide_cont
    uchar  fg_index;       // 0-255 xterm palette, 0xFF = use fg_rgb
    uchar  bg_index;       // 0-255 xterm palette, 0xFF = use bg_rgb
    uchar  atlas_x;        // glyph column in atlas grid
    uchar  atlas_y;        // glyph row in atlas grid
    uint   fg_rgb;         // 0x00RRGGBB (when fg_index == 0xFF)
    uint   bg_rgb;         // 0x00RRGGBB (when bg_index == 0xFF)
};
```

### Compute Shader Design

- **One thread per pixel** across the entire framebuffer
- Threadgroup: `(16, 16, 1)` = 256 threads (multiple of Apple Silicon SIMD width 32)
- Cell lookup: `col = pixel_x / cell_width; row = pixel_y / cell_height;`
- Single pass: background fill → glyph alpha blend → decorations → cursor
- Uses `texture.read()` not `sample()` (1:1 pixel mapping, no filtering unit overhead)
- xterm-256 palette as a uniform buffer (256 × float4 = 4KB)
- Cursor embedded in cell flags (no overlay pass)

### Triple-Buffered Frame Pipeline

```
3 CellData MTLBuffers (StorageModeShared + WriteCombined)
dispatch_semaphore_t(3) for frame pacing

Per frame:
  1. semaphore.wait()
  2. Upload dirty rows → cell_buffers[frame_idx]  (memcpy only changed rows)
  3. layer.next_drawable()  (blocks for vsync when displaySyncEnabled)
  4. Encode compute pass → dispatch_threads(framebuffer_size, (16,16,1))
  5. present_drawable + commit
  6. add_completed_handler { semaphore.signal() }
  7. If !anything_dirty → skip entire frame (zero GPU work)
```

### Dirty Tracking

- Row-level bitset (`bitvec`, one bit per row)
- Parser sets dirty flags as it mutates cells
- Renderer reads bitset under mutex, uploads only dirty rows (`cols × 16 bytes` per row)
- Clear after frame
- Scroll: `memmove` the cell buffer, mark only new content rows dirty

## Phase 4: PTY + Basic I/O

**Goal**: Spawn a shell, read output, display it.

**Files**: `pty/mod.rs`

- `forkpty()` + `posix_openpt()` for PTY creation
- `kqueue` for non-blocking PTY reads (macOS native, faster than poll/select)
- 64KB read buffer to batch reads
- Dedicated I/O thread: `kqueue wait → read → parse → mutate grid → notify renderer`
- Renderer notification: `AtomicBool` or condvar to wake render thread when dirty

## Phase 5: VT Parser (Scalar First, Then SIMD)

**Goal**: Full xterm-256color VT emulation.

**Files**: `parser/*.rs`

### Layer 0: NEON SIMD Scanner (`parser/simd.rs`)

```rust
use core::arch::aarch64::*;

// Classify 16 bytes: returns bitmask of positions needing state-machine attention
// Printable ASCII (0x20-0x7E) = 0 in bitmask (fast path)
// Control (<0x20), DEL (0x7F), high (>=0x80) = non-zero (need attention)
unsafe fn classify_chunk(ptr: *const u8) -> u64 {
    let chunk = vld1q_u8(ptr);
    let is_control = vcltq_u8(chunk, vdupq_n_u8(0x20));
    let is_high_or_del = vcgeq_u8(chunk, vdupq_n_u8(0x7F));
    let attention = vorrq_u8(is_control, is_high_or_del);
    // Pack to 64-bit bitmask via vshrn_n_u16 technique
    vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(attention))))
}
```

Outer loop: scan 64 bytes (4×16 unrolled) per iteration. When all-ASCII, call `performer.print_ascii_run()` for bulk grid copy. When a special byte is found, hand off to Layer 1 or 2.

### Layer 1: CSI Fast-Path (`parser/csi_fast.rs`)

Optimistic inline parser for the ~15 most common CSI sequences:
- SGR (`m`): colors, bold, italic, underline, reset
- Cursor movement: `A/B/C/D/H/G/d`
- Erase: `J/K`
- Scroll: `S/T`
- Insert/delete: `L/M/@/P`

Bail to Layer 2 on: DEC private modes (`?`), colon sub-params, intermediate bytes, unrecognized finals.

### Layer 2: Scalar State Machine (`parser/state_machine.rs`)

- Paul Williams VT500-compatible, 15 states, 14 actions
- Packed transition table: `STATE_TABLE[state][byte_class] → u8` (high nibble = action, low = next state)
- 25 byte equivalence classes × 15 states = 375 bytes (fits in 6 cache lines)
- Returns to Ground ASAP so SIMD scanner can resume

### Layer 3: Width Lookup (`parser/width.rs`)

- 3-stage trie: `STAGE1[cp >> 8] → STAGE2 → STAGE3` packed 2 bits per codepoint
- ~40KB total, O(1) lookup
- ASCII fast-path: `if cp < 0x80 { return 1; }`
- Generated at build time from Unicode data in `build.rs`
- Grapheme clusters: v1 handles zero-width combining marks only, full UAX#29 is v2

### Parser ↔ Grid Interface

```rust
trait Perform {
    fn print_ascii_run(&mut self, bytes: &[u8]);  // HOT PATH — bulk memcpy to grid
    fn print(&mut self, c: char);                   // Single Unicode char
    fn execute(&mut self, byte: u8);                // C0 controls
    fn cursor_up(&mut self, n: u16);                // ... etc for all CSI actions
    fn sgr_reset(&mut self);                        // ... etc for all SGR
    fn csi_dispatch(&mut self, ...);                // Fallback for Layer 2
    fn osc_dispatch(&mut self, data: &[u8]);
}
```

## Phase 6: Input Handling

**Goal**: Keyboard events → correct VT byte sequences → PTY write.

**Files**: `input.rs`

- Translate winit `KeyEvent` → xterm byte sequences
- Handle: arrow keys, function keys, modifiers, bracketed paste
- Respect DECCKM (cursor keys mode), DECKPAM (keypad mode)
- IME support can be deferred post-v1

## Phase 7: Scroll Optimization + Scrollback

**Goal**: Efficient scrolling, scrollback buffer.

**Files**: `terminal/scrollback.rs`, `terminal/grid.rs`

- Scroll = `memmove` the CellData array, mark new rows dirty
- Scrollback: ring buffer of rows (linked list of pages, foot-style, or flat ring)
- Configurable max scrollback (default 10,000 lines)
- On scroll: shift grid, push evicted rows to scrollback

## Phase 8: Polish

- Cursor blinking (timer-based dirty flag toggling)
- Selection (mouse drag → selection state → render highlight in shader via cell flag)
- Resize (recalculate grid dimensions, resize Metal buffers, signal PTY SIGWINCH)
- Bell (visual flash — set all cells dirty with inverted flag for one frame)
- Window title (OSC 0/2 → winit `set_title`)
- Config file (font, font size, colors, scrollback size)

## Verification Plan

1. **Parser correctness**: Run vttest (VT100/VT220 test suite) — all basic tests should pass
2. **Parser throughput**: `criterion` bench against a 10MB ASCII file and escape-heavy vtebench data. Target: match or beat Alacritty's `vte` crate throughput
3. **Render latency**: Instrument frame times with `mach_absolute_time()`. Target: <500μs for single-character-typed frames (dirty tracking pays off here vs Alacritty's full-grid redraw)
4. **Throughput end-to-end**: `time cat large_file.txt` — target: within 10% of Alacritty (parser-limited, not render-limited)
5. **Power**: Profile with Instruments Activity Monitor template during idle/typing. Target: near-zero CPU/GPU when idle (frame skip), minimal GPU wake for single-char updates
6. **Compatibility**: Launch `nvim`, `tmux`, `htop`, `git log --oneline`, colored `ls` output. All should render correctly.
