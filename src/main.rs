mod app_render;
mod clipboard;
mod config;
mod input;
mod parser;
mod perform_shared;
mod performer;
mod pty;
mod renderer;
mod renderer_trait;
mod terminal;
mod unicode;
mod window;

use std::ffi::c_void;
#[cfg(debug_assertions)]
use std::fs::{File, OpenOptions};
#[cfg(debug_assertions)]
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEventMask, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{
    CFFileDescriptor, CFFileDescriptorContext, CFRetained, CFRunLoop, CFRunLoopSource,
    kCFFileDescriptorReadCallBack, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_foundation::NSDefaultRunLoopMode;

use crate::clipboard::{base64_decode, clipboard_has_image, get_clipboard, set_clipboard};

use crate::parser::Parser;
use crate::performer::TermPerformer;
use crate::pty::Pty;
use crate::renderer::atlas::Atlas;
use crate::renderer::font::FontRasterizer;
use crate::renderer::metal::MetalRenderer;
use crate::renderer_trait::Renderer;
use crate::terminal::cell::CellFlags;
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;
use crate::window::{
    Event, Key, Modifiers, MouseButton, NativeWindow, init_app, new_window_requested,
};

/// Shared state between I/O thread and main thread.
struct SharedState {
    grid: Grid,
    scrollback: Scrollback,
    /// Terminal response buffer (DSR, window title, clipboard)
    response_buf: Vec<u8>,
    /// Whether the child process is still alive
    alive: bool,
}

struct App {
    renderer: Box<dyn Renderer>,
    rasterizer: FontRasterizer,
    atlas: Atlas,
    shared: SharedState,
    pty: Arc<Pty>,
    parser: Parser,
    modifiers: Modifiers,
    cursor_visible: bool,
    alive: bool,

    // Selection state
    selection_start: Option<(u16, u16)>, // (col, row)
    selection_end: Option<(u16, u16)>,   // (col, row)
    // Previously rendered selection range for targeted clearing
    prev_sel_rows: Option<(u16, u16)>, // (first_row, last_row) inclusive
    mouse_pressed: bool,
    pending_mouse_release: Option<Vec<u8>>,
    cursor_pos: (f64, f64), // Physical pixel position of mouse cursor

    // Scrollback viewport: 0 = live, >0 = N rows into history
    viewport_offset: usize,

    // Accumulated scroll delta (in logical points) for fractional accumulation
    scroll_accumulator: f64,

    // Reusable PTY read buffer (avoids 64KB stack alloc per frame)
    pty_buf: Vec<u8>,
}

impl App {
    fn mouse_button_code(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    fn new(win: &NativeWindow) -> Self {
        let scale = win.scale_factor();
        let (phys_w, phys_h) = win.physical_size();

        let rasterizer = FontRasterizer::new(config::FONT_FAMILY, config::FONT_SIZE, scale);
        let cell_width = rasterizer.metrics.cell_width;
        let cell_height = rasterizer.metrics.cell_height;

        let padding_px = (config::PADDING as f64 * scale) as u32;
        let cols = (phys_w - padding_px * 2) / cell_width;
        let rows = (phys_h - padding_px * 2) / cell_height;

        let mut renderer = MetalRenderer::new(
            win.view(),
            scale,
            phys_w,
            phys_h,
            cols,
            rows,
            cell_width,
            cell_height,
        );

        let mut atlas = Atlas::new(renderer.device(), cell_width, cell_height);
        atlas.preload_ascii(&rasterizer);
        renderer.atlas_texture = atlas.texture.clone();
        let renderer: Box<dyn Renderer> = Box::new(renderer);

        let mut grid = Grid::new(cols as u16, rows as u16);
        grid.set_ascii_atlas(&atlas.ascii_table_raw());
        grid.set_bold_ascii_atlas(&atlas.bold_ascii_table_raw());
        let scrollback = Scrollback::new(config::SCROLLBACK_LINES);

        let pty = Pty::spawn(
            cols as u16,
            rows as u16,
            cell_width as u16,
            cell_height as u16,
        )
        .expect("failed to spawn PTY");
        let pty = Arc::new(pty);

        Self {
            renderer,
            rasterizer,
            atlas,
            shared: SharedState {
                grid,
                scrollback,
                response_buf: Vec::new(),
                alive: true,
            },
            pty,
            parser: Parser::new(),
            modifiers: Modifiers::default(),
            cursor_visible: true,
            alive: true,
            selection_start: None,
            selection_end: None,
            prev_sel_rows: None,
            mouse_pressed: false,
            pending_mouse_release: None,
            cursor_pos: (0.0, 0.0),
            viewport_offset: 0,
            scroll_accumulator: 0.0,
            pty_buf: vec![0u8; 65536],
        }
    }

    fn pty_fd(&self) -> std::os::fd::RawFd {
        self.pty.fd()
    }

    fn is_alive(&self) -> bool {
        self.alive && self.shared.alive
    }

    /// Returns true if any PTY data was read.
    ///
    /// Reads at most `budget` bytes per call to prevent infinite-output
    /// commands (like `yes`) from starving the render/event loop.
    fn process_pty_output(&mut self, win: &NativeWindow, budget: usize) -> bool {
        let mut got_data = false;
        let mut total = 0;

        loop {
            match self.pty.read(&mut self.pty_buf) {
                Ok(0) => {
                    // EOF — shell exited
                    self.shared.alive = false;
                    break;
                }
                Ok(n) => {
                    got_data = true;
                    total += n;
                    let mut response_buf = std::mem::take(&mut self.shared.response_buf);
                    {
                        let mut performer = TermPerformer {
                            grid: &mut self.shared.grid,
                            scrollback: &mut self.shared.scrollback,
                            atlas: &mut self.atlas,
                            rasterizer: &self.rasterizer,
                            response_buf: &mut response_buf,
                        };
                        self.parser.parse(&self.pty_buf[..n], &mut performer);
                    }
                    self.shared.response_buf = response_buf;
                    if total >= budget {
                        break;
                    }
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    self.shared.alive = false;
                    break;
                }
            }
        }

        // Handle responses
        let responses = std::mem::take(&mut self.shared.response_buf);
        if !responses.is_empty() {
            self.handle_responses(&responses, win);
        }

        got_data
    }

    fn handle_responses(&self, data: &[u8], win: &NativeWindow) {
        let mut pos = 0;
        while pos < data.len() {
            if data[pos..].starts_with(b"\x1B]title:") {
                let start = pos + 8;
                if let Some(end) = data[start..].iter().position(|&b| b == 0x07) {
                    if let Ok(title) = std::str::from_utf8(&data[start..start + end]) {
                        win.set_title(title);
                    }
                    pos = start + end + 1;
                } else {
                    break;
                }
            } else if data[pos..].starts_with(b"\x1B]52;set:") {
                let start = pos + 9;
                if let Some(end) = data[start..].iter().position(|&b| b == 0x07) {
                    let b64 = &data[start..start + end];
                    if let Some(text_bytes) = base64_decode(b64)
                        && let Ok(text) = String::from_utf8(text_bytes)
                    {
                        set_clipboard(&text);
                    }
                    pos = start + end + 1;
                } else {
                    break;
                }
            } else if data[pos..].starts_with(b"\x1B]52;query\x07") {
                // TODO: respond with clipboard contents
                pos += 11;
            } else {
                // Regular response — write to PTY
                let end = data[pos + 1..]
                    .windows(2)
                    .position(|w| w == b"\x1B]")
                    .map(|e| pos + 1 + e)
                    .unwrap_or(data.len());
                let _ = self.pty.write(&data[pos..end]);
                pos = end;
            }
        }
    }

    fn flush_pending_mouse_release(&mut self) -> bool {
        if let Some(bytes) = self.pending_mouse_release.take() {
            let _ = self.pty.write(&bytes);
            true
        } else {
            false
        }
    }

    /// Returns true if the frame was idle (no GPU work dispatched).
    fn render(&mut self) -> bool {
        app_render::render_frame(
            &mut *self.renderer,
            &mut self.shared.grid,
            &self.shared.scrollback,
            self.viewport_offset,
            &mut self.cursor_visible,
        )
    }

    fn copy_selection(&self) {
        if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
            let mut text = String::new();
            let (start, end) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
                (start, end)
            } else {
                (end, start)
            };

            for row in start.1..=end.1 {
                let from_col = if row == start.1 { start.0 } else { 0 };
                let to_col = if row == end.1 {
                    end.0 + 1
                } else {
                    self.shared.grid.cols
                };

                for col in from_col..to_col {
                    let cell = self.shared.grid.cell(row, col);
                    if cell.flags.contains(CellFlags::WIDE_CONT) {
                        continue;
                    }
                    let ch = self.shared.grid.char_at(row, col);
                    if ch >= ' ' {
                        text.push(ch);
                    }
                }
                if row < end.1 {
                    text.push('\n');
                }
            }

            set_clipboard(&text);
        }
    }

    fn paste_clipboard(&self) {
        if let Some(text) = get_clipboard() {
            if text.is_empty() {
                return;
            }
            let bracketed = self.shared.grid.mode.contains(TermMode::BRACKETED_PASTE);

            if bracketed {
                // Strip embedded paste markers to prevent bracketed paste injection attacks.
                let sanitized = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
                let mut buf = Vec::with_capacity(sanitized.len() + 14);
                buf.extend_from_slice(b"\x1B[200~");
                buf.extend_from_slice(sanitized.as_bytes());
                buf.extend_from_slice(b"\x1B[201~");
                // The macOS PTY raw input queue (TTYHOG ≈ 1024 bytes) may not accept
                // the full buffer in one write().  Loop until all bytes are delivered
                // so the editor always receives the closing ESC[201~ and exits paste mode.
                let mut pos = 0;
                while pos < buf.len() {
                    match self.pty.write(&buf[pos..]) {
                        Ok(0) => break,
                        Ok(n) => pos += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // PTY buffer full — yield to let the editor drain it.
                            std::thread::yield_now();
                        }
                        Err(_) => break,
                    }
                }
            } else {
                let _ = self.pty.write(text.as_bytes());
            }
        } else if clipboard_has_image() {
            // No text but image data exists. Send Ctrl+V (0x16) so the
            // application can read the clipboard image directly (e.g. Claude
            // Code runs `osascript` to grab PNG data from NSPasteboard).
            let _ = self.pty.write(&[0x16]);
        }
    }

    /// Convert pixel position to (col, row) cell coordinates.
    /// Coordinates are in physical pixels.
    fn pixel_to_cell(&self, x: f64, y: f64) -> (u16, u16) {
        let scale = self.renderer.scale_factor();
        let padding = config::PADDING as f64 * scale;
        let px = x - padding;
        let py = y - padding;
        if px < 0.0 || py < 0.0 {
            return (0, 0);
        }
        let col = (px / self.renderer.cell_width() as f64) as u16;
        let row = (py / self.renderer.cell_height() as f64) as u16;
        let col = col.min(self.shared.grid.cols.saturating_sub(1));
        let row = row.min(self.shared.grid.rows.saturating_sub(1));
        (col, row)
    }

    fn clear_selection_flags(&mut self) {
        if let Some((first, last)) = self.prev_sel_rows {
            let cols = self.shared.grid.cols as usize;
            for row in first..=last.min(self.shared.grid.rows - 1) {
                let start = self.shared.grid.row_start(row);
                for cell in &mut self.shared.grid.cells[start..start + cols] {
                    cell.flags.remove(CellFlags::SELECTED);
                }
                self.shared.grid.mark_dirty(row);
            }
            self.prev_sel_rows = None;
        }
    }

    fn update_selection(&mut self) {
        if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
            // Normalize start/end
            let (start, end) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
                (start, end)
            } else {
                (end, start)
            };

            // Clear only previously selected rows
            self.clear_selection_flags();

            // Set new selection
            for row in start.1..=end.1 {
                let from_col = if row == start.1 { start.0 } else { 0 };
                let to_col = if row == end.1 {
                    end.0
                } else {
                    self.shared.grid.cols - 1
                };
                for col in from_col..=to_col {
                    self.shared
                        .grid
                        .cell_mut(row, col)
                        .flags
                        .insert(CellFlags::SELECTED);
                }
                self.shared.grid.mark_dirty(row);
            }
            self.prev_sel_rows = Some((start.1, end.1));
        }
    }

    fn clear_selection(&mut self) {
        if self.selection_start.is_some() {
            self.selection_start = None;
            self.selection_end = None;
            self.clear_selection_flags();
        }
    }

    fn handle_event(&mut self, event: &Event, _win: &NativeWindow) {
        match event {
            Event::Closed => {
                self.alive = false;
            }

            Event::Resized { w, h, scale } => {
                if *w == 0 || *h == 0 {
                    return;
                }
                self.renderer.resize(*w, *h, *scale);
                let cols = self.renderer.cols() as u16;
                let rows = self.renderer.rows() as u16;
                self.shared.grid.resize(cols, rows);
                self.pty.resize(
                    cols,
                    rows,
                    self.renderer.cell_width() as u16,
                    self.renderer.cell_height() as u16,
                );
            }

            Event::ModifiersChanged { modifiers } => {
                self.modifiers = *modifiers;
            }

            Event::KeyDown { key, modifiers } => {
                // Cmd+C/V shortcuts (Cmd+Q/N handled globally in main loop)
                if modifiers.super_key()
                    && let Key::Character(s) = key
                {
                    match s.as_str() {
                        "c" => {
                            self.copy_selection();
                            return;
                        }
                        "v" => {
                            self.paste_clipboard();
                            return;
                        }
                        _ => {}
                    }
                }

                // Snap back to live view on any keyboard input
                if self.viewport_offset > 0 {
                    self.viewport_offset = 0;
                    self.shared.grid.mark_all_dirty();
                }

                let term_mode = self.shared.grid.mode;

                if let Some(bytes) = input::key_to_bytes(key, modifiers, term_mode) {
                    let _ = self.pty.write(&bytes);
                    self.cursor_visible = true;
                }
            }

            Event::MouseDown {
                x,
                y,
                button,
                modifiers,
            } => {
                self.cursor_pos = (*x, *y);
                let cell = self.pixel_to_cell(*x, *y);
                let mouse_mode = self.shared.grid.mode.intersects(
                    TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL,
                );
                if mouse_mode {
                    let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
                    let bytes = input::mouse_to_bytes(
                        Self::mouse_button_code(*button),
                        modifiers,
                        cell.0 + 1,
                        cell.1 + 1,
                        true,
                        sgr,
                    );
                    let _ = self.pty.write(&bytes);
                    self.mouse_pressed = true;
                } else {
                    self.clear_selection();
                    self.mouse_pressed = true;
                    self.selection_start = Some(cell);
                    self.selection_end = Some(cell);
                }
            }

            Event::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => {
                self.cursor_pos = (*x, *y);
                let cell = self.pixel_to_cell(*x, *y);
                let mouse_mode = self.shared.grid.mode.intersects(
                    TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL,
                );
                if mouse_mode {
                    let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
                    let motion_mode = self
                        .shared
                        .grid
                        .mode
                        .intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL);
                    // tmux's MouseDragEnd binding runs copy-selection based on
                    // wherever the cursor was last moved by a drag event. The
                    // OS-delivered drag stream ends one frame before the button-up,
                    // so the last drag event is always slightly behind the actual
                    // release position. Send a synthetic drag at the release
                    // coordinates first so tmux's cursor lands on the right cell
                    // before the button-up triggers the copy.
                    if motion_mode && self.mouse_pressed {
                        let bytes = input::mouse_to_bytes(
                            32 | Self::mouse_button_code(*button),
                            modifiers,
                            cell.0 + 1,
                            cell.1 + 1,
                            true,
                            sgr,
                        );
                        let _ = self.pty.write(&bytes);
                    }
                    let release = if sgr {
                        input::mouse_to_bytes(
                            Self::mouse_button_code(*button),
                            modifiers,
                            cell.0 + 1,
                            cell.1 + 1,
                            false,
                            true,
                        )
                    } else {
                        input::mouse_to_bytes(3, modifiers, cell.0 + 1, cell.1 + 1, true, false)
                    };
                    if motion_mode && self.mouse_pressed {
                        self.pending_mouse_release = Some(release);
                    } else {
                        let _ = self.pty.write(&release);
                    }
                }
                self.mouse_pressed = false;
            }

            Event::MouseDragged {
                x,
                y,
                button,
                modifiers,
            } => {
                self.cursor_pos = (*x, *y);
                let motion_mode = self
                    .shared
                    .grid
                    .mode
                    .intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL);
                if motion_mode && self.mouse_pressed {
                    let cell = self.pixel_to_cell(*x, *y);
                    let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
                    // button 0 + 32 = motion flag
                    let bytes = input::mouse_to_bytes(
                        32 | Self::mouse_button_code(*button),
                        modifiers,
                        cell.0 + 1,
                        cell.1 + 1,
                        true,
                        sgr,
                    );
                    let _ = self.pty.write(&bytes);
                } else if !self.shared.grid.mode.intersects(
                    TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL,
                ) && self.mouse_pressed
                {
                    let cell = self.pixel_to_cell(*x, *y);
                    self.selection_end = Some(cell);
                    self.update_selection();
                }
            }

            Event::FocusIn => {
                self.shared.grid.mark_all_dirty();
                if self.shared.grid.mode.contains(TermMode::FOCUS_EVENTS) {
                    let _ = self.pty.write(b"\x1B[I");
                }
            }

            Event::FocusOut => {
                if self.shared.grid.mode.contains(TermMode::FOCUS_EVENTS) {
                    let _ = self.pty.write(b"\x1B[O");
                }
            }

            Event::ScrollWheel {
                x,
                y,
                delta_y,
                precise,
            } => {
                self.cursor_pos = (*x, *y);
                // Accumulate scroll delta — actual PTY events are flushed once
                // per frame (in flush_scroll) so that rapid trackpad events are
                // coalesced into a single batched write.
                let cell_height_pts =
                    self.renderer.cell_height() as f64 / self.renderer.scale_factor();
                if *precise {
                    self.scroll_accumulator += *delta_y;
                } else {
                    self.scroll_accumulator += *delta_y * cell_height_pts;
                }
            }
        }
    }

    /// Flush accumulated scroll delta as batched mouse/arrow events.
    /// Called once per frame after all events are processed, so that rapid
    /// trackpad events are coalesced into a single PTY write.
    fn flush_scroll(&mut self) {
        let cell_height_pts = self.renderer.cell_height() as f64 / self.renderer.scale_factor();
        let lines = (self.scroll_accumulator / cell_height_pts) as i32;
        if lines == 0 {
            return;
        }

        // Cap per-frame events at terminal height — one full page per frame is
        // plenty, and keeps the PTY buffer from overflowing with tmux redraws.
        let max_lines = self.shared.grid.rows as u32;
        let count = lines.unsigned_abs().min(max_lines);

        // Subtract only the consumed delta — excess carries to the next frame.
        let sign = if lines > 0 { 1.0 } else { -1.0 };
        self.scroll_accumulator -= count as f64 * cell_height_pts * sign;

        let mouse_mode = self
            .shared
            .grid
            .mode
            .intersects(TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL);

        let alt_screen = self.shared.grid.mode.contains(TermMode::ALT_SCREEN);

        if mouse_mode {
            let cell = self.pixel_to_cell(self.cursor_pos.0, self.cursor_pos.1);
            let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
            let button = if lines > 0 { 64u8 } else { 65u8 };
            let single =
                input::mouse_to_bytes(button, &self.modifiers, cell.0 + 1, cell.1 + 1, true, sgr);
            let mut batch = Vec::with_capacity(single.len() * count as usize);
            for _ in 0..count {
                batch.extend_from_slice(&single);
            }
            let _ = self.pty.write(&batch);
        } else if alt_screen {
            // Alt screen (vim, etc.): send arrow keys, no scrollback
            let app_cursor = self.shared.grid.mode.contains(TermMode::CURSOR_KEYS);
            let seq: &[u8] = if lines > 0 {
                if app_cursor { b"\x1BOA" } else { b"\x1B[A" }
            } else if app_cursor {
                b"\x1BOB"
            } else {
                b"\x1B[B"
            };
            let mut batch = Vec::with_capacity(seq.len() * count as usize);
            for _ in 0..count {
                batch.extend_from_slice(seq);
            }
            let _ = self.pty.write(&batch);
        } else {
            // Normal mode: navigate scrollback viewport
            let old = self.viewport_offset;
            if lines > 0 {
                // Scroll up into history
                let max = self.shared.scrollback.len();
                self.viewport_offset = (self.viewport_offset + count as usize).min(max);
            } else {
                // Scroll down toward live
                self.viewport_offset = self.viewport_offset.saturating_sub(count as usize);
            }
            if self.viewport_offset != old {
                self.shared.grid.mark_all_dirty();
            }
        }
    }
}

