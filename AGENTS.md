# tty architecture

tty is a macOS terminal emulator implemented in Rust. Application logic runs
on the main thread. Rendering uses Metal compute shaders, and PTY I/O is
non-blocking and performed by the application loop.

## Source map

```text
src/main.rs                 App lifecycle, event loop, terminal windows
src/window.rs               AppKit window and event integration
src/pty/mod.rs              forkpty, non-blocking I/O, child lifecycle, resize
src/parser/                 VT parser and parser-to-performer interface
src/performer.rs            VT actions applied to terminal state
src/perform_shared.rs       Shared performer behavior used by tests
src/terminal/grid.rs         Ring-buffer screen, cursor, modes, dirty rows
src/terminal/cell.rs        8-byte CPU/GPU cell representation
src/terminal/scrollback.rs  Evicted screen rows
src/renderer/metal.rs       Metal device, buffers, uploads, dispatch
src/renderer/shader.metal   Cell-tiled and reference compute kernels
src/renderer/atlas.rs       Glyph atlas and runtime glyph allocation
src/renderer/font.rs        Embedded Hack and CoreText fallback rasterization
src/app_render.rs           Synchronized-output and renderer coordination
src/input.rs                Terminal input encoding
src/clipboard.rs            macOS clipboard and OSC 52 handling
src/config.rs               Compile-time font, palette, padding, and limits
```

## Runtime flow

Each terminal has an `App` and a `NativeWindow`. The main loop:

1. Drains each non-blocking PTY until `WouldBlock`, subject to a 256 KiB
   per-frame budget, using a reusable 64 KiB buffer.
2. If data was read, waits up to 500 µs on the Core Foundation run loop and
   performs one additional PTY read pass to coalesce split writes.
3. Drains AppKit events and translates them into terminal input or state
   changes.
4. Removes exited terminals and registers or unregisters their PTY run-loop
   sources.
5. Renders focused terminals.
6. If the frame is idle, blocks in `CFRunLoopRunInMode` until an AppKit event
   or registered PTY `CFFileDescriptor` source wakes it. The idle wait has no
   8 ms timeout and the code does not directly call `kqueue`.

PTY callbacks disable themselves after waking the loop; callbacks are
re-enabled after each frame. PTY reads and writes remain synchronous on the
main thread. Metal command buffers complete asynchronously.

## Parser and terminal state

`Parser::parse` uses three paths:

1. An AArch64 NEON/scalar scanner recognizes runs of printable ASCII.
2. A fast parser handles complete common `ESC [` sequences.
3. The VT500 state machine handles everything else, including split sequences,
   OSC, DCS, ESC, and UTF-8 state across parse calls.

`TermPerformer` bridges parser actions to `Grid`, `Scrollback`, the glyph
atlas, and the terminal response buffer. It owns cursor movement, scrolling,
erase and insert/delete operations, SGR, DEC modes, OSC/ESC dispatch, device
reports, mouse modes, bracketed paste, and synchronized output.

The fast CSI dispatch table is shared by the fast path and state-machine
fallback. RGB colors are converted to the nearest 256-color palette entry.

`Grid` stores cells in a flat vector with ring-buffer row addressing. Full
screen scroll advances the ring and clears the newly exposed row; partial
scrolls use row copies. A per-row dirty bitset drives renderer uploads.
Alt-screen state swaps the screen storage and associated ring state.

Printable characters resolve their glyph atlas coordinates when written:

- ASCII uses `Grid`'s 128-entry preloaded atlas table.
- Other glyphs use the atlas cache and rasterizer on demand.

The cursor's last-column wrap is deferred until the next printable character,
matching DECAWM behavior.

## Cell and renderer contract

`Cell` is `#[repr(C)]`, exactly 8 bytes, and matches the Metal cell structure:

```text
offset  size  field
0       2     BMP codepoint (u16)
2       2     CellFlags (u16)
4       1     foreground palette index
5       1     background palette index
6       1     atlas x coordinate
7       1     atlas y coordinate
```

Non-BMP characters use a parallel grid path. The fixed cell layout means
cells do not store truecolor values, combining marks, grapheme shaping, or
runtime font configuration.

The atlas is a 2048×2048 R8Unorm texture. ASCII glyphs are pinned and loaded
at startup; other glyphs are cached with eviction. Hack Regular is embedded,
with CoreText fallback for missing glyphs.

`MetalRenderer` maintains two shared cell buffers. It copies only pending
dirty rows into the target buffer, skips a frame when that buffer is still in
flight, dispatches the cell-tiled compute kernel, and marks the buffer ready
from the command-buffer completion handler. The shader resolves palette
colors, attributes, atlas glyphs, box drawing, arrows, decorations, wide-cell
continuations, selection, and cursor rendering.

Mode 2026 defers rendering while synchronized output is active. Dirty state
accumulates and rendering resumes when the mode ends, with a 100 ms timeout to
avoid a permanent display freeze.

## Deliberate limits

- Configuration is compile-time only.
- Cell colors are limited to the 256-entry palette.
- Each cell represents one codepoint; combining and grapheme shaping are not
  supported.
- Scrollback stores rendered cell rows and is not searchable text.

## Development constraints

- Prefer the simplest correct implementation; avoid unnecessary dependencies
  and abstractions.
- Do not add banner or separator comments.
- Preserve useful comments that explain non-obvious behavior or constants.
- Do not run pre-commit hooks.
- Do not push to a remote.
