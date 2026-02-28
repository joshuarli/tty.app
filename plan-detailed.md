# tty.app: Detailed Implementation Plan

## Overview

Minimalist terminal emulator for macOS. Compute-shader-only Metal renderer (no vertex pipeline), SIMD VT parser (NEON), Apple Silicon only. Single-threaded event loop with native AppKit windowing (no winit).

## Decision Log

| # | Decision | Choice | Rationale |
|---|---|---|---|
| 1 | Atlas coords | `u8×u8`, 2048 atlas | 20K+ slots sufficient for v1. Revisit if needed. |
| 2 | Codepoint field | Keep `u16` in CellData | GPU doesn't use it but useful for debugging. BMP-only fine for v1. |
| 3 | Metal layer setup | Native `objc2` AppKit | `NativeWindow` in `window.rs` — NSApplication + NSWindow + TtyView (NSView subclass). No winit, no cocoa crate. |
| 4 | Grid sync | Single-threaded, no Mutex | Everything runs in one thread — no lock needed. |
| 5 | DECSET scope | Full list + mode 2026 (synchronized output) | Enough for nvim/tmux/htop. Sync output prevents tearing. |
| 6 | Config | Compile-time `config.rs` constants | No runtime config parsing. Change and recompile. |
| 7 | Wide chars | Single-width atlas slots, overflow | Wide glyphs rasterized at 2×cell_width but stored starting at a single grid slot, overflowing into adjacent pixel space. Shader checks wide flag. |
| 8 | Bold | Bright colors only | No bold font variant in v1. Simplifies atlas (regular weight only). |
| 9 | Subpixel AA | Weighted grayscale R8 | RGBA context with `FONT_SMOOTH_WEIGHT` blend between min-channel (thinnest) and average (medium). Fine on Retina. |
| 10 | Resize reflow | No reflow | Resize changes grid dims + SIGWINCH. Wrapped lines stay wrapped. |
| 11 | Clipboard | Cmd+C/V + OSC 52 | NSPasteboard for GUI copy/paste. OSC 52 for programmatic (tmux/nvim over SSH). |
| 12 | Cursor shape | Block only | Ignore DECSCUSR. Single cursor style with blink. |
| 13 | Font | Hack 16pt | Configured in `config.rs`. CoreText rasterization with `CTFontCreateForString` fallback for missing glyphs. |
| 14 | TERM | `xterm-256color` | Universal terminfo. No custom terminfo entry. |
| 15 | Scrollback | Ring buffer, fixed width | `Vec<Vec<Cell>>`. Old rows stored at the terminal width at time of eviction. |
| 16 | Box drawing | Procedural in shader | Pixel-perfect alignment for U+2500–U+257F. 128-entry lookup table for edge connectivity + weight. |
| 17 | Color scheme | Dracula-inspired | ANSI colors: bg #000000, fg #ffffff. Bright palette with distinctive colors per index. |
| 18 | Selection | Simple linear model | Click-drag only (no double/triple-click). Rendered as inverted fg/bg via SELECTED cell flag. |
| 19 | Window padding | 16px default | Constant in config.rs. Shader offsets grid origin. Padding top respects notch safe area. |
| 20 | Transparency | Opaque only | layer.setOpaque(true). No alpha compositing. |
| 21 | Shell exit | Close window immediately | Shell exits → `alive = false` → loop breaks → process exits. |
| 22 | Framerate | kqueue idle + 8ms poll | displaySyncEnabled for vsync. kqueue on PTY fd for immediate wake on shell output. 8ms timeout for AppKit event polling. Zero GPU work when idle. |
| 23 | DEC graphics | G0/G1 character set support | 31-entry lookup table (0x60–0x7E). Required for tmux/ncurses. |
| 24 | DPI change | Handled via resize event | NativeWindow detects scale factor changes, triggers resize path. |
| 25 | Option key | Always Meta | Both Option keys send ESC prefix. Simplest. |
| 26 | Cmd shortcuts | Cmd+Q/C/V | Cmd+Q quits, Cmd+C copies selection, Cmd+V pastes. All other Cmd combos ignored (not sent to PTY). |
| 27 | Window size | Native fullscreen | Launch into macOS fullscreen with suppressed animation. Notch-aware safe area inset. |
| 28 | Intel support | Apple Silicon only | NEON-only SIMD, scalar fallback compiled but untested. StorageMode::Shared everywhere. |
| 29 | Environment | TERM only, inherit parent | Set TERM=xterm-256color. Pass through parent env otherwise. |

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                    Single Thread (main loop)                  │
│                                                              │
│  1. process_pty_output()  — read PTY, feed parser, mutate grid│
│  2. poll_events()         — drain AppKit events               │
│  3. handle_event()        — keys/mouse → PTY write / state    │
│  4. render()              — drain PTY again, upload grid, GPU  │
│  5. if idle → kqueue wait — block on PTY fd, 8ms timeout      │
│                                                              │
│  NativeWindow (objc2 AppKit)    MetalRenderer (CAMetalLayer) │
│  Parser (SIMD + state machine)  Grid + Scrollback            │
└──────────────────────────────────────────────────────────────┘
```

### Threading Model

Single-threaded. A manual `loop` in `main()` drives everything:

- **Event polling**: `NativeWindow::poll_events()` drains AppKit event queue (keys, mouse, resize, focus)
- **PTY I/O**: Non-blocking `libc::read()`/`libc::write()` on the master fd in the main loop
- **Parsing**: SIMD VT parser runs synchronously, mutates grid directly (no locking)
- **Rendering**: Metal compute dispatch via `render_frame()`, GPU work is the only async component
- **Idle**: kqueue on PTY fd with 8ms timeout — wakes immediately on shell output, falls back to polling AppKit

---

## Project Structure

```
tty.app/
├── Cargo.toml
├── build.rs                    # Records rustc commit hash for --version
├── src/
│   ├── main.rs                 # Entry point, App struct, TermPerformer, event loop
│   ├── lib.rs                  # Re-exports config, parser, terminal as public modules
│   ├── config.rs               # Compile-time constants (font, colors, padding, scrollback)
│   ├── window.rs               # NativeWindow (objc2 AppKit), TtyView, Event types, key translation
│   ├── input.rs                # Key/mouse events → VT byte sequences
│   ├── terminal/
│   │   ├── mod.rs
│   │   ├── grid.rs             # Cell grid, dirty tracking bitset, cursor state, alt-screen
│   │   ├── cell.rs             # Cell struct (must match Metal CellData layout)
│   │   └── scrollback.rs       # Scrollback ring buffer
│   ├── parser/
│   │   ├── mod.rs              # Parser public API, 3-layer dispatch
│   │   ├── simd.rs             # NEON byte classifier + ASCII run scanner
│   │   ├── csi_fast.rs         # Optimistic CSI parser (~25 sequences, handles colon sub-params)
│   │   ├── state_machine.rs    # Full VT state machine (Paul Williams model)
│   │   ├── table.rs            # Packed transition table (14 states × 24 classes = 336 entries)
│   │   ├── perform.rs          # Perform trait (parser → grid interface)
│   │   ├── utf8.rs             # UTF-8 codepoint assembler
│   │   └── charset.rs          # G0/G1 character set translation (DEC Special Graphics)
│   ├── renderer/
│   │   ├── mod.rs
│   │   ├── metal.rs            # Device, queue, pipeline, double-buffer, dispatch
│   │   ├── atlas.rs            # Glyph texture atlas (grid packing, LRU, font fallback)
│   │   ├── font.rs             # CoreText rasterization → weighted R8 alpha bitmaps
│   │   └── shader.metal        # Compute shader (glyph render + box drawing + cursor)
│   └── pty/
│       └── mod.rs              # libc::forkpty, non-blocking read/write, TIOCSWINSZ resize
└── tests/
    └── parser_tests.rs         # Split-boundary and UTF-8 reassembly tests
