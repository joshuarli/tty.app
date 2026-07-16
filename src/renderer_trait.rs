use crate::terminal::grid::Grid;
use crate::terminal::scrollback::Scrollback;

/// Abstract renderer interface.
///
/// The trait covers everything `App` needs from the renderer, so `App::render()`
/// logic can be tested with a `MockRenderer` without Metal or a display.
pub trait Renderer {
    /// Render a frame. Returns true if GPU work was dispatched, false if idle.
    fn render_frame(
        &mut self,
        grid: &mut Grid,
        scrollback: &Scrollback,
        viewport_offset: usize,
        cursor_visible: bool,
    ) -> bool;

    /// Resize to the given physical dimensions and scale factor.
    /// Recalculates cols/rows/cell dimensions internally.
    fn resize(&mut self, width: u32, height: u32, scale: f64);

    fn cols(&self) -> u32;
    fn rows(&self) -> u32;
    fn cell_width(&self) -> u32;
    fn cell_height(&self) -> u32;
    fn scale_factor(&self) -> f64;
    fn needs_render(&self) -> bool;
}
