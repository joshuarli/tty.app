# Etch: Detailed Implementation Plan

## Overview

Minimalist terminal emulator for macOS. Compute-shader-only Metal renderer (no vertex pipeline), SIMD VT parser (NEON), Apple Silicon only. Goal: outperform Alacritty on input latency, throughput, and power efficiency.

## Decision Log

| # | Decision | Choice | Rationale |
|---|---|---|---|
| 1 | Atlas coords | `u8×u8`, 2048 atlas | 20K+ slots sufficient for v1. Revisit if needed. |
| 2 | Codepoint field | Keep `u16` in CellData | GPU doesn't use it but useful for debugging. BMP-only fine for v1. |
| 3 | Metal layer setup | Inline `raw-window-metal` logic | ~30 lines of objc, no extra dependency. |
| 4 | Grid sync | Single `Mutex` | Critical section is tiny (memcpy dirty rows). Upgrade if profiling shows contention. |
| 5 | DECSET scope | Full list + mode 2026 (synchronized output) | Enough for nvim/tmux/htop. Sync output prevents tearing. |
| 6 | Config | Compile-time `config.rs` constants | No runtime config parsing. Change and recompile. |
| 7 | Wide chars | Double-width atlas slots | Wide glyphs get 2×cell_width slot. Shader checks wide flag. |
| 8 | Bold | Bright colors only | No bold font variant in v1. Simplifies atlas (regular weight only). |
| 9 | Subpixel AA | Grayscale R8 only | Simpler shader, fine on Retina. macOS disabled subpixel AA in Mojave. |
| 10 | Resize reflow | No reflow | Resize changes grid dims + SIGWINCH. Wrapped lines stay wrapped. |
| 11 | Clipboard | Cmd+C/V + OSC 52 | NSPasteboard for GUI copy/paste. OSC 52 for programmatic (tmux/nvim over SSH). |
| 12 | Cursor shape | Block only | Ignore DECSCUSR. Single cursor style. |
| 13 | Font | Menlo 13pt | Ships with every macOS. |
| 14 | TERM | `xterm-256color` | Universal terminfo. No custom terminfo entry. |
| 15 | Scrollback | Ring buffer, fixed width | Pre-allocated. Old rows padded/truncated to current width. Cache-friendly. |
| 16 | Box drawing | Procedural in shader | Pixel-perfect alignment for U+2500–U+257F. Lookup table for edge connectivity. |
| 17 | Color scheme | Tweaked (Alacritty-style) | Softer ANSI colors. bg #1d1f21, fg #c5c8c6. |
| 18 | Selection | Simple linear model | Click-drag, double-click word, triple-click line. Rendered as inverted fg/bg. |
| 19 | Window padding | Configurable, 8px default | Constant in config.rs. Shader offsets grid origin. |
| 20 | Transparency | Opaque only | layer.setOpaque(true). No alpha compositing. |
| 21 | Shell exit | Close window immediately | Shell exits → process exits. |
| 22 | Framerate | Display-synced + frame skip | 60Hz/120Hz via displaySyncEnabled. Zero GPU work when idle. |
| 23 | DEC graphics | G0/G1 character set support | 96-entry lookup table. Required for tmux/ncurses. |
| 24 | DPI change | Eager atlas rebuild | ScaleFactorChanged → clear + rebuild atlas (~10ms). |
| 25 | Option key | Always Meta | Both Option keys send ESC prefix. Simplest. Matches Alacritty. |
| 26 | Cmd shortcuts | Cmd+C/V only | Clipboard only. Cmd+Q/W via AppKit defaults. |
| 27 | Window size | Native fullscreen | Launch into macOS borderless fullscreen. Grid auto-sizes. |
| 28 | Intel support | Apple Silicon only | NEON-only SIMD, no SSE2 fallbacks. StorageMode::Shared everywhere. |
| 29 | Environment | TERM only, inherit parent | Set TERM=xterm-256color. Pass through parent env otherwise. |

---

## Architecture

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
    │ SIMD VT │   (Mutex)    │ rows to GPU │
    │ parse   │              │ Dispatch    │
    │ Grid    │              │ compute     │
    │ mutate  │              │ Present     │
    └─────────┘              └─────────────┘