```

---

## CellData Layout (16 bytes, must match Rust and Metal exactly)

```
Offset  Size  Field       Description
0       2     codepoint   Unicode BMP (u16). Debugging aid; shader uses atlas coords.
2       2     flags       Bitfield:
                            [0]    WIDE         — this cell starts a wide character
                            [1]    WIDE_CONT    — continuation of wide char
                            [2]    UNDERLINE
                            [3]    STRIKE
                            [4]    INVERSE
                            [5]    CURSOR       — cursor is on this cell
                            [6]    SELECTED     — selection highlight active
                            [7]    BOLD         — triggers bright color mapping in shader
                            [8]    ITALIC
                            [9]    DIM
                            [10]   HIDDEN       — fg = bg in shader
                            [11:15] reserved
4       1     fg_index    0-255 xterm palette. 0xFF = use fg_rgb field.
5       1     bg_index    0-255 xterm palette. 0xFF = use bg_rgb field.
6       1     atlas_x     Glyph column in atlas grid.
7       1     atlas_y     Glyph row in atlas grid.
8       4     fg_rgb      0x00RRGGBB (used when fg_index == 0xFF)
12      4     bg_rgb      0x00RRGGBB (used when bg_index == 0xFF)
```

---

## Phase 1: Metal Window + Compute Shader

**Goal**: Native fullscreen window with CAMetalLayer, compute shader fills drawable with a solid color.

**Files**: `Cargo.toml`, `build.rs`, `src/main.rs`, `src/window.rs`, `src/config.rs`, `src/renderer/mod.rs`, `src/renderer/metal.rs`, `src/renderer/shader.metal`

### Cargo.toml Dependencies

```toml
[dependencies]
metal = "0.33"
objc2 = "0.6"
objc2-foundation = { version = "0.3", features = [...] }
objc2-app-kit = { version = "0.3", features = [...] }
bytemuck = { version = "1", features = ["derive"] }
bitflags = "2"
bitvec = "1"
core-foundation = "0.10"
core-graphics = "0.25"
core-graphics-types = "0.2"
core-text = "21"
block = "0.1"
libc = "0.2"
```

### build.rs

Records the rustc commit hash into `TTY_RUSTC_COMMIT` env var for `--version` output. Shader is compiled at runtime via `include_str!` + `new_library_with_source()`, not pre-compiled.

### Window Setup (window.rs)

1. `NSApplication::sharedApplication()` → `setActivationPolicy(Regular)` → `finishLaunching()`
2. Create `NSWindow` with `FullSizeContentView` style, transparent titlebar, black background
3. `TtyView` (NSView subclass via `define_class!`) set as content view
4. Enable `FullScreenPrimary` collection behavior
5. Enter native fullscreen with suppressed animation via `NSAnimationContext`
6. Detect notch safe area via `safeAreaInsets`

### Metal Setup Sequence (metal.rs)

1. `Device::system_default()`
2. Create `MetalLayer`, set pixel format `BGRA8Unorm`, `framebufferOnly = false`, `displaySyncEnabled = true`, `presentsWithTransaction = true`
3. Attach layer to TtyView via `setLayer:`
4. Compile shader from source at runtime: `include_str!("shader.metal")` → `new_library_with_source()` with fast math
5. Create compute pipeline from `render` function
6. Create command queue
7. Render loop: check dirty → spin-wait buffer → memcpy grid → `next_drawable()` → encode compute → dispatch → present → commit

### config.rs

```rust
pub const FONT_FAMILY: &str = "Hack";
pub const FONT_SIZE: f64 = 16.0;
pub const FONT_SMOOTH_WEIGHT: f32 = 0.3;
pub const PADDING: u32 = 16;
pub const SCROLLBACK_LINES: usize = 10_000;
pub const DEFAULT_FG: u32 = 0x00ffffff;
pub const DEFAULT_BG: u32 = 0x00000000;
pub const CURSOR_BLINK_MS: u64 = 1000;
pub const PALETTE: [u32; 256] = { /* const block: Dracula-inspired ANSI 0-15, 6×6×6 cube, 24-step grayscale */ };
```

---

## Phase 2: Font Rasterization + Glyph Atlas

**Goal**: CoreText renders ASCII glyphs into a 2048×2048 R8Unorm texture atlas.

**Files**: `src/renderer/font.rs`, `src/renderer/atlas.rs`

### Font Rasterization (font.rs)

- Load font via `ct_font::new_from_name("Hack", 16.0 * scale)` at current DPI scale
- Measure cell size: `get_advances_for_glyphs()` on 'M' for advance width, `ascent + descent + leading` for height
- Font fallback: `CTFontCreateForString` (FFI) finds system fonts for glyphs missing from primary font
- Rasterize each glyph:
  1. Get glyph index from codepoint via `CTFont::get_glyphs_for_characters()`
  2. Create `CGBitmapContext` with `kCGImageAlphaNoneSkipLast` (RGBA — required by CoreText)
  3. Draw glyph in white on black with `CTFont::draw_glyphs()` at baseline position
  4. Extract weighted single-channel alpha: blend between min(R,G,B) and avg(R,G,B) controlled by `FONT_SMOOTH_WEIGHT`

### Atlas (atlas.rs)

- 2048×2048 `MTLTexture` with `PixelFormat::R8Unorm`, `StorageMode::Shared`
- Grid layout: uniform cell-width slots, `(2048 / cell_w) × (2048 / cell_h)` slots
- At startup: pre-rasterize ASCII 0x20–0x7E (95 glyphs, regular weight only since bold = bright colors). These are pinned and never evicted.
- `HashMap<GlyphKey, AtlasPos>` for lookup (GlyphKey = codepoint + wide flag, AtlasPos = u8 × u8 grid coords)
- LRU eviction for non-ASCII glyphs: `frame` counter tracks last-access per slot, `evict_lru()` finds minimum
- Wide glyphs rasterized at 2×cell_width but stored starting at a single grid slot, overflowing into adjacent pixel space
- Upload via `texture.replace_region()` for individual slots

---

## Phase 3: Cell Grid + Compute Shader Text Rendering

**Goal**: Render a static grid of colored text to screen.

**Files**: `src/terminal/cell.rs`, `src/terminal/grid.rs`, `src/renderer/shader.metal` (update)

### Grid (grid.rs)

- `Grid { cells: Vec<Cell>, cols: u16, rows: u16, dirty: BitVec }`
- `dirty`: one bit per row, managed by `bitvec`
- Cursor position: `(cursor_col, cursor_row)` stored in grid
- `mark_dirty(row)` / `mark_all_dirty()` / `clear_dirty()`
- `cursor_pending_wrap`: DECAWM deferred wrap flag
- `mode: TermMode` bitflags for all DECSET modes
- `saved_cursor: SavedCursor` for DECSC/DECRC
- `sync_start: Option<Instant>` for mode 2026 timeout
- Alt-screen: `alt_cells: Vec<Cell>` + `main_cursor: SavedCursor`
- Resize: reallocate cells, copy preserved content, rebuild tab stops, send SIGWINCH to PTY

### Double-Buffered Frame Pipeline

```
2 × MTLBuffer for CellData (StorageMode::Shared)
  Size: rows × cols × 16 bytes each