struct Terminal {
    win: NativeWindow,
    app: App,
}

struct DebugLogger {
    #[cfg(debug_assertions)]
    file: Option<File>,
    #[cfg(debug_assertions)]
    started: Instant,
    #[cfg(debug_assertions)]
    last_log: Instant,
}

impl DebugLogger {
    fn new() -> Self {
        #[cfg(debug_assertions)]
        {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("debug.log")
                .ok();
            let now = Instant::now();
            let mut logger = Self {
                file,
                started: now,
                last_log: now,
            };
            logger.write_line("start");
            logger
        }

        #[cfg(not(debug_assertions))]
        {
            Self {}
        }
    }

    #[cfg(debug_assertions)]
    fn write_line(&mut self, line: &str) {
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }

    #[cfg(debug_assertions)]
    fn log(&mut self, terminals: &[Terminal]) {
        if self.last_log.elapsed().as_secs() < 1 {
            return;
        }
        self.last_log = Instant::now();

        for (index, terminal) in terminals.iter().enumerate() {
            let app = &terminal.app;
            let (cells, chars, alt_cells, alt_chars) = app.shared.grid.debug_buffer_capacities();
            let (scrollback_vec, scrollback_rows, scrollback_max_row) =
                app.shared.scrollback.debug_buffer_capacities();
            let (osc, dcs) = app.parser.debug_buffer_capacities();
            self.write_line(&format!(
                "t={:.1}s term={index} cols={} rows={} grid_cells_cap={} grid_chars_cap={} alt_cells_cap={} alt_chars_cap={} scrollback_len={} scrollback_vec_cap={} scrollback_row_caps={} scrollback_max_row_cap={} osc_cap={} dcs_cap={} response_cap={} pty_cap={}",
                self.started.elapsed().as_secs_f64(),
                app.shared.grid.cols,
                app.shared.grid.rows,
                cells,
                chars,
                alt_cells,
                alt_chars,
                app.shared.scrollback.len(),
                scrollback_vec,
                scrollback_rows,
                scrollback_max_row,
                osc,
                dcs,
                app.shared.response_buf.capacity(),
                app.pty_buf.capacity(),
            ));
        }
    }

