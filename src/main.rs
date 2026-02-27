mod config;
mod input;
mod parser;
mod pty;
mod renderer;
mod terminal;
mod window;

use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSString, ns_string};

use crate::parser::Parser;
use crate::parser::charset::translate_dec_special;
use crate::parser::perform::Perform;
use crate::pty::Pty;
use crate::renderer::atlas::Atlas;
use crate::renderer::font::FontRasterizer;
use crate::renderer::metal::MetalRenderer;
use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;
use crate::window::{Event, Key, Modifiers, NativeWindow};

/// Shared state between I/O thread and main thread.
struct SharedState {
    grid: Grid,
    scrollback: Scrollback,
    /// Terminal response buffer (DSR, window title, clipboard)
    response_buf: Vec<u8>,
    /// Whether the child process is still alive
    alive: bool,
}

/// The performer that bridges parser actions to grid mutations.
struct TermPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
    atlas: &'a mut Atlas,
    rasterizer: &'a FontRasterizer,
    response_buf: &'a mut Vec<u8>,
}

impl<'a> Perform for TermPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        let use_dec = (self.grid.active_charset == 0 && self.grid.charset_g0 == 1)
            || (self.grid.active_charset == 1 && self.grid.charset_g1 == 1);

        for &b in bytes {
            let ch = if use_dec && (0x60..=0x7E).contains(&b) {
                translate_dec_special(b)
            } else {
                b as char
            };
            let cp = ch as u16;
            let pos = self.atlas.get_or_insert(cp, false, self.rasterizer);
            self.grid.write_char(ch, pos.x, pos.y);
        }
    }

    fn print(&mut self, c: char) {
        let cp = c as u32;
        if cp > 0xFFFF {
            let pos = self.atlas.get_or_insert(0xFFFD, false, self.rasterizer);
            self.grid.write_char('\u{FFFD}', pos.x, pos.y);
            return;
        }

        let wide = is_wide(cp);

        if wide {
            let pos = self.atlas.get_or_insert(cp as u16, true, self.rasterizer);
            self.grid.write_wide_char(c, pos.x, pos.y);
        } else if is_zero_width(cp) {
            // Zero-width combining marks — ignore for v1
        } else {
            let pos = self.atlas.get_or_insert(cp as u16, false, self.rasterizer);
            self.grid.write_char(c, pos.x, pos.y);
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {} // BEL (TODO: visual bell)
            0x08 => {
                // BS (backspace)
                if self.grid.cursor_col > 0 {
                    self.grid.cursor_col -= 1;
                    self.grid.cursor_pending_wrap = false;
                }
            }
            0x09 => {
                // TAB
                let col = self.grid.cursor_col;
                let cols = self.grid.cols;
                let mut next = col + 1;
                while next < cols {
                    if self.grid.tab_stops[next as usize] {
                        break;
                    }
                    next += 1;
                }
                self.grid.cursor_col = next.min(cols - 1);
                self.grid.cursor_pending_wrap = false;
            }
            0x0A..=0x0C => {
                // LF, VT, FF
                if self.grid.cursor_row == self.grid.scroll_bottom {
                    let evicted = self.grid.scroll_up(1);
                    for row in evicted {
                        self.scrollback.push(row);
                    }
                } else if self.grid.cursor_row < self.grid.rows - 1 {
                    self.grid.cursor_row += 1;
                }
                self.grid.mark_dirty(self.grid.cursor_row);
            }
            0x0D => {
                // CR
                self.grid.cursor_col = 0;
                self.grid.cursor_pending_wrap = false;
            }
            0x0E => self.grid.active_charset = 1, // SO → G1
            0x0F => self.grid.active_charset = 0, // SI → G0
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        let top = self.grid.scroll_top;
        self.grid.cursor_row = self.grid.cursor_row.saturating_sub(n).max(top);
        self.grid.cursor_pending_wrap = false;
        self.grid.mark_dirty(self.grid.cursor_row);
    }

    fn cursor_down(&mut self, n: u16) {
        let bottom = self.grid.scroll_bottom;
        self.grid.cursor_row = (self.grid.cursor_row + n).min(bottom);
        self.grid.cursor_pending_wrap = false;
        self.grid.mark_dirty(self.grid.cursor_row);
    }

    fn cursor_forward(&mut self, n: u16) {
        self.grid.cursor_col = (self.grid.cursor_col + n).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_backward(&mut self, n: u16) {
        self.grid.cursor_col = self.grid.cursor_col.saturating_sub(n);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_position(&mut self, row: u16, col: u16) {
        self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn erase_in_display(&mut self, mode: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        match mode {
            0 => {
                self.grid.clear_cols(row, col, self.grid.cols);
                self.grid.clear_rows(row + 1, self.grid.rows);
            }
            1 => {
                self.grid.clear_rows(0, row);
                self.grid.clear_cols(row, 0, col + 1);
            }
            2 | 3 => {
                self.grid.clear_rows(0, self.grid.rows);
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        match mode {
            0 => self.grid.clear_cols(row, col, self.grid.cols),
            1 => self.grid.clear_cols(row, 0, col + 1),
            2 => self.grid.clear_cols(row, 0, self.grid.cols),
            _ => {}
        }
    }

    fn scroll_up(&mut self, n: u16) {
        let evicted = self.grid.scroll_up(n);
        for row in evicted {
            self.scrollback.push(row);
        }
    }

    fn scroll_down(&mut self, n: u16) {
        self.grid.scroll_down(n);
    }

    fn insert_lines(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        if row < self.grid.scroll_top || row > self.grid.scroll_bottom {
            return;
        }
        let old_top = self.grid.scroll_top;
        self.grid.scroll_top = row;
        self.grid.scroll_down(n);
        self.grid.scroll_top = old_top;
        self.grid.cursor_col = 0;
    }

    fn delete_lines(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        if row < self.grid.scroll_top || row > self.grid.scroll_bottom {
            return;
        }
        let old_top = self.grid.scroll_top;
        self.grid.scroll_top = row;
        self.grid.scroll_up(n);
        self.grid.scroll_top = old_top;
        self.grid.cursor_col = 0;
    }

    fn insert_chars(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        let cols = self.grid.cols;
        let n = n.min(cols - col);

        let row_start = row as usize * cols as usize;
        for c in (col + n..cols).rev() {
            self.grid.cells[row_start + c as usize] = self.grid.cells[row_start + (c - n) as usize];
        }
        let attr = self.grid.attr;
        for c in col..col + n {
            self.grid.cells[row_start + c as usize].erase(&attr);
        }
        self.grid.mark_dirty(row);
    }

    fn delete_chars(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        let cols = self.grid.cols;
        let n = n.min(cols - col);

        let row_start = row as usize * cols as usize;
        for c in col..cols - n {
            self.grid.cells[row_start + c as usize] = self.grid.cells[row_start + (c + n) as usize];
        }
        let attr = self.grid.attr;
        for c in cols - n..cols {
            self.grid.cells[row_start + c as usize].erase(&attr);
        }
        self.grid.mark_dirty(row);
    }

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.grid.attr.flags = CellFlags::empty();
                    self.grid.attr.fg_index = 7;
                    self.grid.attr.bg_index = 0;
                    self.grid.attr.fg_rgb = config::DEFAULT_FG;
                    self.grid.attr.bg_rgb = config::DEFAULT_BG;
                }
                1 => self.grid.attr.flags.insert(CellFlags::BOLD),
                2 => self.grid.attr.flags.insert(CellFlags::DIM),
                3 => self.grid.attr.flags.insert(CellFlags::ITALIC),
                4 => self.grid.attr.flags.insert(CellFlags::UNDERLINE),
                7 => self.grid.attr.flags.insert(CellFlags::INVERSE),
                8 => self.grid.attr.flags.insert(CellFlags::HIDDEN),
                9 => self.grid.attr.flags.insert(CellFlags::STRIKE),
                21 | 22 => {
                    self.grid.attr.flags.remove(CellFlags::BOLD);
                    self.grid.attr.flags.remove(CellFlags::DIM);
                }
                23 => self.grid.attr.flags.remove(CellFlags::ITALIC),
                24 => self.grid.attr.flags.remove(CellFlags::UNDERLINE),
                27 => self.grid.attr.flags.remove(CellFlags::INVERSE),
                28 => self.grid.attr.flags.remove(CellFlags::HIDDEN),
                29 => self.grid.attr.flags.remove(CellFlags::STRIKE),
                30..=37 => self.grid.attr.fg_index = (params[i] - 30) as u8,
                38 => {
                    i += 1;
                    if i < params.len() {
                        match params[i] {
                            5 => {
                                i += 1;
                                if i < params.len() {
                                    self.grid.attr.fg_index = params[i] as u8;
                                }
                            }
                            2 => {
                                if i + 3 < params.len() {
                                    let r = params[i + 1] as u32;
                                    let g = params[i + 2] as u32;
                                    let b = params[i + 3] as u32;
                                    self.grid.attr.fg_rgb = (r << 16) | (g << 8) | b;
                                    self.grid.attr.fg_index = 0xFF;
                                    i += 3;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                39 => {
                    self.grid.attr.fg_index = 7;
                    self.grid.attr.fg_rgb = config::DEFAULT_FG;
                }
                40..=47 => self.grid.attr.bg_index = (params[i] - 40) as u8,
                48 => {
                    i += 1;
                    if i < params.len() {
                        match params[i] {
                            5 => {
                                i += 1;
                                if i < params.len() {
                                    self.grid.attr.bg_index = params[i] as u8;
                                }
                            }
                            2 => {
                                if i + 3 < params.len() {
                                    let r = params[i + 1] as u32;
                                    let g = params[i + 2] as u32;
                                    let b = params[i + 3] as u32;
                                    self.grid.attr.bg_rgb = (r << 16) | (g << 8) | b;
                                    self.grid.attr.bg_index = 0xFF;
                                    i += 3;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                49 => {
                    self.grid.attr.bg_index = 0;
                    self.grid.attr.bg_rgb = config::DEFAULT_BG;
                }
                90..=97 => self.grid.attr.fg_index = (params[i] - 90 + 8) as u8,
                100..=107 => self.grid.attr.bg_index = (params[i] - 100 + 8) as u8,
                _ => {}
            }
            i += 1;
        }
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    1 => self.grid.mode.insert(TermMode::CURSOR_KEYS),
                    7 => self.grid.mode.insert(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.insert(TermMode::CURSOR_VISIBLE),
                    47 | 1047 => self.grid.enter_alt_screen(),
                    1049 => {
                        self.grid.save_cursor();
                        self.grid.enter_alt_screen();
                    }
                    1000 => self.grid.mode.insert(TermMode::MOUSE_BUTTON),
                    1002 => self.grid.mode.insert(TermMode::MOUSE_MOTION),
                    1003 => self.grid.mode.insert(TermMode::MOUSE_ALL),
                    1004 => self.grid.mode.insert(TermMode::FOCUS_EVENTS),
                    1006 => self.grid.mode.insert(TermMode::MOUSE_SGR),
                    2004 => self.grid.mode.insert(TermMode::BRACKETED_PASTE),
                    2026 => self.grid.mode.insert(TermMode::SYNC_OUTPUT),
                    _ => {}
                }
            }
        }
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    1 => self.grid.mode.remove(TermMode::CURSOR_KEYS),
                    7 => self.grid.mode.remove(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.remove(TermMode::CURSOR_VISIBLE),
                    47 | 1047 => self.grid.exit_alt_screen(),
                    1049 => {
                        self.grid.exit_alt_screen();
                        self.grid.restore_cursor();
                    }
                    1000 => self.grid.mode.remove(TermMode::MOUSE_BUTTON),
                    1002 => self.grid.mode.remove(TermMode::MOUSE_MOTION),
                    1003 => self.grid.mode.remove(TermMode::MOUSE_ALL),
                    1004 => self.grid.mode.remove(TermMode::FOCUS_EVENTS),
                    1006 => self.grid.mode.remove(TermMode::MOUSE_SGR),
                    2004 => self.grid.mode.remove(TermMode::BRACKETED_PASTE),
                    2026 => self.grid.mode.remove(TermMode::SYNC_OUTPUT),
                    _ => {}
                }
            }
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = if bottom == 0 {
            self.grid.rows - 1
        } else {
            (bottom.saturating_sub(1)).min(self.grid.rows - 1)
        };
        if top < bottom {
            self.grid.scroll_top = top;
            self.grid.scroll_bottom = bottom;
            self.grid.cursor_row = 0;
            self.grid.cursor_col = 0;
            self.grid.cursor_pending_wrap = false;
        }
    }

    fn tab_clear(&mut self, mode: u16) {
        match mode {
            0 => {
                let col = self.grid.cursor_col as usize;
                if col < self.grid.tab_stops.len() {
                    self.grid.tab_stops.set(col, false);
                }
            }
            3 => self.grid.tab_stops.fill(false),
            _ => {}
        }
    }

    fn set_tab_stop(&mut self) {
        let col = self.grid.cursor_col as usize;
        if col < self.grid.tab_stops.len() {
            self.grid.tab_stops.set(col, true);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        if params.is_empty() {
            return;
        }
        let num = std::str::from_utf8(params[0])
            .ok()
            .and_then(|s| s.parse::<u16>().ok());

        match num {
            Some(0) | Some(2) => {
                if params.len() > 1 {
                    let title: Vec<u8> = params[1..].join(&b';');
                    self.response_buf.extend_from_slice(b"\x1B]title:");
                    self.response_buf.extend_from_slice(&title);
                    self.response_buf.push(0x07);
                }
            }
            Some(52) => {
                if params.len() >= 3 {
                    let data = params[2];
                    if data.is_empty() {
                        self.response_buf.extend_from_slice(b"\x1B]52;query\x07");
                    } else {
                        self.response_buf.extend_from_slice(b"\x1B]52;set:");
                        self.response_buf.extend_from_slice(data);
                        self.response_buf.push(0x07);
                    }
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.grid.save_cursor(),
            ([], b'8') => self.grid.restore_cursor(),
            ([], b'D') => {
                // IND
                if self.grid.cursor_row == self.grid.scroll_bottom {
                    let evicted = self.grid.scroll_up(1);
                    for row in evicted {
                        self.scrollback.push(row);
                    }
                } else if self.grid.cursor_row < self.grid.rows - 1 {
                    self.grid.cursor_row += 1;
                }
            }
            ([], b'E') => {
                // NEL
                self.grid.cursor_col = 0;
                if self.grid.cursor_row == self.grid.scroll_bottom {
                    let evicted = self.grid.scroll_up(1);
                    for row in evicted {
                        self.scrollback.push(row);
                    }
                } else if self.grid.cursor_row < self.grid.rows - 1 {
                    self.grid.cursor_row += 1;
                }
            }
            ([], b'H') => self.set_tab_stop(),
            ([], b'M') => {
                // RI
                if self.grid.cursor_row == self.grid.scroll_top {
                    self.grid.scroll_down(1);
                } else if self.grid.cursor_row > 0 {
                    self.grid.cursor_row -= 1;
                }
            }
            ([], b'c') => {
                // RIS
                let rows = self.grid.rows;
                self.grid.clear_rows(0, rows);
                self.grid.cursor_row = 0;
                self.grid.cursor_col = 0;
                self.grid.attr = Cell::default();
                self.grid.mode = TermMode::AUTO_WRAP | TermMode::CURSOR_VISIBLE;
                self.grid.scroll_top = 0;
                self.grid.scroll_bottom = rows - 1;
                self.grid.charset_g0 = 0;
                self.grid.charset_g1 = 0;
                self.grid.active_charset = 0;
            }
            ([b'('], b'0') => self.grid.charset_g0 = 1,
            ([b'('], b'B') => self.grid.charset_g0 = 0,
            ([b')'], b'0') => self.grid.charset_g1 = 1,
            ([b')'], b'B') => self.grid.charset_g1 = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'X') => {
                // ECH
                let n = params.first().copied().unwrap_or(1).max(1);
                let row = self.grid.cursor_row;
                let col = self.grid.cursor_col;
                self.grid
                    .clear_cols(row, col, (col + n).min(self.grid.cols));
            }
            ([], b'c') => {
                // DA — report as VT220
                self.response_buf.extend_from_slice(b"\x1B[?62;c");
            }
            ([b'>'], b'c') => {
                // DA2
                self.response_buf.extend_from_slice(b"\x1B[>0;0;0c");
            }
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        self.grid.save_cursor();
    }

    fn restore_cursor(&mut self) {
        self.grid.restore_cursor();
    }

    fn device_status_report(&mut self, mode: u16) {
        match mode {
            5 => self.response_buf.extend_from_slice(b"\x1B[0n"),
            6 => {
                let row = self.grid.cursor_row + 1;
                let col = self.grid.cursor_col + 1;
                let resp = format!("\x1B[{};{}R", row, col);
                self.response_buf.extend_from_slice(resp.as_bytes());
            }
            _ => {}
        }
    }
}

fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33BF |
        0x3400..=0x4DBF | 0x4E00..=0xA4CF | 0xA960..=0xA97C |
        0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE10..=0xFE6F |
        0xFF01..=0xFF60 | 0xFFE0..=0xFFE6
    )
}

fn is_zero_width(cp: u32) -> bool {
    matches!(cp,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD |
        0x0610..=0x061A | 0x064B..=0x065F | 0x0670 |
        0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 |
        0x06EA..=0x06ED | 0x0711 | 0x0730..=0x074A |
        0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2069 |
        0xFE00..=0xFE0F | 0xFEFF
    )
}

struct App {
    renderer: MetalRenderer,
    rasterizer: FontRasterizer,
    atlas: Atlas,
    shared: SharedState,
    pty: Arc<Pty>,
    parser: Parser,
    modifiers: Modifiers,
    cursor_visible: bool,
    last_blink: Instant,
    alive: bool,

    // Selection state
    selection_start: Option<(u16, u16)>, // (col, row)
    selection_end: Option<(u16, u16)>,   // (col, row)
    mouse_pressed: bool,
    cursor_pos: (f64, f64), // Physical pixel position of mouse cursor

    // Previous cursor position for clearing stale cursor flags
    prev_cursor_row: u16,
    prev_cursor_col: u16,

    // Timestamp when synchronized output (Mode 2026) was last enabled
    sync_start: Instant,
}

impl App {
    fn new(win: &NativeWindow) -> Self {
        let scale = win.scale_factor();
        let (phys_w, phys_h) = win.physical_size();

        let rasterizer = FontRasterizer::new(config::FONT_FAMILY, config::FONT_SIZE, scale);
        let cell_width = rasterizer.metrics.cell_width;
        let cell_height = rasterizer.metrics.cell_height;

        let padding_px = (config::PADDING as f64 * scale) as u32;
        let padding_top_px = padding_px.max(win.safe_area_top());
        let cols = (phys_w - padding_px * 2) / cell_width;
        let rows = (phys_h - padding_top_px - padding_px) / cell_height;

        let mut renderer = MetalRenderer::new(
            win.view(),
            scale,
            phys_w,
            phys_h,
            cols,
            rows,
            cell_width,
            cell_height,
            win.safe_area_top(),
        );

        let mut atlas = Atlas::new(renderer.device(), cell_width, cell_height);
        atlas.preload_ascii(&rasterizer);
        renderer.atlas_texture = atlas.texture.clone();

        let grid = Grid::new(cols as u16, rows as u16);
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
            last_blink: Instant::now(),
            alive: true,
            selection_start: None,
            selection_end: None,
            mouse_pressed: false,
            cursor_pos: (0.0, 0.0),
            prev_cursor_row: 0,
            prev_cursor_col: 0,
            sync_start: Instant::now(),
        }
    }

    /// Returns true if any PTY data was read.
    fn process_pty_output(&mut self, win: &NativeWindow) -> bool {
        let pty = self.pty.clone();
        let mut buf = [0u8; 65536];
        let mut got_data = false;

        loop {
            match pty.read(&mut buf) {
                Ok(0) => {
                    // EOF — shell exited
                    self.shared.alive = false;
                    break;
                }
                Ok(n) => {
                    got_data = true;
                    let was_syncing = self.shared.grid.mode.contains(TermMode::SYNC_OUTPUT);
                    let mut response_buf = std::mem::take(&mut self.shared.response_buf);
                    {
                        let mut performer = TermPerformer {
                            grid: &mut self.shared.grid,
                            scrollback: &mut self.shared.scrollback,
                            atlas: &mut self.atlas,
                            rasterizer: &self.rasterizer,
                            response_buf: &mut response_buf,
                        };
                        self.parser.parse(&buf[..n], &mut performer);
                    }
                    self.shared.response_buf = response_buf;

                    // Record when synchronized output begins
                    if !was_syncing && self.shared.grid.mode.contains(TermMode::SYNC_OUTPUT) {
                        self.sync_start = Instant::now();
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
                    if let Ok(text_bytes) = base64_decode(b64)
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

    /// Returns true if the frame was idle (no GPU work dispatched).
    fn render(&mut self, win: &NativeWindow) -> bool {
        // Drain any PTY data that arrived since last poll.
        self.process_pty_output(win);

        // Synchronized output (Mode 2026): defer rendering while the application
        // is mid-update. Dirty bits accumulate and get flushed on the first frame
        // after sync ends. Timeout after 100ms to prevent a stuck application from
        // freezing the display.
        if self.shared.grid.mode.contains(TermMode::SYNC_OUTPUT) {
            let elapsed = Instant::now().duration_since(self.sync_start);
            if elapsed.as_millis() < 100 {
                return true; // deferred — idle for now
            }
            // Timeout — render anyway and clear the flag
            self.shared.grid.mode.remove(TermMode::SYNC_OUTPUT);
        }

        // Cursor blink — only when DECTCEM (CURSOR_VISIBLE mode) is set
        let dectcem = self.shared.grid.mode.contains(TermMode::CURSOR_VISIBLE);
        let now = Instant::now();
        let prev_visible = self.cursor_visible;

        if !dectcem {
            self.cursor_visible = false;
        } else if now.duration_since(self.last_blink).as_millis() >= config::CURSOR_BLINK_MS as u128
        {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = now;
        }

        // Update cursor cell flag — only if position or visibility changed
        let cursor_row = self.shared.grid.cursor_row;
        let cursor_col = self.shared.grid.cursor_col;
        let prev_row = self.prev_cursor_row;
        let prev_col = self.prev_cursor_col;
        let cursor_moved = cursor_row != prev_row || cursor_col != prev_col;
        let blink_changed = self.cursor_visible != prev_visible;

        if cursor_moved || blink_changed {
            // Clear CURSOR flag from previous position
            if prev_row < self.shared.grid.rows && prev_col < self.shared.grid.cols {
                self.shared
                    .grid
                    .cell_mut(prev_row, prev_col)
                    .flags
                    .remove(CellFlags::CURSOR);
                self.shared.grid.mark_dirty(prev_row);
            }

            // Set CURSOR flag at new position (only if visible)
            if self.cursor_visible {
                self.shared
                    .grid
                    .cell_mut(cursor_row, cursor_col)
                    .flags
                    .insert(CellFlags::CURSOR);
            }
            self.shared.grid.mark_dirty(cursor_row);

            self.prev_cursor_row = cursor_row;
            self.prev_cursor_col = cursor_col;
        }

        // render_frame returns true when GPU work was dispatched, false when idle
        let dispatched = self
            .renderer
            .render_frame(&mut self.shared.grid, self.cursor_visible);
        !dispatched
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
                    if cell.codepoint >= 0x20
                        && let Some(ch) = char::from_u32(cell.codepoint as u32)
                    {
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
            let bracketed = self.shared.grid.mode.contains(TermMode::BRACKETED_PASTE);

            if bracketed {
                let _ = self.pty.write(b"\x1B[200~");
                let _ = self.pty.write(text.as_bytes());
                let _ = self.pty.write(b"\x1B[201~");
            } else {
                let _ = self.pty.write(text.as_bytes());
            }
        }
    }

    /// Convert pixel position to (col, row) cell coordinates.
    /// Coordinates are in physical pixels.
    fn pixel_to_cell(&self, x: f64, y: f64) -> (u16, u16) {
        let scale = self.renderer.scale_factor;
        let padding = config::PADDING as f64 * scale;
        let padding_top = (self.renderer.notch_px as f64).max(padding);
        let px = x - padding;
        let py = y - padding_top;
        if px < 0.0 || py < 0.0 {
            return (0, 0);
        }
        let col = (px / self.renderer.cell_width as f64) as u16;
        let row = (py / self.renderer.cell_height as f64) as u16;
        let col = col.min(self.shared.grid.cols.saturating_sub(1));
        let row = row.min(self.shared.grid.rows.saturating_sub(1));
        (col, row)
    }

    fn update_selection(&mut self) {
        if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
            // Normalize start/end
            let (start, end) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
                (start, end)
            } else {
                (end, start)
            };

            // Clear old selection
            let total = self.shared.grid.cols as usize * self.shared.grid.rows as usize;
            for i in 0..total {
                self.shared.grid.cells[i].flags.remove(CellFlags::SELECTED);
            }

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
        }
    }

    fn clear_selection(&mut self) {
        if self.selection_start.is_some() {
            self.selection_start = None;
            self.selection_end = None;
            let total = self.shared.grid.cols as usize * self.shared.grid.rows as usize;
            for i in 0..total {
                self.shared.grid.cells[i].flags.remove(CellFlags::SELECTED);
            }
            self.shared.grid.mark_all_dirty();
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
                let cols = self.renderer.cols as u16;
                let rows = self.renderer.rows as u16;
                self.shared.grid.resize(cols, rows);
                self.pty.resize(
                    cols,
                    rows,
                    self.renderer.cell_width as u16,
                    self.renderer.cell_height as u16,
                );
            }

            Event::ModifiersChanged { modifiers } => {
                self.modifiers = *modifiers;
            }

            Event::KeyDown { key, modifiers } => {
                // Cmd+Q/C/V shortcuts
                if modifiers.super_key()
                    && let Key::Character(s) = key
                {
                    match s.as_str() {
                        "q" => {
                            self.alive = false;
                            return;
                        }
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

                let term_mode = self.shared.grid.mode;

                if let Some(bytes) = input::key_to_bytes(key, modifiers, term_mode) {
                    let _ = self.pty.write(&bytes);
                    self.cursor_visible = true;
                    self.last_blink = Instant::now();
                }
            }

            Event::MouseDown { x, y } => {
                self.cursor_pos = (*x, *y);
                let cell = self.pixel_to_cell(*x, *y);
                let mouse_mode = self.shared.grid.mode.intersects(
                    TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL,
                );
                if mouse_mode {
                    let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
                    let bytes = input::mouse_to_bytes(0, cell.0 + 1, cell.1 + 1, true, sgr);
                    let _ = self.pty.write(&bytes);
                    self.mouse_pressed = true;
                } else {
                    self.clear_selection();
                    self.mouse_pressed = true;
                    self.selection_start = Some(cell);
                    self.selection_end = Some(cell);
                }
            }

            Event::MouseUp { x, y } => {
                self.cursor_pos = (*x, *y);
                let cell = self.pixel_to_cell(*x, *y);
                let mouse_mode = self.shared.grid.mode.intersects(
                    TermMode::MOUSE_BUTTON | TermMode::MOUSE_MOTION | TermMode::MOUSE_ALL,
                );
                if mouse_mode {
                    let sgr = self.shared.grid.mode.contains(TermMode::MOUSE_SGR);
                    if sgr {
                        let bytes = input::mouse_to_bytes(0, cell.0 + 1, cell.1 + 1, false, true);
                        let _ = self.pty.write(&bytes);
                    } else {
                        let bytes = input::mouse_to_bytes(3, cell.0 + 1, cell.1 + 1, true, false);
                        let _ = self.pty.write(&bytes);
                    }
                }
                self.mouse_pressed = false;
            }

            Event::MouseDragged { x, y } => {
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
                    let bytes = input::mouse_to_bytes(32, cell.0 + 1, cell.1 + 1, true, sgr);
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
        }
    }
}

// ── Clipboard helpers ──

fn set_clipboard(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let pasteboard_type = ns_string!("public.utf8-plain-text");
    let types = NSArray::from_slice(&[pasteboard_type]);
    unsafe { pb.declareTypes_owner(&types, None) };
    let ns_text = NSString::from_str(text);
    pb.setString_forType(&ns_text, pasteboard_type);
}

fn get_clipboard() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    let pasteboard_type = ns_string!("public.utf8-plain-text");
    pb.stringForType(pasteboard_type).map(|s| s.to_string())
}

fn base64_decode(input: &[u8]) -> Result<Vec<u8>, ()> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in input {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return Err(()),
        };
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
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

    let mut win = NativeWindow::new();
    let mut app = App::new(&win);

    loop {
        let idle = objc2::rc::autoreleasepool(|_| {
            let got_pty_data = app.process_pty_output(&win);

            let events = win.poll_events();
            let got_events = !events.is_empty();
            for event in &events {
                app.handle_event(event, &win);
            }

            let frame_idle = app.render(&win);
            !got_pty_data && !got_events && frame_idle
        });

        if !app.alive || !app.shared.alive {
            break;
        }

        // When nothing happened this frame, sleep briefly to avoid busy-spinning.
        // Vsync already throttles active frames; this covers the truly idle case.
        if idle {
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}