```

### Threading Model

- **Main thread**: winit event loop, AppKit, keyboard/mouse input → PTY write
- **I/O thread**: PTY read (kqueue), SIMD VT parsing, grid state mutation, dirty flag setting
- **Render thread**: Metal rendering — lock grid (Mutex), upload dirty rows, dispatch compute, present

---

## Project Structure

```
etch/
├── Cargo.toml
├── build.rs                    # Metal shader compilation
├── src/
│   ├── main.rs                 # Entry point, winit bootstrap, thread spawning
│   ├── config.rs               # Compile-time constants (font, colors, padding, scrollback)
│   ├── terminal/
│   │   ├── mod.rs
│   │   ├── grid.rs             # Cell grid, dirty tracking bitset, cursor state
│   │   ├── cell.rs             # Cell struct (must match Metal CellData layout)
│   │   └── scrollback.rs       # Scrollback ring buffer
│   ├── parser/
│   │   ├── mod.rs              # Parser public API, 3-layer dispatch
│   │   ├── simd.rs             # NEON byte classifier + ASCII run scanner
│   │   ├── csi_fast.rs         # Optimistic CSI parser (SGR, cursor, erase)
│   │   ├── state_machine.rs    # Full VT state machine (Paul Williams model)
│   │   ├── table.rs            # Packed transition table (375 bytes)
│   │   ├── perform.rs          # Perform trait (parser → grid interface)
│   │   ├── utf8.rs             # UTF-8 codepoint assembler
│   │   └── charset.rs          # G0/G1 character set translation (DEC Special Graphics)
│   ├── renderer/
│   │   ├── mod.rs
│   │   ├── metal.rs            # Device, queue, pipeline, triple-buffer, dispatch
│   │   ├── atlas.rs            # Glyph texture atlas (grid packing, LRU)
│   │   ├── font.rs             # CoreText rasterization → R8 alpha bitmaps
│   │   └── shader.metal        # Compute shader (glyph render + box drawing + cursor)
│   ├── pty/
│   │   └── mod.rs              # forkpty + kqueue non-blocking I/O
│   └── input.rs                # Key event → VT byte sequence translation
└── benches/
    ├── parser_bench.rs
    └── render_bench.rs
```

---

## CellData Layout (16 bytes, must match Rust and Metal exactly)

```
Offset  Size  Field       Description
0       2     codepoint   Unicode BMP (u16). Debugging aid; shader uses atlas coords.
2       2     flags       Bitfield:
                            [0]    wide         — this cell starts a wide character
                            [1]    wide_cont    — continuation of wide char (skip rendering)
                            [2]    underline
                            [3]    strikethrough
                            [4]    inverse
                            [5]    cursor       — cursor is on this cell
                            [6]    selected     — selection highlight active
                            [7:8]  reserved
                            [9:15] reserved