AtomicBool per buffer for GPU completion signaling

Per frame:
  1. Check dirty.any() — skip frame if nothing changed (and !needs_render)
  2. Spin-wait for target buffer to be free (AtomicBool, rarely spins)
  3. ptr::copy_nonoverlapping(grid.cells → cell_buffer) — bulk memcpy entire grid
  4. Clear dirty bits
  5. next_drawable() — if None, set needs_render = true, return
  6. Update uniforms (cols, rows, cell size, padding, cursor pos, etc.)
  7. Encode compute: bind cell_buffer, atlas_texture, palette_buffer, uniforms
  8. dispatch_threads(framebuffer_size, threadgroup_size(16, 16, 1))
  9. Mark buffer in-flight (AtomicBool = false)
  10. add_completed_handler { AtomicBool = true }
  11. commit() + wait_until_scheduled() + present()
  12. Swap to other buffer for next frame
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
    uint  padding_top;       // max(padding, notch safe area)
    uint  cursor_row;
    uint  cursor_col;
    uint  cursor_visible;
    uint  frame_bg;          // 0x00RRGGBB default background
};
```

### Compute Shader Pipeline

Per-pixel compute kernel processes the entire framebuffer:

1. Map pixel → grid cell via integer division
2. Padding region → `frame_bg` default background
3. Wide continuation cells → look at owner cell to the left, sample right half of owner's glyph
4. Bold → remap palette index 0-7 to 8-15
5. Resolve fg/bg colors (palette lookup or RGB unpack)
6. Hidden → fg = bg
7. Inverse → swap fg/bg
8. Selected → swap fg/bg
9. Box drawing (U+2500..U+257F) → procedural from `BOX_EDGES[128]` lookup table
10. Normal glyphs → alpha blend from atlas texture via `atlas.read()`
11. Underline → 1px line at `cell_height - 2`
12. Strikethrough → 1px line at `cell_height / 2`
13. Cursor → full color inversion (`1.0 - color` per channel)

### Box Drawing (Procedural)

For codepoints U+2500–U+257F, instead of sampling the atlas:
- 128-entry `BOX_EDGES` lookup table, each byte encodes edge connectivity:
  - Bits [0-3]: right, left, down, up (light)
  - Bits [4-7]: right-heavy, left-heavy, down-heavy, up-heavy
- `draw_box_line()` tests pixel position against horizontal/vertical line segments
- Light lines = `max(1, cell_width/8)` px, heavy = `max(2, cell_width/4)` px

---

## Phase 4: PTY + Basic I/O

**Goal**: Spawn a shell, read output, write input.

**Files**: `src/pty/mod.rs`

### PTY Setup

```rust
// libc::forkpty() to create PTY pair + fork child
// Child: cd $HOME, set TERM=xterm-256color, exec($SHELL or /bin/zsh) as login shell (prefix with -)
// Parent: set master fd non-blocking via fcntl(O_NONBLOCK)
```

### Read Path (in main loop)

```rust
// process_pty_output() called from main loop (twice: once at top, once in render())
// Non-blocking read() into 64KB buffer
// Returns WouldBlock when empty → break
// Returns Ok(0) on EOF → shell exited → alive = false
// Feed buffer to VT parser which mutates grid directly (single-threaded, no lock)
```

### Write Path

- Main thread receives keyboard/mouse events → translate to bytes via `input.rs` → `pty.write()` to master_fd
- Non-blocking write (unlikely to block for typical keyboard input)

### Idle Wait (in main loop)

- kqueue registered with `EVFILT_READ` on PTY master fd
- When main loop is idle: `kevent()` with 8ms timeout
- Wakes immediately on shell output, falls back to 8ms polling for AppKit events

### Child Exit

- Detect `read()` returning `Ok(0)` (EOF) or `Err` (not WouldBlock) → set `alive = false`
- Main loop checks `alive` flag → breaks → process exits
- `Pty::drop()` sends SIGHUP to child

---

## Phase 5: VT Parser

**Goal**: Full xterm-256color VT emulation with SIMD acceleration.

**Files**: `src/parser/*.rs`

### Architecture: 3-Layer Pipeline

```
Input bytes ──► Layer 1: NEON SIMD Scanner
                │
                ├─ Printable ASCII run ──► performer.print_ascii_run()
                │
                ├─ ESC [ detected ──► Layer 2: CSI Fast-Path
                │                     │
                │                     ├─ Recognized (~25 sequences) ──► performer.specific_method()
                │                     │
                │                     └─ Bail (intermediate bytes, incomplete) ──► Layer 3
                │
                ├─ UTF-8 lead byte ──► Utf8Assembler (buffers across parse() calls)
                │
                └─ Control / ESC ──► Layer 3: Scalar State Machine
                                     │
                                     └─ Actions ──► performer.csi_dispatch() / osc_dispatch() / etc.
```

### Layer 1: NEON SIMD Scanner (simd.rs)

- Process 64 bytes per iteration (4×16 unrolled)
- Classify each byte: printable ASCII (0x20–0x7E) vs everything else
- When all 64 bytes are printable: call `performer.print_ascii_run()`
- When a special byte is found: return position, flush the ASCII run
- Remaining 16-byte chunks processed individually, then scalar tail
- `find_first_zero()` via `vshrn_n_u16` narrowing + nibble scan
- Scalar-only fallback for non-aarch64 (compiled but untested)

### Layer 2: CSI Fast-Path (csi_fast.rs)

Handles ~25 CSI sequences inline without entering the state machine:

| Final | Sequence | Action |
|---|---|---|
| `m` | SGR | Colors, bold, dim, italic, underline, inverse, hidden, strike, reset |
| `A` | CUU | Cursor up |
| `B`/`e` | CUD/VPR | Cursor down |
| `C`/`a` | CUF/HPR | Cursor forward |
| `D` | CUB | Cursor backward |
| `E` | CNL | Cursor next line |
| `F` | CPL | Cursor previous line |
| `H`/`f` | CUP/HVP | Cursor position |
| `G`/`` ` `` | CHA/HPA | Cursor horizontal absolute |
| `d` | VPA | Cursor vertical absolute |
| `J` | ED | Erase in display (0/1/2/3) |
| `K` | EL | Erase in line |
| `S` | SU | Scroll up |
| `T` | SD | Scroll down |
| `L` | IL | Insert lines |
| `M` | DL | Delete lines |
| `@` | ICH | Insert characters |
| `P` | DCH | Delete characters |
| `X` | ECH | Erase characters |
| `h`/`l` | SM/RM | Set/reset mode (with `?` prefix for DECSET/DECRST) |
| `r` | DECSTBM | Set scroll region |
| `g` | TBC | Tab clear |
| `n` | DSR | Device status report |
| `b` | REP | Repeat last character |
| `s`/`u` | SCOSC/SCORC | Save/restore cursor (ANSI.SYS-style) |

Colon sub-parameters: handled inline for SGR (`m` with colons) — dispatched to `performer.sgr_colon()` with raw bytes. Supports `4:N` underline styles, `38:2::R:G:B` / `48:2::R:G:B` direct color.

Bail to Layer 3 on: intermediate bytes (`0x20..0x2F`), incomplete sequences (buffer ends mid-sequence), unrecognized final bytes.

### Layer 3: Scalar State Machine (state_machine.rs)

- Paul Williams VT500-compatible model
- 14 states: Ground, Escape, EscapeIntermediate, CsiEntry, CsiParam, CsiIntermediate, CsiIgnore, DcsEntry, DcsParam, DcsIntermediate, DcsPassthrough, DcsIgnore, OscString, SosPmApcString
- 14 actions: Print, Execute, Hook, Put, Unhook, OscStart, OscPut, OscEnd, CsiDispatch, EscDispatch, Clear, Collect, Param, Ignore (+ None)
- Packed transition table: 24 byte equivalence classes × 14 states = 336 entries
- Each entry: `u8` where high nibble = action, low nibble = next state
- 7-bit C1 equivalents handled: ESC P → DCS, ESC [ → CSI, ESC ] → OSC, ESC X/^/_ → SOS/PM/APC
- BEL (0x07) terminates OSC strings (xterm extension)

### Character Sets (charset.rs)

- Track active G0/G1 designations via `charset_g0`, `charset_g1`, `active_charset`
- DEC Special Graphics (ESC ( 0): 31-entry lookup table mapping 0x60–0x7E → Unicode box drawing / special glyphs
- `SO` (0x0E) / `SI` (0x0F) to switch between G0 and G1
- Translation applied in `print_ascii_run()` and `print()` paths before grid insertion

### UTF-8 Assembler (utf8.rs)

- Buffers incomplete multi-byte sequences (2-4 bytes) across `parse()` calls
- `try_complete()`: called at start of each `parse()` to finish buffered sequence with new data
- `decode()`: attempts to decode from current position, buffers if incomplete
- Validates continuation bytes, rejects overlong encodings and surrogates
- Replaces malformed sequences with U+FFFD

### Perform Trait (perform.rs)

```rust
pub trait Perform {
    fn print_ascii_run(&mut self, bytes: &[u8]);
    fn print(&mut self, c: char);
    fn execute(&mut self, byte: u8);
    fn cursor_up(&mut self, n: u16);
    fn cursor_down(&mut self, n: u16);
    fn cursor_forward(&mut self, n: u16);
    fn cursor_backward(&mut self, n: u16);
    fn cursor_position(&mut self, row: u16, col: u16);
    fn cursor_horizontal_absolute(&mut self, col: u16);
    fn cursor_vertical_absolute(&mut self, row: u16);
    fn erase_in_display(&mut self, mode: u16);
    fn erase_in_line(&mut self, mode: u16);
    fn scroll_up(&mut self, n: u16);
    fn scroll_down(&mut self, n: u16);
    fn insert_lines(&mut self, n: u16);
    fn delete_lines(&mut self, n: u16);
    fn insert_chars(&mut self, n: u16);
    fn delete_chars(&mut self, n: u16);
    fn erase_chars(&mut self, n: u16);
    fn sgr(&mut self, params: &[u16]);
    fn sgr_colon(&mut self, raw: &[u8]);
    fn set_mode(&mut self, params: &[u16], private: bool);
    fn reset_mode(&mut self, params: &[u16], private: bool);
    fn set_scroll_region(&mut self, top: u16, bottom: u16);
    fn tab_clear(&mut self, mode: u16);
    fn set_tab_stop(&mut self);
    fn osc_dispatch(&mut self, params: &[&[u8]]);
    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8);
    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], ignore: bool, byte: u8);
    fn save_cursor(&mut self);
    fn restore_cursor(&mut self);
    fn device_status_report(&mut self, mode: u16);
    fn set_cursor_style(&mut self, _style: u16) {}  // no-op, block only
    fn repeat_char(&mut self, n: u16);
}
```

### DECSET/DECRST Modes Supported

| Mode | Name | Description |
|---|---|---|
| 1 | DECCKM | Cursor keys send ESC O vs ESC [ |
| 6 | DECOM | Origin mode — cursor relative to scroll region |
| 7 | DECAWM | Auto-wrap at right margin |
| 25 | DECTCEM | Cursor visible/hidden |
| 47/1047 | Alt screen | Switch to/from alternate screen buffer |
| 1000 | Mouse button tracking | Report mouse clicks |
| 1002 | Mouse cell-motion | Report mouse motion while button held |
| 1003 | Mouse all-motion | Report all mouse motion |
| 1004 | Focus events | Report focus in (ESC[I) / out (ESC[O) |
| 1006 | SGR mouse encoding | Mouse reports as CSI < ... M/m |
| 1049 | Alt screen + cursor | Save cursor, switch alt screen; restore on exit |
| 2004 | Bracketed paste | Wrap pasted text in ESC[200~ / ESC[201~ |
| 2026 | Synchronized output | Defer rendering during application updates |

### Synchronized Output (Mode 2026)

- On `CSI ? 2026 h`: set `SYNC_OUTPUT` flag, record `sync_start = Instant::now()`
- On `CSI ? 2026 l`: clear flag, clear `sync_start`
- `render()` returns early while `SYNC_OUTPUT` is set — dirty bits accumulate
- Timeout: if sync active for >100ms, force render and clear the flag (prevents stuck display)

---

## Phase 6: Input Handling

**Goal**: Keyboard events → correct VT byte sequences → PTY write.

**Files**: `src/input.rs`, `src/window.rs`

### Window Event System (window.rs)

- `NativeWindow::poll_events()` returns `Vec<Event>` with typed event variants
- Key events: `Event::KeyDown { key: Key, modifiers: Modifiers }`
- `Key::Character(String)` for printable keys, `Key::Named(NamedKey)` for special keys
- `Modifiers` is a u8 bitfield: SHIFT=1, CONTROL=2, ALT=4, SUPER=8
- Uses `charactersIgnoringModifiers` when Ctrl/Alt/Cmd held (gets base letter)
- macOS virtual key codes translated to `NamedKey` variants (arrows, F1-F12, etc.)

### Key Translation (input.rs)

- `key_to_bytes(key, modifiers, term_mode) -> Option<Vec<u8>>`
- Cmd modifier → return None (handled by App directly for Cmd+Q/C/V)
- Ctrl+letter: `(ch as u8) - b'a' + 1` (maps to 0x01–0x1A)
- Ctrl+special: `@`→0x00, `[`→0x1B, `\`→0x1C, `]`→0x1D, `^`→0x1E, `_`/`/`→0x1F
- Alt/Option: ESC prefix before key bytes
- Printable characters: UTF-8 encode

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
| PageUp/Down | `ESC [ 5~` / `ESC [ 6~` | same |
| Insert | `ESC [ 2~` | same |
| Delete | `ESC [ 3~` | same |
| Backspace | `0x7F` | `0x7F` |
| Tab | `0x09` (Shift+Tab = `ESC[Z`) | same |
| Enter | `0x0D` | `0x0D` |
| Escape | `0x1B` | `0x1B` |
| Space | `0x20` (Ctrl+Space = `0x00`) | same |

### Modifier Encoding

- xterm modifier parameter: 1=none, +1 shift, +2 alt, +4 ctrl
- Modified arrows: `ESC [ 1 ; <mod> <key>` (e.g., Shift+Up = `ESC [ 1 ; 2 A`)
- Modified F5+: `ESC [ <num> ; <mod> ~`

### Mouse Events (input.rs)

When mouse tracking modes are active:
- SGR encoding (mode 1006): `ESC [ < button ; col ; row M` (press) / `m` (release)
- Normal encoding: `ESC [ M` + 3 bytes (button+32, col+32, row+32), capped at 223
- Button values: 0=left, 1=middle, 2=right, 3=release (normal only), 32+=motion, 64=scroll-up, 65=scroll-down

### Scroll Wheel

- Trackpad (precise): accumulate `delta_y` in logical points, convert to lines via `cell_height_pts`
- Mouse wheel (non-precise): delta is in lines, multiply by `cell_height_pts` for accumulator
- Extract whole lines from accumulator, cap at 5 per frame to prevent PTY flooding
- Mouse mode: send scroll button events (64=up, 65=down)
- No mouse mode: send arrow up/down keys (respects DECCKM)

---

## Phase 7: Scrollback + Scroll Optimization

**Goal**: Efficient scrolling and scrollback buffer.

**Files**: `src/terminal/scrollback.rs`, `src/terminal/grid.rs`

### Ring Buffer

```rust
pub struct Scrollback {
    buf: Vec<Vec<Cell>>,   // Ring buffer of rows
    capacity: usize,        // Max rows (from config::SCROLLBACK_LINES)
    head: usize,            // Next write position
    len: usize,             // Current number of stored rows
}
```

- Push: store row at `buf[head]`, advance head modulo capacity
- Lazy allocation: `Vec::with_capacity(min(capacity, 1024))`, grows as needed
- Each row stored at the terminal width at time of eviction (fixed-width, per decision #15)
- `clear()`: reset buf, head, len

### Scroll Operations (grid.rs)

- **Scroll up** (`scroll_up`): `copy_within` (memmove) cells up within scroll region, clear new rows at bottom with current bg color, return evicted rows (only when `scroll_top == 0`)
- **Scroll down** (`scroll_down`): `copy_within` (memmove) cells down within scroll region, clear new rows at top with current bg color
- Both mark all rows in the scroll region dirty (`scroll_top..=scroll_bottom`)

### Efficiency

- `copy_within` for bulk cell movement (memmove, handles overlapping regions)
- Scrollback rows are not uploaded to GPU — only the visible grid's cells
- Erase operations use current SGR background color (per VT spec)

---

## Phase 8: Polish

### Selection

- State: `selection_start: Option<(u16, u16)>` and `selection_end: Option<(u16, u16)>` as `(col, row)`
- Mouse down: set start = end = cell position, set `mouse_pressed`
- Mouse drag: update end, call `update_selection()` which sets SELECTED flag on affected cells
- Cmd+C: `copy_selection()` extracts text from selected cells, writes to NSPasteboard
- `clear_selection()`: remove SELECTED flag from all cells, mark all dirty
- Render: shader swaps fg/bg for cells with SELECTED flag
- Not implemented: double-click word select, triple-click line select

### OSC 52 (Clipboard)

- Set: `OSC 52 ; <selection> ; <base64-data> ST` → custom base64 decoder → `set_clipboard()` via NSPasteboard
- Query (empty data): recognized but not yet implemented (TODO)
- Response encoded via internal response buffer mechanism

### Window Title

- OSC 0 / OSC 2: set window title via `NativeWindow::set_title()` (wraps `NSWindow::setTitle`)
- Title data passed through internal response buffer as `\x1B]title:<text>\x07`, decoded in `handle_responses()`

### Cursor Blink

- Timer: toggle cursor visibility every `CURSOR_BLINK_MS` (1000ms)
- On keypress: reset blink timer to visible state
- Cursor flag (CellFlags::CURSOR) set/cleared on the cursor cell each frame
- Previous cursor position tracked to clear stale CURSOR flags
- When DECTCEM (mode 25) is off: cursor forced invisible

### Resize

1. `Event::Resized { w, h, scale }` from NativeWindow → `MetalRenderer::resize()`
2. Recalculate grid dimensions from physical pixel size minus padding
3. Reallocate both double-buffer MTLBuffers
4. `Grid::resize()`: allocate new cells, copy preserved content, rebuild tab stops, reset scroll region
5. `Pty::resize()`: `ioctl(TIOCSWINSZ)` signals child process
6. Mark all rows dirty

### Bell

- BEL (0x07): currently a no-op (`// TODO: visual bell`)

---

## Color Palette

### Default ANSI (Dracula-inspired)

```
 0 Black       #000000     8 Bright Black   #666666
 1 Red         #ff5555     9 Bright Red     #ff6e6e
 2 Green       #50fa7b    10 Bright Green   #69ff94
 3 Yellow      #f1fa8c    11 Bright Yellow  #ffffa5
 4 Blue        #caa9fa    12 Bright Blue    #d6bfff
 5 Magenta     #ff79c6    13 Bright Magenta #ff92df
 6 Cyan        #8be9fd    14 Bright Cyan    #a4ffff
 7 White       #ffffff    15 Bright White   #ffffff
```

Indices 16–231: 6×6×6 color cube (standard formula).
Indices 232–255: grayscale ramp (standard formula).

Default foreground: index 7 (#ffffff)
Default background: index 0 (#000000)

Bold text: palette colors 0–7 brightened to 8–15 in shader. No font weight change.