    #[cfg(not(debug_assertions))]
    fn log(&mut self, _terminals: &[Terminal]) {}
}

unsafe extern "C-unwind" fn pty_read_callback(
    descriptor: *mut CFFileDescriptor,
    flags: usize,
    _info: *mut c_void,
) {
    if flags & kCFFileDescriptorReadCallBack != 0 && !descriptor.is_null() {
        unsafe {
            (*descriptor).disable_call_backs(kCFFileDescriptorReadCallBack);
        }
    }
}

struct PtyRunLoopSources {
    run_loop: CFRetained<CFRunLoop>,
    descriptors: Vec<CFRetained<CFFileDescriptor>>,
    sources: Vec<CFRetained<CFRunLoopSource>>,
}

impl PtyRunLoopSources {
    fn new() -> Self {
        Self {
            run_loop: CFRunLoop::current().expect("no current Core Foundation run loop"),
            descriptors: Vec::new(),
            sources: Vec::new(),
        }
    }

    fn register(&mut self, fd: std::os::fd::RawFd) {
        let context = CFFileDescriptorContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let descriptor =
            unsafe { CFFileDescriptor::new(None, fd, false, Some(pty_read_callback), &context) }
                .expect("failed to create PTY run-loop descriptor");
        let source = CFFileDescriptor::new_run_loop_source(None, Some(&descriptor), 0)
            .expect("failed to create PTY run-loop source");
        let common_modes = unsafe {
            kCFRunLoopCommonModes
                .as_ref()
                .expect("Core Foundation common modes unavailable")
        };
        self.run_loop.add_source(Some(&source), Some(common_modes));
        descriptor.enable_call_backs(kCFFileDescriptorReadCallBack);
        self.descriptors.push(descriptor);
        self.sources.push(source);
    }

