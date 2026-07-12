# Coverage Plan

Close the coverage gap in macOS-framework-dependent modules.

## Approach

Two complementary strategies:

### A. Trait abstraction (Rust patterns)

Introduce traits behind each platform-dependent module so non-GPU, non-AppKit logic can be tested with mock implementations.

| Module | Trait | What it unlocks |
|--------|-------|----------------|
| `renderer/metal.rs` → `renderer/` | `Renderer` | Test `App::render()` (dirty bit merge, sync mode, cursor, scrollback viewport, selection) |
| `renderer/atlas.rs` | `TextureUpload` | Test LRU eviction, glyph caching, ASCII preload without Metal `Device` |
| `renderer/font.rs` | `Rasterize` | Test atlas insertion path, wide CJK fallback, missing-glyph handling without CoreText |
| `window.rs` | `EventSource` | Test `App::handle_event()` resize, Cmd+N, focus handling with synthetic events |
| `pty/mod.rs` | `TerminalIo` | Test key/mouse→PTY write path without `forkpty()` |

**Priority:** Renderer > Atlas/Font > Window > Pty

### B. Headless Metal tests

Metal does not require a window. Create a `MTLDevice`, compile shaders, dispatch compute on an off-screen texture, and read back pixels with `getBytes`. This tests the actual GPU pipeline without `NSWindow` or a display.

| Test | What it covers |
|------|----------------|
| Shader compilation | Metal pipeline state creation, function constants |
| Per-pixel kernel | Each layer: background fill, glyph sampling (atlas + box-drawing procedural), bold remap, hidden/inverse, underline/strikethrough, cursor inversion |
| Double-buffer handoff | Buffer flip, `add_completed_handler`, `needs_render` retry |
| Cell data layout | `Cell` → shader `CellData` struct alignment and field offsets |
| Palette resolution | `fg_palette[index]` and `bg_palette[index]` in shader, including bold → bright 0-7→8-15 |

Since the Cell struct IS the GPU format and the shader is the single source of truth for pixel output, headless tests provide the highest confidence for correctness of the rendering pipeline.

### Files to add

| File | Purpose |
|------|---------|
| `src/renderer/traits.rs` | `Renderer`, `TextureUpload`, `Rasterize` trait definitions |
| `src/renderer/mod.rs` | Re-export traits; mark metal/atlas/font as `pub(crate)` behind the traits |
| `tests/renderer.rs` | Headless Metal shader tests + mock-atlas performer tests |
| `tests/headless_metal.rs` | Off-screen compute dispatch, pixel readback assertions |
| `tests/app.rs` | `App` with mock renderer/window/pty — event handling, resize, multi-window |

### Out of scope

- `main.rs` — the glue layer. Its logic is routing between windows. Integration-tested through XCTest UI automation only.
- `performer.rs` — already tested via `TestPerformer`. Drift risk addressed by extracting shared dispatch functions into a utility module (lower priority).
- `table.rs` — static lookup tables, 2.75% coverage. Data declarations, not logic. Mark `#[cfg(not(tarpaulin_include))]` or accept as uncovered.
