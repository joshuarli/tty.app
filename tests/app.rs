use std::time::Instant;

use tty::app_render;
use tty::renderer_trait::Renderer;
use tty::terminal::grid::Grid;
use tty::terminal::scrollback::Scrollback;

struct MockRenderer {
    cols: u32,
    rows: u32,
    cell_width: u32,
    cell_height: u32,
    scale_factor: f64,
    notch_px: u32,
    needs_render: bool,
    blocked: bool,
    render_count: usize,
    last_cursor_visible: bool,
    last_viewport_offset: usize,
}

impl MockRenderer {
    fn new(cols: u32, rows: u32, cell_width: u32, cell_height: u32) -> Self {
        Self {
            cols,
            rows,
            cell_width,
            cell_height,
            scale_factor: 2.0,
            notch_px: 0,
            needs_render: false,
            blocked: false,
            render_count: 0,
            last_cursor_visible: false,
            last_viewport_offset: 0,
        }
    }
}

impl Renderer for MockRenderer {
    fn render_frame(
        &mut self,
        grid: &mut Grid,
        _scrollback: &Scrollback,
        viewport_offset: usize,
        cursor_visible: bool,
    ) -> bool {
        if self.blocked {
            self.needs_render = true;
            return false;
        }
        let dirty = grid.dirty.any();
        grid.clear_dirty();
        let cursor_changed = cursor_visible != self.last_cursor_visible;
        if !dirty && !cursor_changed && !self.needs_render {
            return false;
        }
        self.render_count += 1;
        self.last_cursor_visible = cursor_visible;
        self.last_viewport_offset = viewport_offset;
        self.needs_render = false;
        true
    }

    fn resize(&mut self, width: u32, height: u32, scale: f64) {
        let padding = (tty::config::PADDING as f64 * scale) as u32;
        let padding_top = self.notch_px.max(padding);
        self.cols = (width - padding * 2) / self.cell_width;
        self.rows = (height - padding_top - padding) / self.cell_height;
        self.scale_factor = scale;
        self.needs_render = true;
    }

    fn cols(&self) -> u32 {
        self.cols
    }
    fn rows(&self) -> u32 {
        self.rows
    }
    fn cell_width(&self) -> u32 {
        self.cell_width
    }
    fn cell_height(&self) -> u32 {
        self.cell_height
    }
    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
    fn notch_px(&self) -> u32 {
        self.notch_px
    }
    fn needs_render(&self) -> bool {
        self.needs_render
    }
}

#[test]
fn render_frame_dispatches_when_dirty() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);

    grid.mark_dirty(0);
    let mut cursor_visible = false;
    assert!(!app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert_eq!(renderer.render_count, 1);
}

#[test]
fn render_frame_idle_when_nothing_changed() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);
    let mut cursor_visible = false;

    assert!(!app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert!(app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert_eq!(renderer.render_count, 1);
}

#[test]
fn render_frame_passes_cursor_visibility() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);
    let mut cursor_visible = false;
    grid.mode
        .remove(tty::terminal::grid::TermMode::CURSOR_VISIBLE);

    app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    );
    assert!(!renderer.last_cursor_visible);

    grid.mode
        .insert(tty::terminal::grid::TermMode::CURSOR_VISIBLE);
    app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    );
    assert!(renderer.last_cursor_visible);
}

#[test]
fn render_frame_defers_during_synchronized_output() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);
    let mut cursor_visible = false;
    grid.mode.insert(tty::terminal::grid::TermMode::SYNC_OUTPUT);
    grid.sync_start = Some(Instant::now());
    grid.mark_dirty(0);

    assert!(app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert_eq!(renderer.render_count, 0);
    assert!(grid.dirty[0]);
}

#[test]
fn render_frame_forwards_viewport_and_hides_cursor_in_scrollback() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);
    let mut cursor_visible = true;
    grid.mode
        .insert(tty::terminal::grid::TermMode::CURSOR_VISIBLE);
    grid.mark_dirty(0);

    assert!(!app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        3,
        &mut cursor_visible,
    ));
    assert!(!cursor_visible);
    assert_eq!(renderer.last_viewport_offset, 3);
}

#[test]
fn render_frame_retries_when_renderer_is_temporarily_blocked() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    let mut grid = Grid::new(80, 24);
    let scrollback = Scrollback::new(100);
    let mut cursor_visible = false;
    renderer.blocked = true;

    assert!(!app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert_eq!(renderer.render_count, 0);

    renderer.blocked = false;
    assert!(!app_render::render_frame(
        &mut renderer,
        &mut grid,
        &scrollback,
        0,
        &mut cursor_visible,
    ));
    assert_eq!(renderer.render_count, 1);
}

#[test]
fn resize_updates_dimensions() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    renderer.resize(1600, 1200, 2.0);
    assert_eq!(renderer.cols, 153);
    assert_eq!(renderer.rows, 56);
    assert!(renderer.needs_render);
}

#[test]
fn resize_small_dimensions_yields_zero_cols_rows() {
    let mut renderer = MockRenderer::new(80, 24, 10, 20);
    renderer.resize(64, 64, 2.0);
    // padding_px = 32, usable_w = 64 - 64 = 0
    assert_eq!(renderer.cols, 0);
    assert_eq!(renderer.rows, 0);
}

#[test]
fn accessors_return_configured_values() {
    let renderer = MockRenderer::new(120, 40, 8, 16);
    assert_eq!(renderer.cols(), 120);
    assert_eq!(renderer.rows(), 40);
    assert_eq!(renderer.cell_width(), 8);
    assert_eq!(renderer.cell_height(), 16);
    assert_eq!(renderer.scale_factor(), 2.0);
    assert_eq!(renderer.notch_px(), 0);
    assert!(!renderer.needs_render());
}