    fn enable_callbacks(&self) {
        for descriptor in &self.descriptors {
            descriptor.enable_call_backs(kCFFileDescriptorReadCallBack);
        }
    }

    fn unregister(&mut self, fd: std::os::fd::RawFd) {
        let Some(index) = self
            .descriptors
            .iter()
            .position(|descriptor| descriptor.native_descriptor() == fd)
        else {
            return;
        };

        let common_modes = unsafe {
            kCFRunLoopCommonModes
                .as_ref()
                .expect("Core Foundation common modes unavailable")
        };
        let source = self.sources.swap_remove(index);
        self.run_loop
            .remove_source(Some(&source), Some(common_modes));
        let descriptor = self.descriptors.swap_remove(index);
        descriptor.disable_call_backs(kCFFileDescriptorReadCallBack);
        descriptor.invalidate();
    }

    fn wait(&self, seconds: f64) {
        let default_mode = unsafe {
            kCFRunLoopDefaultMode
                .as_ref()
                .expect("Core Foundation default mode unavailable")
        };
        let _ = CFRunLoop::run_in_mode(Some(default_mode), seconds, true);
    }
}

fn spawn_terminal(mtm: MainThreadMarker) -> Terminal {
    let win = NativeWindow::new(mtm);
    let app = App::new(&win);
    Terminal { win, app }
}

