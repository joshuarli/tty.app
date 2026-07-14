# tty.app

tty.app is a small macOS terminal emulator for Apple Silicon. It is designed
to provide the terminal itself; tmux remains responsible for sessions, panes,
tabs, searchable history, and selection workflows.

The implementation is Rust with native AppKit windowing and a Metal compute
renderer. The supported terminal behavior is the xterm-256color subset needed
by shells and applications such as tmux, vim, and htop.

## Install

Edit the compile-time settings in `src/config.rs`, then run:

```sh
make install
```

## Design decisions

### The screen is also the GPU input

`Cell` is an 8-byte `#[repr(C)]` value shared by Rust and Metal:

```text
u16 codepoint | u16 flags | u8 foreground | u8 background | u8 atlas_x | u8 atlas_y
```

The grid can therefore copy dirty rows directly into Metal shared storage.
There is no render-time cell conversion or packing pass. The fixed layout is a
deliberate constraint: colors are palette indices, and combining marks,
grapheme shaping, and runtime font configuration are outside the model.

Glyph atlas coordinates are resolved when characters are written. ASCII uses
a preloaded 128-entry table; other glyphs are rasterized and cached on demand.
Hack Regular is embedded, with CoreText fallback for glyphs it does not cover.

### Scrolling rotates rows

The screen is a flat cell vector with ring-buffer row addressing. A full-screen
scroll advances the ring offset, clears the newly exposed row, and copies the
evicted row to scrollback. It does not move the rest of the screen. Partial
scroll regions use row copies.

### Rendering is damage-driven and asynchronous

The renderer tracks dirty rows separately for two shared cell buffers. It
uploads only pending rows and skips submission if the target buffer is still in
flight. Metal completion handlers mark buffers available for reuse; the
application loop never waits for GPU completion during normal rendering.

The production shader is cell-tiled: each terminal cell owns a threadgroup
that handles its glyph, palette attributes, decorations, wide-cell behavior,
selection, and cursor. Box drawing and arrows are rendered procedurally.

### One event-driven application loop

Each terminal owns an `App` and a native window. The main thread drains
non-blocking PTYs, processes AppKit events, handles input, and renders focused
windows. PTY descriptors are Core Foundation run-loop sources, so idle time
blocks until AppKit or PTY activity; there is no fixed idle timer, async
runtime, or PTY I/O thread.

After PTY data arrives, the loop performs one 500 µs run-loop wait and a second
read pass. This coalesces split writes from applications that update a screen
in several small writes. Continuous output is bounded by a 256 KiB per-frame
read budget so input and rendering still get turns.

PTY output is drained and parsed for unfocused windows, but their Metal
submission is deferred. Focus changes mark the screen for repaint.

### Synchronized output is a terminal mode

Mode 2026 defers rendering while an application performs a coordinated update.
Dirty state accumulates and is rendered once the mode ends. A 100 ms timeout
prevents a misbehaving application from keeping the display frozen.

## Deliberate limits

- 256-color palette storage; truecolor SGR is mapped to the nearest palette
  entry.
- One codepoint per cell; combining marks and ZWJ composition are ignored.
- Non-BMP characters use a parallel character store because the cell's
  codepoint field is `u16`.
- Font, palette, padding, and scrollback capacity are compile-time settings.
- Scrollback contains rendered cell rows, not searchable text.