4       1     fg_index    0-255 xterm palette. 0xFF = use fg_rgb field.
5       1     bg_index    0-255 xterm palette. 0xFF = use bg_rgb field.
6       1     atlas_x     Glyph column in atlas grid.
7       1     atlas_y     Glyph row in atlas grid.
8       4     fg_rgb      0x00RRGGBB (used when fg_index == 0xFF)
12      4     bg_rgb      0x00RRGGBB (used when bg_index == 0xFF)
```

---

## Phase 1: Metal Window + Compute Shader

**Goal**: winit fullscreen window with CAMetalLayer, compute shader fills drawable with a solid color.

**Files**: `Cargo.toml`, `build.rs`, `src/main.rs`, `src/config.rs`, `src/renderer/mod.rs`, `src/renderer/metal.rs`, `src/renderer/shader.metal`

### Cargo.toml Dependencies

```toml
[dependencies]
metal = "0.32"
objc = "0.2"
cocoa = "0.26"
winit = "0.30"
bytemuck = { version = "1", features = ["derive"] }
bitflags = "2"
bitvec = "1"
core-foundation = "0.10"
core-graphics = "0.24"
core-text = "20"
foreign-types = "0.5"
log = "0.4"
env_logger = "0.11"
```

### build.rs

- Compile `src/renderer/shader.metal` → `shader.air` → `shader.metallib`
- Command: `xcrun -sdk macosx metal -c shader.metal -o shader.air -ffast-math -std=metal3.0`
- Command: `xcrun -sdk macosx metallib shader.air -o shader.metallib`
- Embed in binary via `include_bytes!(concat!(env!("OUT_DIR"), "/shader.metallib"))`

### Metal Setup Sequence

1. `Device::system_default()`
2. Create `CAMetalLayer` via inline objc (from NSView obtained via winit's `HasWindowHandle`)
3. Set pixel format `BGRAUnorm`, `framebufferOnly = false`, `displaySyncEnabled = true`
4. Create compute pipeline from compiled metallib
5. Create command queue
6. Render loop: `next_drawable()` → encode compute → dispatch threads → present → commit

### Compute Shader (Phase 1)

Simple: one thread per pixel, writes a solid color to the output texture.

```metal
kernel void render(
    texture2d<half, access::write> output [[texture(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= output.get_width() || gid.y >= output.get_height()) return;
    output.write(half4(0.114, 0.122, 0.129, 1.0), gid);  // #1d1f21
}
```

### config.rs

```rust
pub const FONT_FAMILY: &str = "Menlo";
pub const FONT_SIZE: f64 = 13.0;
pub const PADDING: u32 = 8;
pub const SCROLLBACK_LINES: usize = 10_000;

// Tweaked palette (Alacritty-style)
pub const DEFAULT_FG: u32 = 0x00c5c8c6;
pub const DEFAULT_BG: u32 = 0x001d1f21;
pub const PALETTE: [u32; 256] = [ /* ... */ ];
```

---

## Phase 2: Font Rasterization + Glyph Atlas

**Goal**: CoreText renders ASCII glyphs into a 2048×2048 R8Unorm texture atlas.

**Files**: `src/renderer/font.rs`, `src/renderer/atlas.rs`

### Font Rasterization (font.rs)

- Load font via `CTFont::new_from_name("Menlo", 13.0)` at current DPI scale
- Measure cell size: `CTFont::bounding_rects_for_glyphs()` for advance width, `ascent + descent + leading` for height
- Rasterize each glyph:
  1. Get glyph index from codepoint via `CTFont::get_glyphs_for_characters()`
  2. Create `CGBitmapContext` with `kCGImageAlphaOnly` (8-bit grayscale)
  3. Draw glyph with `CTFont::draw_glyphs()` at baseline position
  4. Extract pixel buffer → R8 alpha bitmap

### Atlas (atlas.rs)

- 2048×2048 `MTLTexture` with `PixelFormat::R8Unorm`, `StorageMode::Shared`
- Grid layout: uniform cell slots, `(2048 / cell_w) × (2048 / cell_h)` slots
- At startup: pre-rasterize ASCII 0x20–0x7E (95 glyphs, regular weight only since bold = bright colors)
- HashMap<GlyphKey, AtlasPosition> for lookup (GlyphKey = codepoint + wide flag)
- LRU eviction for non-ASCII glyphs. ASCII permanently pinned (slot indices 0–94).
- Wide glyphs get a double-width slot (uses two adjacent columns in the grid)
- Upload via `texture.replace_region()` for individual slots
- On DPI change: clear entire atlas, re-rasterize from scratch

### DPI Handling

- Track current scale factor from winit
- On `ScaleFactorChanged`: re-create CTFont at new size × scale, clear atlas, re-rasterize ASCII, recalculate cell dimensions

---

## Phase 3: Cell Grid + Compute Shader Text Rendering

**Goal**: Render a static grid of colored text to screen.

**Files**: `src/terminal/cell.rs`, `src/terminal/grid.rs`, `src/renderer/shader.metal` (update)

### Grid (grid.rs)

- `Grid { cells: Vec<Cell>, cols: u16, rows: u16, dirty: BitVec }`
- `dirty`: one bit per row, managed by `bitvec`
- Cursor position: `(col, row)` stored in grid
- `mark_dirty(row)` / `is_dirty(row)` / `clear_dirty()`
- Resize: reallocate cells, send SIGWINCH to PTY

### Triple-Buffered Frame Pipeline

```
3 × MTLBuffer for CellData (StorageMode::Shared)
  Size: max_rows × max_cols × 16 bytes each
dispatch_semaphore(3) for frame pacing

Per frame:
  1. semaphore.wait()
  2. Lock grid mutex
  3. For each dirty row: memcpy row to cell_buffers[frame_idx % 3]
  4. Copy dirty bitset, clear it
  5. Unlock grid mutex
  6. If nothing was dirty → semaphore.signal(), skip to next frame
  7. next_drawable()
  8. Encode compute: bind cell_buffer, atlas_texture, palette_buffer, uniforms
  9. dispatch_threads(drawable_size, threadgroup_size(16, 16, 1))
  10. present_drawable + commit
  11. add_completed_handler { semaphore.signal() }
```

### Shader Uniforms

```metal
struct Uniforms {
    uint  cols;
    uint  rows;
    uint  cell_width;
    uint  cell_height;
    uint  atlas_cell_width;
    uint  atlas_cell_height;
    uint  padding;
    uint  cursor_row;
    uint  cursor_col;
};
```

### Updated Compute Shader

```metal
kernel void render(
    texture2d<half, access::write>  output     [[texture(0)]],
    texture2d<half, access::read>   atlas      [[texture(1)]],
    device const CellData*          cells      [[buffer(0)]],
    device const float4*            palette    [[buffer(1)]],
    constant Uniforms&              uni        [[buffer(2)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= output.get_width() || gid.y >= output.get_height()) return;

    // Account for padding
    int2 pos = int2(gid) - int2(uni.padding);
    if (pos.x < 0 || pos.y < 0) {
        output.write(bg_color, gid);
        return;
    }

    uint col = pos.x / uni.cell_width;
    uint row = pos.y / uni.cell_height;
    if (col >= uni.cols || row >= uni.rows) {
        output.write(bg_color, gid);
        return;
    }

    CellData cell = cells[row * uni.cols + col];

    // Skip wide_cont cells (already rendered by the wide cell)
    // Resolve fg/bg colors from palette or RGB
    // Read glyph alpha from atlas
    // Blend: bg_color * (1 - alpha) + fg_color * alpha
    // Handle inverse flag
    // Handle underline/strikethrough (single-pixel horizontal lines)
    // Handle cursor (block = invert entire cell)
    // Handle selection (invert colors)
    // Handle box drawing (procedural, see below)
}
```

### Box Drawing (Procedural)

For codepoints U+2500–U+257F, instead of sampling the atlas:
- Decode the codepoint into edge connectivity (top, bottom, left, right) and weight (light, heavy, double)
- Calculate pixel position within the cell
- Draw horizontal/vertical line segments and corners with appropriate thickness
- Light lines = 1px, heavy = 2px, double = two 1px lines with 1px gap

Lookup table: `box_drawing_edges[128]` mapping each char to a packed byte of edge flags.

---

## Phase 4: PTY + Basic I/O

**Goal**: Spawn a shell, read output, write input.

**Files**: `src/pty/mod.rs`

### PTY Setup

```rust
// forkpty() to create PTY pair + fork child
// Child: setsid(), set TERM=xterm-256color, exec($SHELL or /bin/zsh)
// Parent: get master fd, set non-blocking via fcntl(O_NONBLOCK)
```

### I/O Thread

```rust
// Dedicated thread, runs independently
loop {
    // kqueue wait on master_fd (EVFILT_READ)
    // read() into 64KB buffer
    // Feed buffer to VT parser
    // Parser calls Perform methods which mutate grid (under Mutex)
    // Set AtomicBool dirty flag to wake render thread
}
```

### Write Path

- Main thread receives keyboard events → translate to bytes → write() to master_fd
- Non-blocking write, buffer if EAGAIN (unlikely for typical keyboard input)

### Child Exit

- kqueue EVFILT_PROC or detect read() returning 0 / EIO
- Signal main thread to exit → winit event loop breaks → process exits

---

## Phase 5: VT Parser

**Goal**: Full xterm-256color VT emulation with SIMD acceleration.

**Files**: `src/parser/*.rs`

### Architecture: 3-Layer Pipeline

```
Input bytes ──► Layer 0: NEON SIMD Scanner
                │
                ├─ Printable ASCII run ──► performer.print_ascii_run()  [bulk memcpy to grid]
                │
                ├─ ESC [ detected ──► Layer 1: CSI Fast-Path
                │                     │
                │                     ├─ Recognized (SGR, cursor, erase) ──► performer.specific_method()
                │                     │
                │                     └─ Unrecognized ──► Layer 2: Scalar State Machine
                │
                └─ Control / high byte ──► Layer 2: Scalar State Machine
                                           │
                                           └─ Actions ──► performer.csi_dispatch() / osc_dispatch() / etc.
```

### Layer 0: NEON SIMD Scanner (simd.rs)

- Process 64 bytes per iteration (4×16 unrolled)
- Classify each byte: printable ASCII (0x20–0x7E) vs control/high
- When all 64 bytes are printable: call `performer.print_ascii_run(&buf[start..start+64])`
- When a special byte is found: flush the ASCII run, hand off to Layer 1 or 2

```rust
#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

unsafe fn classify_chunk_16(ptr: *const u8) -> u64 {
    let chunk = vld1q_u8(ptr);
    let is_control = vcltq_u8(chunk, vdupq_n_u8(0x20));
    let is_high_or_del = vcgeq_u8(chunk, vdupq_n_u8(0x7F));
    let attention = vorrq_u8(is_control, is_high_or_del);
    // Pack 16 bytes to bitmask
    vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(attention))))
}
```

### Layer 1: CSI Fast-Path (csi_fast.rs)

Handles the ~15 most common CSI sequences inline without entering the state machine:

| Final | Sequence | Action |
|---|---|---|
| `m` | SGR | Colors, bold (→ bright), italic, underline, reset |
| `A` | CUU | Cursor up |
| `B` | CUD | Cursor down |
| `C` | CUF | Cursor forward |
| `D` | CUB | Cursor backward |
| `H` | CUP | Cursor position |
| `G` | CHA | Cursor horizontal absolute |
| `d` | VPA | Cursor vertical absolute |
| `J` | ED | Erase in display |
| `K` | EL | Erase in line |
| `S` | SU | Scroll up |
| `T` | SD | Scroll down |
| `L` | IL | Insert lines |
| `M` | DL | Delete lines |
| `@` | ICH | Insert characters |
| `P` | DCH | Delete characters |
| `h`/`l` | SM/RM | Set/reset mode (with `?` prefix for DECSET/DECRST) |

Bail to Layer 2 on: colon sub-params, intermediate bytes, unrecognized finals, malformed sequences.

### Layer 2: Scalar State Machine (state_machine.rs)

- Paul Williams VT500-compatible model
- 15 states: Ground, Escape, EscapeIntermediate, CsiEntry, CsiParam, CsiIntermediate, CsiIgnore, DcsEntry, DcsParam, DcsIntermediate, DcsPassthrough, DcsIgnore, OscString, SosPmApcString
- 14 actions: Print, Execute, Hook, Put, Unhook, OscStart, OscPut, OscEnd, CsiDispatch, EscDispatch, Clear, Collect, Param, Ignore
- Packed transition table: 25 byte equivalence classes × 15 states = 375 entries
- Each entry: `u8` where high nibble = action, low nibble = next state

### Character Sets (charset.rs)

- Track active G0/G1 designations
- DEC Special Graphics (ESC ( 0): 96-entry lookup table mapping ASCII → Unicode box drawing
- `SO` (0x0E) / `SI` (0x0F) to switch between G0 and G1
- Translation applied in `print()` path before grid insertion

### UTF-8 Assembler (utf8.rs)

- Accumulate continuation bytes across parser calls
- Validate sequences, replace malformed with U+FFFD
- Feed complete codepoints to `performer.print()`

### Perform Trait (perform.rs)

```rust
pub trait Perform {
    // Hot path — bulk ASCII
    fn print_ascii_run(&mut self, bytes: &[u8]);
    // Single Unicode char (after charset translation)
    fn print(&mut self, c: char);
    // C0 controls
    fn execute(&mut self, byte: u8);  // CR, LF, BS, TAB, BEL, etc.
    // Cursor movement
    fn cursor_up(&mut self, n: u16);
    fn cursor_down(&mut self, n: u16);
    fn cursor_forward(&mut self, n: u16);
    fn cursor_backward(&mut self, n: u16);
    fn cursor_position(&mut self, row: u16, col: u16);
    fn cursor_horizontal_absolute(&mut self, col: u16);
    fn cursor_vertical_absolute(&mut self, row: u16);
    // Erase
    fn erase_in_display(&mut self, mode: u16);
    fn erase_in_line(&mut self, mode: u16);
    // Scroll
    fn scroll_up(&mut self, n: u16);
    fn scroll_down(&mut self, n: u16);
    // Insert/delete
    fn insert_lines(&mut self, n: u16);
    fn delete_lines(&mut self, n: u16);
    fn insert_chars(&mut self, n: u16);
    fn delete_chars(&mut self, n: u16);
    // SGR
    fn sgr(&mut self, params: &[u16]);
    // Modes
    fn set_mode(&mut self, mode: u16, private: bool);
    fn reset_mode(&mut self, mode: u16, private: bool);
    // OSC
    fn osc_dispatch(&mut self, params: &[&[u8]]);
    // Fallback
    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], byte: u8);
    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8);
}
```

### DECSET/DECRST Modes Supported

| Mode | Name | Description |
|---|---|---|
| 1 | DECCKM | Cursor keys send ESC O vs ESC [ |
| 7 | DECAWM | Auto-wrap at right margin |
| 25 | DECTCEM | Cursor visible/hidden |
| 47 | Alt screen (old) | Switch to/from alternate screen buffer |
| 1000 | Mouse button tracking | Report mouse clicks |
| 1002 | Mouse cell-motion | Report mouse motion while button held |
| 1003 | Mouse all-motion | Report all mouse motion |
| 1004 | Focus events | Report focus in/out (stub: accept, don't report) |
| 1006 | SGR mouse encoding | Mouse reports as CSI < ... M/m |
| 1049 | Alt screen + cursor | Switch alt screen, save/restore cursor |
| 2004 | Bracketed paste | Wrap pasted text in ESC [200~/201~ |
| 2026 | Synchronized output | Defer rendering between BSU/ESU markers |

### Synchronized Output (Mode 2026)

- On BSU (`ESC P =1s ESC \` or `CSI ? 2026 h`): set flag, suppress render-thread wake
- On ESU (`ESC P =2s ESC \` or `CSI ? 2026 l`): clear flag, mark all dirty, wake render thread
- Timeout: if BSU without ESU for >1 second, force render (prevents stuck invisible state)

---

## Phase 6: Input Handling

**Goal**: Keyboard events → correct VT byte sequences → PTY write.

**Files**: `src/input.rs`

### Key Translation

- winit `KeyEvent` → match on `key.logical_key` and `key.physical_key`
- Printable characters: UTF-8 encode and write
- Option key: always send ESC prefix (both Left and Right Option)
- Cmd+C: copy selection to NSPasteboard (not sent to PTY)
- Cmd+V: read NSPasteboard, wrap in bracketed paste if mode 2004 active, write to PTY

### Special Keys

| Key | Normal | Application (DECCKM) |
|---|---|---|
| Up | `ESC [ A` | `ESC O A` |
| Down | `ESC [ B` | `ESC O B` |
| Right | `ESC [ C` | `ESC O C` |
| Left | `ESC [ D` | `ESC O D` |
| Home | `ESC [ H` | `ESC O H` |
| End | `ESC [ F` | `ESC O F` |
| F1-F4 | `ESC O P/Q/R/S` | same |
| F5-F12 | `ESC [ 15~` ... `ESC [ 24~` | same |
| Backspace | `0x7F` | `0x7F` |
| Delete | `ESC [ 3 ~` | same |
| Tab | `0x09` | `0x09` |
| Enter | `0x0D` | `0x0D` |
| Escape | `0x1B` | `0x1B` |

### Modifier Encoding

- Ctrl+letter: `byte & 0x1F` (e.g., Ctrl+C = 0x03)
- Alt/Option+key: `ESC` prefix + key byte
- Shift+special: modify parameter (e.g., Shift+Up = `ESC [ 1 ; 2 A`)

### Mouse Events

When mouse tracking modes are active:
- SGR encoding (mode 1006): `ESC [ < button ; col ; row M` (press) / `m` (release)
- Button values: 0=left, 1=middle, 2=right, 32+=motion, 64+=scroll

---

## Phase 7: Scrollback + Scroll Optimization

**Goal**: Efficient scrolling and scrollback buffer.

**Files**: `src/terminal/scrollback.rs`, updates to `src/terminal/grid.rs`

### Ring Buffer

```rust
pub struct Scrollback {
    buf: Vec<Vec<Cell>>,   // Ring buffer of rows
    capacity: usize,        // Max rows (from config::SCROLLBACK_LINES)
    head: usize,            // Write position
    len: usize,             // Current number of stored rows
}
```

- Push: copy top grid row to `buf[head]`, advance head
- Each row stored at the terminal width at time of eviction (fixed-width, per decision #15)
- Access by offset from current viewport

### Scroll Operations

- **Scroll up (new content at bottom)**: memmove cells up by N rows, push evicted rows to scrollback, clear new rows at bottom, mark only new rows dirty
- **Scroll down (new content at top)**: memmove cells down, clear new rows at top, mark only new rows dirty
- **Viewport scroll (user scrolling through history)**: render from scrollback instead of active grid. Any new PTY output snaps back to bottom.

### Efficiency

- `memmove` the CellData buffer instead of shifting individual cells
- Only mark actually-changed rows dirty (new content rows after scroll)
- Scrollback rows are not uploaded to GPU — only the visible viewport's worth of cells

---

## Phase 8: Polish

### Selection

- State: `Option<(SelectionStart, SelectionEnd)>` where each is `(col, row)`
- Mouse down: set start, clear end
- Mouse drag: update end, mark affected rows dirty
- Mouse up: finalize selection
- Double-click: select word (scan for word boundaries)
- Triple-click: select line
- Cmd+C: extract selected text from grid cells, write to NSPasteboard
- Render: set `selected` flag on cells within selection range, shader inverts colors

### OSC 52 (Clipboard)

- Parse: `OSC 52 ; c ; <base64-data> ST`
- Decode base64, write to NSPasteboard
- Query (empty data): respond with current clipboard contents encoded as base64

### Window Title

- OSC 0 / OSC 2: set window title via winit `window.set_title()`
- Parse title from OSC data bytes (UTF-8)

### Cursor Blink

- Timer: toggle cursor visibility every 500ms
- When cursor cell changes (typing), reset blink timer to visible state
- When toggling: mark only the cursor row dirty

### Resize

1. winit `Resized` event → recalculate grid dimensions from new window size
2. Reallocate CellData buffers (all 3 triple-buffer slots)
3. Copy existing grid content (truncate or pad)
4. Send `SIGWINCH` to child process via `ioctl(TIOCSWINSZ)`
5. Full dirty (all rows)

### Bell

- BEL (0x07): visual bell — invert all cells for one frame, then restore
- Set all rows dirty with inverse flag, schedule restore for next frame

---

## Color Palette

### Default ANSI (Tweaked/Alacritty-style)

```
 0 Black       #1d1f21     8 Bright Black   #969896
 1 Red         #cc6666     9 Bright Red     #cc6666
 2 Green       #b5bd68    10 Bright Green   #b5bd68
 3 Yellow      #f0c674    11 Bright Yellow  #f0c674
 4 Blue        #81a2be    12 Bright Blue    #81a2be
 5 Magenta     #b294bb    13 Bright Magenta #b294bb
 6 Cyan        #8abeb7    14 Bright Cyan    #8abeb7
 7 White       #c5c8c6    15 Bright White   #ffffff
```

Indices 16–231: 6×6×6 color cube (standard formula).
Indices 232–255: grayscale ramp (standard formula).

Default foreground: index 7 (#c5c8c6)
Default background: index 0 (#1d1f21)

Bold text: palette colors 0–7 brightened to 8–15. No font weight change.

---

## Build & Run

```bash
cargo build --release
./target/release/etch
```

Environment: inherits parent env, sets `TERM=xterm-256color`.
Shell: `$SHELL` or fallback to `/bin/zsh`.
Launches into macOS native fullscreen.