fn run_startup_bench() {
    let total = Instant::now();
    let scale = 2.0;
    let phys_w = 2880;
    let phys_h = 1800;
    let t = Instant::now();
    let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("no Metal device found");
    let metal_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let rasterizer = FontRasterizer::new(config::FONT_FAMILY, config::FONT_SIZE, scale);
    let rasterizer_ms = t.elapsed().as_secs_f64() * 1000.0;

    let cell_width = rasterizer.metrics.cell_width;
    let cell_height = rasterizer.metrics.cell_height;
    let padding_px = (config::PADDING as f64 * scale) as u32;
    let cols = (phys_w - padding_px * 2) / cell_width;
    let rows = (phys_h - padding_px * 2) / cell_height;

    let t = Instant::now();
    let mut atlas = Atlas::new(&device, cell_width, cell_height);
    atlas.preload_ascii(&rasterizer);
    let atlas_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let mut grid = Grid::new(cols as u16, rows as u16);
    grid.set_ascii_atlas(&atlas.ascii_table_raw());
    grid.set_bold_ascii_atlas(&atlas.bold_ascii_table_raw());
    let _scrollback = Scrollback::new(config::SCROLLBACK_LINES);
    let grid_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let _pty = Pty::spawn(
        cols as u16,
        rows as u16,
        cell_width as u16,
        cell_height as u16,
    )
    .expect("failed to spawn PTY");
    let pty_ms = t.elapsed().as_secs_f64() * 1000.0;

    let total_ms = total.elapsed().as_secs_f64() * 1000.0;
    println!(
        "startup_bench_headless font=embedded_ttf total_ms={total_ms:.3} metal_ms={metal_ms:.3} rasterizer_ms={rasterizer_ms:.3} atlas_ms={atlas_ms:.3} grid_ms={grid_ms:.3} pty_ms={pty_ms:.3} cols={cols} rows={rows}"
    );
}

