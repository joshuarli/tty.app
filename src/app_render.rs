use crate::renderer_trait::Renderer;
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;

pub fn render_frame<R: Renderer + ?Sized>(
    renderer: &mut R,
    grid: &mut Grid,
    scrollback: &Scrollback,
    viewport_offset: usize,
    cursor_visible: &mut bool,
) -> bool {
    // Synchronized output (Mode 2026): defer rendering while the application
    // is mid-update. sync_start is set by the parser when mode 2026 is enabled
    // and cleared when disabled, so it precisely tracks each sync block.
    // Timeout after 100ms to prevent a stuck application from freezing the display.
    if let Some(start) = grid.sync_start {
        if start.elapsed().as_millis() < 100 {
            return true; // deferred — idle for now
        }
        // Timeout — render anyway and clear the flag
        grid.mode.remove(TermMode::SYNC_OUTPUT);
        grid.sync_start = None;
    }

    // Cursor visible when DECTCEM is set and viewing live (not scrollback)
    *cursor_visible = grid.mode.contains(TermMode::CURSOR_VISIBLE) && viewport_offset == 0;

    // render_frame returns true when GPU work was dispatched, false when idle.
    // A deferred render (GPU buffer busy) is not idle — we want to retry promptly.
    let dispatched = renderer.render_frame(grid, scrollback, viewport_offset, *cursor_visible);
    !dispatched && !renderer.needs_render()
}
