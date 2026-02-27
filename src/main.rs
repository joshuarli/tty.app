mod config;
mod input;
mod parser;
mod pty;
mod renderer;
mod terminal;

use std::sync::Arc;
use std::time::Instant;

use objc2_app_kit::NSPasteboard;
use objc2_foundation::{ns_string, NSArray, NSString};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Fullscreen, Window, WindowAttributes, WindowId};

use crate::parser::Parser;
use crate::parser::perform::Perform;
use crate::parser::charset::translate_dec_special;
use crate::pty::Pty;
use crate::renderer::atlas::Atlas;
use crate::renderer::font::FontRasterizer;
use crate::renderer::metal::MetalRenderer;
use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;

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
            self.grid.cells[row_start + c as usize] =
                self.grid.cells[row_start + (c - n) as usize];
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
            self.grid.cells[row_start + c as usize] =
                self.grid.cells[row_start + (c + n) as usize];
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
                self.grid.clear_cols(row, col, (col + n).min(self.grid.cols));
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
    window: Option<Window>,
    renderer: Option<MetalRenderer>,
    rasterizer: Option<FontRasterizer>,
    atlas: Option<Atlas>,
    shared: Option<SharedState>,
    pty: Option<Arc<Pty>>,
    parser: Parser,
    modifiers: ModifiersState,
    cursor_visible: bool,
    last_blink: Instant,

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
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            rasterizer: None,
            atlas: None,
            shared: None,
            pty: None,
            parser: Parser::new(),
            modifiers: ModifiersState::empty(),
            cursor_visible: true,
            last_blink: Instant::now(),
            selection_start: None,
            selection_end: None,
            mouse_pressed: false,
            cursor_pos: (0.0, 0.0),
            prev_cursor_row: 0,
            prev_cursor_col: 0,
            sync_start: Instant::now(),
        }
    }

    fn process_pty_output(&mut self) {
        let pty = match &self.pty {
            Some(p) => p.clone(),
            None => return,
        };

        let mut buf = [0u8; 65536];

        loop {
            match pty.read(&mut buf) {
                Ok(0) => {
                    // EOF — shell exited
                    if let Some(state) = self.shared.as_mut() {
                        state.alive = false;
                    }
                    break;
                }
                Ok(n) => {
                    let state = self.shared.as_mut().unwrap();
                    let atlas = self.atlas.as_mut().unwrap();
                    let rasterizer = self.rasterizer.as_ref().unwrap();

                    let was_syncing = state.grid.mode.contains(TermMode::SYNC_OUTPUT);
                    let mut response_buf = std::mem::take(&mut state.response_buf);
                    {
                        let mut performer = TermPerformer {
                            grid: &mut state.grid,
                            scrollback: &mut state.scrollback,
                            atlas,
                            rasterizer,
                            response_buf: &mut response_buf,
                        };
                        self.parser.parse(&buf[..n], &mut performer);
                    }
                    state.response_buf = response_buf;

                    // Record when synchronized output begins
                    if !was_syncing && state.grid.mode.contains(TermMode::SYNC_OUTPUT) {
                        self.sync_start = Instant::now();
                    }
                    // Keep looping — only WouldBlock reliably indicates the
                    // kernel buffer is empty. A short read doesn't guarantee it.
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    if let Some(state) = &mut self.shared {
                        state.alive = false;
                    }
                    break;
                }
            }
        }

        // Handle responses
        if let Some(state) = &mut self.shared {
            let responses = std::mem::take(&mut state.response_buf);
            if !responses.is_empty() {
                self.handle_responses(&responses);
            }
        }
    }

    fn handle_responses(&self, data: &[u8]) {
        let mut pos = 0;
        while pos < data.len() {
            if data[pos..].starts_with(b"\x1B]title:") {
                let start = pos + 8;
                if let Some(end) = data[start..].iter().position(|&b| b == 0x07) {
                    if let Ok(title) = std::str::from_utf8(&data[start..start + end]) {
                        if let Some(w) = &self.window {
                            w.set_title(title);
                        }
                    }
                    pos = start + end + 1;
                } else {
                    break;
                }
            } else if data[pos..].starts_with(b"\x1B]52;set:") {
                let start = pos + 9;
                if let Some(end) = data[start..].iter().position(|&b| b == 0x07) {
                    let b64 = &data[start..start + end];
                    if let Ok(text_bytes) = base64_decode(b64) {
                        if let Ok(text) = String::from_utf8(text_bytes) {
                            set_clipboard(&text);
                        }
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
                if let Some(pty) = &self.pty {
                    // Find the end of this response (next marker or end)
                    let end = data[pos + 1..]
                        .windows(2)
                        .position(|w| w == b"\x1B]")
                        .map(|e| pos + 1 + e)
                        .unwrap_or(data.len());
                    let _ = pty.write(&data[pos..end]);
                    pos = end;
                } else {
                    break;
                }
            }
        }
    }

    fn render(&mut self) {
        // Drain any PTY data that arrived since new_events().
        // This is critical for apps that don't use synchronized updates (mode 2026)
        // like htop and tmux — it gives the kernel buffer maximum time to accumulate
        // the full screen update before we paint.
        self.process_pty_output();

        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => return,
        };
        let state = match &mut self.shared {
            Some(s) => s,
            None => return,
        };

        // Synchronized output (Mode 2026): defer rendering while the application
        // is mid-update. Dirty bits accumulate and get flushed on the first frame
        // after sync ends. Timeout after 100ms to prevent a stuck application from
        // freezing the display.
        if state.grid.mode.contains(TermMode::SYNC_OUTPUT) {
            let elapsed = Instant::now().duration_since(self.sync_start);
            if elapsed.as_millis() < 100 {
                return;
            }
            // Timeout — render anyway and clear the flag
            state.grid.mode.remove(TermMode::SYNC_OUTPUT);
        }

        // Cursor blink
        let now = Instant::now();
        if now.duration_since(self.last_blink).as_millis() >= config::CURSOR_BLINK_MS as u128 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = now;
            state.grid.mark_dirty(state.grid.cursor_row);
        }

        // Update cursor cell flag — clear old position first
        let cursor_row = state.grid.cursor_row;
        let cursor_col = state.grid.cursor_col;
        let prev_row = self.prev_cursor_row;
        let prev_col = self.prev_cursor_col;

        // Clear CURSOR flag from previous position
        if prev_row < state.grid.rows && prev_col < state.grid.cols {
            state.grid.cell_mut(prev_row, prev_col).flags.remove(CellFlags::CURSOR);
            state.grid.mark_dirty(prev_row);
        }

        // Set CURSOR flag at new position
        state.grid.cell_mut(cursor_row, cursor_col).flags.insert(CellFlags::CURSOR);
        state.grid.mark_dirty(cursor_row);

        self.prev_cursor_row = cursor_row;
        self.prev_cursor_col = cursor_col;

        renderer.render_frame(&mut state.grid, self.cursor_visible);
    }

    fn copy_selection(&self) {
        if let (Some(start), Some(end), Some(state)) =
            (self.selection_start, self.selection_end, &self.shared)
        {
            let mut text = String::new();
            let (start, end) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
                (start, end)
            } else {
                (end, start)
            };

            for row in start.1..=end.1 {
                let from_col = if row == start.1 { start.0 } else { 0 };
                let to_col = if row == end.1 { end.0 + 1 } else { state.grid.cols };

                for col in from_col..to_col {
                    let cell = state.grid.cell(row, col);
                    if cell.flags.contains(CellFlags::WIDE_CONT) {
                        continue;
                    }
                    if cell.codepoint >= 0x20 {
                        if let Some(ch) = char::from_u32(cell.codepoint as u32) {
                            text.push(ch);
                        }
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
            if let Some(pty) = &self.pty {
                let bracketed = self
                    .shared
                    .as_ref()
                    .map(|s| s.grid.mode.contains(TermMode::BRACKETED_PASTE))
                    .unwrap_or(false);

                if bracketed {
                    let _ = pty.write(b"\x1B[200~");
                    let _ = pty.write(text.as_bytes());
                    let _ = pty.write(b"\x1B[201~");
                } else {
                    let _ = pty.write(text.as_bytes());
                }
            }
        }
    }

    /// Convert pixel position to (col, row) cell coordinates.
    /// winit CursorMoved gives physical pixels on macOS.
    fn pixel_to_cell(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        let renderer = self.renderer.as_ref()?;
        let state = self.shared.as_ref()?;
        let scale = renderer.scale_factor;
        let padding = config::PADDING as f64 * scale;
        // x, y are already in physical pixels — don't multiply by scale
        let px = x - padding;
        let py = y - padding;
        if px < 0.0 || py < 0.0 {
            return Some((0, 0));
        }
        let col = (px / renderer.cell_width as f64) as u16;
        let row = (py / renderer.cell_height as f64) as u16;
        let col = col.min(state.grid.cols.saturating_sub(1));
        let row = row.min(state.grid.rows.saturating_sub(1));
        Some((col, row))
    }

    fn update_selection(&mut self) {
        if let (Some(start), Some(end), Some(state)) =
            (self.selection_start, self.selection_end, &mut self.shared)
        {
            // Normalize start/end
            let (start, end) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
                (start, end)
            } else {
                (end, start)
            };

            // Clear old selection
            let total = state.grid.cols as usize * state.grid.rows as usize;
            for i in 0..total {
                state.grid.cells[i].flags.remove(CellFlags::SELECTED);
            }

            // Set new selection
            for row in start.1..=end.1 {
                let from_col = if row == start.1 { start.0 } else { 0 };
                let to_col = if row == end.1 { end.0 } else { state.grid.cols - 1 };
                for col in from_col..=to_col {
                    state.grid.cell_mut(row, col).flags.insert(CellFlags::SELECTED);
                }
                state.grid.mark_dirty(row);
            }
        }
    }

    fn clear_selection(&mut self) {
        if self.selection_start.is_some() {
            self.selection_start = None;
            self.selection_end = None;
            if let Some(state) = &mut self.shared {
                let total = state.grid.cols as usize * state.grid.rows as usize;
                for i in 0..total {
                    state.grid.cells[i].flags.remove(CellFlags::SELECTED);
                }
                state.grid.mark_all_dirty();
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("etch")
            .with_fullscreen(Some(Fullscreen::Borderless(None)));
        let window = event_loop.create_window(attrs).expect("failed to create window");
        let scale = window.scale_factor();

        let rasterizer = FontRasterizer::new(config::FONT_FAMILY, config::FONT_SIZE, scale);
        let cell_width = rasterizer.metrics.cell_width;
        let cell_height = rasterizer.metrics.cell_height;

        let win_size = window.inner_size();
        let padding_px = (config::PADDING as f64 * scale) as u32;
        let cols = (win_size.width - padding_px * 2) / cell_width;
        let rows = (win_size.height - padding_px * 2) / cell_height;

        let mut renderer = MetalRenderer::new(&window, cols, rows, cell_width, cell_height);

        let mut atlas = Atlas::new(renderer.device(), cell_width, cell_height);
        atlas.preload_ascii(&rasterizer);
        renderer.atlas_texture = atlas.texture.clone();

        let grid = Grid::new(cols as u16, rows as u16);
        let scrollback = Scrollback::new(config::SCROLLBACK_LINES);

        let pty = Pty::spawn(cols as u16, rows as u16, cell_width as u16, cell_height as u16)
            .expect("failed to spawn PTY");
        let pty = Arc::new(pty);

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.rasterizer = Some(rasterizer);
        self.atlas = Some(atlas);
        self.shared = Some(SharedState {
            grid,
            scrollback,
            response_buf: Vec::new(),
            alive: true,
        });
        self.pty = Some(pty);
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: StartCause) {
        self.process_pty_output();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::RedrawRequested => {
                if let Some(state) = &self.shared {
                    if !state.alive {
                        event_loop.exit();
                        return;
                    }
                }
                self.render();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    return;
                }
                let scale = self.window.as_ref().unwrap().scale_factor();
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height, scale);
                    let cols = renderer.cols as u16;
                    let rows = renderer.rows as u16;
                    if let Some(state) = &mut self.shared {
                        state.grid.resize(cols, rows);
                    }
                    if let Some(pty) = &self.pty {
                        pty.resize(cols, rows, renderer.cell_width as u16, renderer.cell_height as u16);
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let (Some(renderer), Some(atlas)) = (&mut self.renderer, &mut self.atlas) {
                    let rasterizer =
                        FontRasterizer::new(config::FONT_FAMILY, config::FONT_SIZE, scale_factor);
                    renderer.cell_width = rasterizer.metrics.cell_width;
                    renderer.cell_height = rasterizer.metrics.cell_height;
                    *atlas = Atlas::new(renderer.device(), rasterizer.metrics.cell_width, rasterizer.metrics.cell_height);
                    atlas.preload_ascii(&rasterizer);
                    renderer.atlas_texture = atlas.texture.clone();
                    self.rasterizer = Some(rasterizer);
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // Cmd+C
                if self.modifiers.super_key() && event.logical_key == Key::Character("c".into()) {
                    self.copy_selection();
                    return;
                }
                // Cmd+V
                if self.modifiers.super_key() && event.logical_key == Key::Character("v".into()) {
                    self.paste_clipboard();
                    return;
                }

                let term_mode = self
                    .shared
                    .as_ref()
                    .map(|s| s.grid.mode)
                    .unwrap_or_default();

                if let Some(bytes) =
                    input::key_to_bytes(&event.logical_key, &self.modifiers, term_mode)
                {
                    if let Some(pty) = &self.pty {
                        let _ = pty.write(&bytes);
                    }
                    self.cursor_visible = true;
                    self.last_blink = Instant::now();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                if self.mouse_pressed {
                    if let Some(cell) = self.pixel_to_cell(position.x, position.y) {
                        self.selection_end = Some(cell);
                        self.update_selection();
                    }
                }
            }

            WindowEvent::MouseInput { state: btn_state, button, .. } => {
                if button == MouseButton::Left {
                    match btn_state {
                        ElementState::Pressed => {
                            self.clear_selection();
                            self.mouse_pressed = true;
                            if let Some(cell) = self.pixel_to_cell(self.cursor_pos.0, self.cursor_pos.1) {
                                self.selection_start = Some(cell);
                                self.selection_end = Some(cell);
                            }
                        }
                        ElementState::Released => {
                            self.mouse_pressed = false;
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
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
    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop failed");
}