fn main() {
    if std::env::args().any(|a| a == "-v" || a == "--version") {
        let commit = &env!("TTY_RUSTC_COMMIT")[..7];
        println!("tty {} (rustc nightly {commit})", env!("CARGO_PKG_VERSION"));
        return;
    }

    if std::env::args().any(|a| a == "--stats") {
        unsafe { std::env::set_var("MTL_HUD_ENABLED", "1") };
    }

    if std::env::args().any(|a| a == "--startup-bench") {
        run_startup_bench();
        return;
    }

    let mtm = MainThreadMarker::new().expect("must be called from the main thread");
    let nsapp = init_app(mtm);

    let mut terminals: Vec<Terminal> = Vec::new();
    terminals.push(spawn_terminal(mtm));

    let mut pty_sources = PtyRunLoopSources::new();
    pty_sources.register(terminals[0].app.pty_fd());

    let mut state_events = Vec::new();
    let mut debug_logger = DebugLogger::new();

    loop {
        let (idle, quit) = objc2::rc::autoreleasepool(|_| {
            // Process PTY output for all terminals.
            // Budget caps bytes read per frame so infinite-output commands
            // (like `yes`) can't starve the render/event loop.
            const PTY_BUDGET: usize = 256 * 1024;
            let mut got_any_pty_data = false;
            for t in terminals.iter_mut() {
                got_any_pty_data |= t.app.process_pty_output(&t.win, PTY_BUDGET);
            }

            // Coalesce: after receiving data, wait up to 500µs for one more
            // batch. This prevents rendering intermediate states from split
            // writes (e.g., tmux hiding cursor, drawing, then showing cursor).
            // Single pass only — looping would hang on continuous output (yes).
            if got_any_pty_data {
                pty_sources.wait(0.0005);
                for t in terminals.iter_mut() {
                    t.app.process_pty_output(&t.win, PTY_BUDGET);
                }
            }

            let mut sent_deferred_mouse = false;
            for t in terminals.iter_mut() {
                sent_deferred_mouse |= t.app.flush_pending_mouse_release();
            }

            // Drain NSEvents globally
            let mut spawn_pending = false;
            let mut quit = false;
            let mut got_events = false;

            loop {
                let ns_event = nsapp.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    None,
                    // SAFETY: NSDefaultRunLoopMode is a global NSString constant,
                    // always valid in a running application.
                    unsafe { NSDefaultRunLoopMode },
                    true,
                );
                let ns_event = match ns_event {
                    Some(e) => e,
                    None => break,
                };

                let event_type = ns_event.r#type();
                let is_escape = event_type == NSEventType::KeyDown && ns_event.keyCode() == 0x35;

                // Global shortcuts: Cmd+Q quits, Cmd+N spawns a new window
                if event_type == NSEventType::KeyDown
                    && ns_event
                        .modifierFlags()
                        .contains(NSEventModifierFlags::Command)
                    && let Some(chars) = ns_event.charactersIgnoringModifiers()
                {
                    match chars.to_string().as_str() {
                        "q" => {
                            quit = true;
                            nsapp.sendEvent(&ns_event);
                            continue;
                        }
                        "n" => {
                            spawn_pending = true;
                            nsapp.sendEvent(&ns_event);
                            continue;
                        }
                        _ => {}
                    }
                }

                // Route to matching terminal by window pointer
                let mut handled = false;
                if let Some(t) = terminals
                    .iter_mut()
                    .find(|t| t.win.owns_ns_event(&ns_event, mtm))
                    && let Some(translated) = t.win.translate_ns_event(&ns_event)
                {
                    t.app.handle_event(&translated, &t.win);
                    got_events = true;
                    handled = true;
                }

                // Don't sendEvent for key-downs we already handled — AppKit's
                // responder chain has no keyDown: override, so unhandled keys
                // trigger NSBeep().  Also skip Escape to prevent AppKit from
                // exiting fullscreen via the responder chain.
                if !is_escape && !(handled && event_type == NSEventType::KeyDown) {
                    nsapp.sendEvent(&ns_event);
                }
            }

            // Check state changes (resize/focus) for all terminals
            for t in terminals.iter_mut() {
                state_events.clear();
                t.win.check_state_changes(&mut state_events);
                for event in &state_events {
                    t.app.handle_event(event, &t.win);
                    got_events = true;
                }
            }

            // Flush accumulated scroll for all terminals
            for t in terminals.iter_mut() {
                t.app.flush_scroll();
            }

            // Spawn new terminal if Cmd+N was pressed or dock menu clicked
            if spawn_pending || new_window_requested() {
                let t = spawn_terminal(mtm);
                pty_sources.register(t.app.pty_fd());
                terminals.push(t);
            }

            // Remove dead terminals (shell exited) and their run-loop sources.
            let dead_fds: Vec<_> = terminals
                .iter()
                .filter(|t| !t.app.is_alive())
                .map(|t| t.app.pty_fd())
                .collect();
            for fd in dead_fds {
                pty_sources.unregister(fd);
            }
            terminals.retain_mut(|t| {
                if t.app.is_alive() {
                    true
                } else {
                    t.win.close();
                    false
                }
            });

            // Render all terminals
            let mut all_idle = true;
            for t in terminals.iter_mut() {
                if t.win.is_focused() {
                    all_idle &= t.app.render();
                }
            }

            let idle = !got_any_pty_data && !sent_deferred_mouse && !got_events && all_idle;
            pty_sources.enable_callbacks();
            (idle, quit)
        });

        if quit || terminals.is_empty() {
            break;
        }

        debug_logger.log(&terminals);

        // When idle, block until AppKit or a PTY run-loop source wakes us.
        if idle {
            pty_sources.wait(f64::MAX);
        }
    }
}
