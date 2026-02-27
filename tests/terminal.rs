//! Integration tests for the tty terminal emulator.
//!
//! These tests drive raw byte sequences (including VT escape sequences) through the
//! parser and into the grid, then assert on the resulting terminal state. No GPU, no
//! Metal, no windowing — pure terminal semantics.

use tty::config;
use tty::parser::perform::Perform;
use tty::parser::Parser;
use tty::terminal::cell::{Cell, CellFlags};
use tty::terminal::grid::{Grid, TermMode};
use tty::terminal::scrollback::Scrollback;

// ── Test harness ────────────────────────────────────────────────────────────

/// A minimal terminal performer for testing. Mirrors the real TermPerformer's
/// behavior but without atlas/font/Metal dependencies. Atlas coordinates are
/// always (0, 0) since we only care about grid state in tests.
struct TestPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
    response_buf: &'a mut Vec<u8>,
}

impl<'a> Perform for TestPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.grid.write_char(b as char, 0, 0);
        }
    }

    fn print(&mut self, c: char) {
        self.grid.write_char(c, 0, 0);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {}
            0x08 => {
                if self.grid.cursor_col > 0 {
                    self.grid.cursor_col -= 1;
                    self.grid.cursor_pending_wrap = false;
                }
            }
            0x09 => {
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
                self.grid.cursor_col = 0;
                self.grid.cursor_pending_wrap = false;
            }
            0x0E => self.grid.active_charset = 1,
            0x0F => self.grid.active_charset = 0,
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
                if self.grid.cursor_row == self.grid.scroll_top {
                    self.grid.scroll_down(1);
                } else if self.grid.cursor_row > 0 {
                    self.grid.cursor_row -= 1;
                }
            }
            ([], b'c') => {
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
                let n = params.first().copied().unwrap_or(1).max(1);
                let row = self.grid.cursor_row;
                let col = self.grid.cursor_col;
                self.grid.clear_cols(row, col, (col + n).min(self.grid.cols));
            }
            ([], b'c') => {
                self.response_buf.extend_from_slice(b"\x1B[?62;c");
            }
            ([b'>'], b'c') => {
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

/// Test terminal: wraps parser + grid + scrollback for driving from raw bytes.
struct Term {
    parser: Parser,
    grid: Grid,
    scrollback: Scrollback,
    response_buf: Vec<u8>,
}

impl Term {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: Parser::new(),
            grid: Grid::new(cols, rows),
            scrollback: Scrollback::new(1000),
            response_buf: Vec::new(),
        }
    }

    /// Feed raw bytes (including escape sequences) into the terminal.
    fn feed(&mut self, data: &[u8]) {
        let mut response_buf = std::mem::take(&mut self.response_buf);
        {
            let mut performer = TestPerformer {
                grid: &mut self.grid,
                scrollback: &mut self.scrollback,
                response_buf: &mut response_buf,
            };
            self.parser.parse(data, &mut performer);
        }
        self.response_buf = response_buf;
    }

    /// Feed a string literal.
    fn feed_str(&mut self, s: &str) {
        self.feed(s.as_bytes());
    }

    /// Read the codepoints of a row as a string (trimming trailing spaces).
    fn row_text(&self, row: u16) -> String {
        let cols = self.grid.cols as usize;
        let start = row as usize * cols;
        let cells = &self.grid.cells[start..start + cols];
        let s: String = cells
            .iter()
            .map(|c| {
                if c.flags.contains(CellFlags::WIDE_CONT) {
                    return '\0'; // placeholder, will be filtered
                }
                char::from_u32(c.codepoint as u32).unwrap_or(' ')
            })
            .filter(|&c| c != '\0')
            .collect();
        s.trim_end().to_string()
    }

    /// Get a specific cell.
    fn cell(&self, row: u16, col: u16) -> &Cell {
        self.grid.cell(row, col)
    }

    /// Cursor position (row, col).
    fn cursor(&self) -> (u16, u16) {
        (self.grid.cursor_row, self.grid.cursor_col)
    }

    /// Take the response buffer contents.
    fn take_response(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.response_buf)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

// -- Basic text output --

#[test]
fn ascii_text_appears_in_grid() {
    let mut t = Term::new(80, 24);
    t.feed_str("Hello, world!");
    assert_eq!(t.row_text(0), "Hello, world!");
    assert_eq!(t.cursor(), (0, 13));
}

#[test]
fn multiple_lines_via_crlf() {
    let mut t = Term::new(40, 10);
    t.feed_str("line one\r\nline two\r\nline three");
    assert_eq!(t.row_text(0), "line one");
    assert_eq!(t.row_text(1), "line two");
    assert_eq!(t.row_text(2), "line three");
    assert_eq!(t.cursor(), (2, 10));
}

#[test]
fn lf_without_cr_does_not_reset_column() {
    let mut t = Term::new(40, 10);
    t.feed_str("abcde\nfghij");
    // LF moves down, but column stays at 5; next chars write from col 5
    assert_eq!(t.row_text(0), "abcde");
    assert_eq!(t.row_text(1), "     fghij");
    assert_eq!(t.cursor(), (1, 10));
}

#[test]
fn backspace_moves_cursor_left() {
    let mut t = Term::new(20, 5);
    t.feed_str("abc\x08X");
    // BS moves left from col 3 to col 2, then 'X' overwrites 'c'
    assert_eq!(t.row_text(0), "abX");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn backspace_at_column_zero_is_noop() {
    let mut t = Term::new(20, 5);
    t.feed_str("\x08\x08\x08A");
    assert_eq!(t.row_text(0), "A");
    assert_eq!(t.cursor(), (0, 1));
}

// -- Autowrap --

#[test]
fn autowrap_wraps_at_end_of_line() {
    let mut t = Term::new(5, 3);
    // Grid is 5 columns wide. Writing "abcde" fills the row,
    // then "f" should wrap to the next line (with pending wrap).
    t.feed_str("abcdef");
    assert_eq!(t.row_text(0), "abcde");
    assert_eq!(t.row_text(1), "f");
    assert_eq!(t.cursor(), (1, 1));
}

#[test]
fn no_wrap_without_autowrap_mode() {
    let mut t = Term::new(5, 3);
    // Disable autowrap
    t.feed(b"\x1B[?7l");
    t.feed_str("abcdefgh");
    // Without autowrap, chars overwrite the last column
    assert_eq!(t.row_text(0), "abcdh");
    assert_eq!(t.cursor(), (0, 4));
}

#[test]
fn pending_wrap_deferred_until_next_char() {
    let mut t = Term::new(5, 3);
    t.feed_str("abcde");
    // Cursor is at col 4 with pending wrap — hasn't moved to row 1 yet
    assert_eq!(t.cursor(), (0, 4));
    assert!(t.grid.cursor_pending_wrap);
    // Writing another char triggers the wrap
    t.feed_str("f");
    assert_eq!(t.cursor(), (1, 1));
    assert!(!t.grid.cursor_pending_wrap);
}

// -- Cursor movement --

#[test]
fn cup_cursor_position() {
    let mut t = Term::new(80, 24);
    // ESC[5;10H — move to row 5, col 10 (1-indexed)
    t.feed(b"\x1B[5;10H");
    assert_eq!(t.cursor(), (4, 9)); // 0-indexed
}

#[test]
fn cup_defaults_to_home() {
    let mut t = Term::new(80, 24);
    t.feed_str("something");
    t.feed(b"\x1B[H");
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn cursor_up_down_forward_backward() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1B[10;20H"); // row 10, col 20
    t.feed(b"\x1B[3A"); // up 3
    assert_eq!(t.cursor(), (6, 19));
    t.feed(b"\x1B[5B"); // down 5
    assert_eq!(t.cursor(), (11, 19));
    t.feed(b"\x1B[10C"); // forward 10
    assert_eq!(t.cursor(), (11, 29));
    t.feed(b"\x1B[7D"); // backward 7
    assert_eq!(t.cursor(), (11, 22));
}

#[test]
fn cursor_movement_clamps_to_grid_bounds() {
    let mut t = Term::new(10, 5);
    t.feed(b"\x1B[1;1H"); // home
    t.feed(b"\x1B[999A"); // up 999 — should clamp to row 0
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1B[999B"); // down 999 — should clamp to last row
    assert_eq!(t.cursor(), (4, 0));
    t.feed(b"\x1B[999C"); // forward 999 — should clamp to last col
    assert_eq!(t.cursor(), (4, 9));
    t.feed(b"\x1B[999D"); // backward 999 — should clamp to col 0
    assert_eq!(t.cursor(), (4, 0));
}

#[test]
fn cursor_horizontal_absolute() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1B[5;1H"); // row 5, col 1
    t.feed(b"\x1B[15G"); // CHA — move to column 15
    assert_eq!(t.cursor(), (4, 14));
}

// -- Erase operations --

#[test]
fn erase_in_display_below() {
    let mut t = Term::new(10, 5);
    for i in 0..5 {
        t.feed(format!("\x1B[{};1Hrow{}", i + 1, i).as_bytes());
    }
    // Move to row 2, col 3 and erase below (ED 0)
    t.feed(b"\x1B[3;4H\x1B[J");
    assert_eq!(t.row_text(0), "row0");
    assert_eq!(t.row_text(1), "row1");
    assert_eq!(t.row_text(2), "row"); // col 3 onwards erased
    assert_eq!(t.row_text(3), "");
    assert_eq!(t.row_text(4), "");
}

#[test]
fn erase_in_display_entire_screen() {
    let mut t = Term::new(10, 3);
    t.feed_str("AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC");
    t.feed(b"\x1B[2J"); // ED 2 — erase all
    assert_eq!(t.row_text(0), "");
    assert_eq!(t.row_text(1), "");
    assert_eq!(t.row_text(2), "");
}

#[test]
fn erase_in_line() {
    let mut t = Term::new(20, 3);
    t.feed_str("Hello, world!");
    // Move to col 5 and erase to end of line
    t.feed(b"\x1B[1;6H\x1B[K");
    assert_eq!(t.row_text(0), "Hello");
    // Erase from start to cursor (inclusive). Cursor at col 3 erases cols 0-3.
    t.feed(b"\x1B[1;4H\x1B[1K");
    assert_eq!(t.row_text(0), "    o");
}

#[test]
fn erase_characters() {
    let mut t = Term::new(20, 3);
    t.feed_str("ABCDEFGHIJ");
    t.feed(b"\x1B[1;3H"); // col 3 (0-indexed: col 2)
    t.feed(b"\x1B[4X"); // ECH 4 — erase 4 chars starting at cursor
    assert_eq!(t.row_text(0), "AB    GHIJ");
}

// -- Scrolling --

#[test]
fn scroll_up_moves_content_and_evicts_to_scrollback() {
    let mut t = Term::new(10, 3);
    t.feed_str("AAA\r\nBBB\r\nCCC");
    t.feed(b"\x1B[1S"); // SU — scroll up 1
    assert_eq!(t.row_text(0), "BBB");
    assert_eq!(t.row_text(1), "CCC");
    assert_eq!(t.row_text(2), "");
    assert_eq!(t.scrollback.len(), 1);
}

#[test]
fn scroll_down_inserts_blank_lines_at_top() {
    let mut t = Term::new(10, 3);
    t.feed_str("AAA\r\nBBB\r\nCCC");
    t.feed(b"\x1B[1T"); // SD — scroll down 1
    assert_eq!(t.row_text(0), "");
    assert_eq!(t.row_text(1), "AAA");
    assert_eq!(t.row_text(2), "BBB");
    // CCC scrolled off the bottom
}

#[test]
fn linefeed_at_bottom_scrolls() {
    let mut t = Term::new(10, 3);
    t.feed_str("line1\r\nline2\r\nline3\r\nline4");
    // After 4 lines in a 3-row grid, "line1" should have scrolled off
    assert_eq!(t.row_text(0), "line2");
    assert_eq!(t.row_text(1), "line3");
    assert_eq!(t.row_text(2), "line4");
    assert_eq!(t.scrollback.len(), 1);
}

#[test]
fn scroll_region_confines_scrolling() {
    let mut t = Term::new(10, 5);
    t.feed_str("row0\r\nrow1\r\nrow2\r\nrow3\r\nrow4");
    // Set scroll region to rows 2-4 (1-indexed)
    t.feed(b"\x1B[2;4r");
    // Scroll up within region
    t.feed(b"\x1B[1S");
    // Row 0 untouched, rows 1-3 scrolled, row 4 untouched
    assert_eq!(t.row_text(0), "row0");
    assert_eq!(t.row_text(1), "row2");
    assert_eq!(t.row_text(2), "row3");
    assert_eq!(t.row_text(3), "");
    assert_eq!(t.row_text(4), "row4");
}

// -- Insert/delete lines and characters --

#[test]
fn insert_lines() {
    let mut t = Term::new(10, 5);
    t.feed_str("AAA\r\nBBB\r\nCCC\r\nDDD\r\nEEE");
    t.feed(b"\x1B[2;1H"); // row 2
    t.feed(b"\x1B[2L"); // insert 2 lines
    assert_eq!(t.row_text(0), "AAA");
    assert_eq!(t.row_text(1), "");
    assert_eq!(t.row_text(2), "");
    assert_eq!(t.row_text(3), "BBB");
    assert_eq!(t.row_text(4), "CCC");
    // DDD and EEE scrolled off
}

#[test]
fn delete_lines() {
    let mut t = Term::new(10, 5);
    t.feed_str("AAA\r\nBBB\r\nCCC\r\nDDD\r\nEEE");
    t.feed(b"\x1B[2;1H"); // row 2
    t.feed(b"\x1B[2M"); // delete 2 lines
    assert_eq!(t.row_text(0), "AAA");
    assert_eq!(t.row_text(1), "DDD");
    assert_eq!(t.row_text(2), "EEE");
    assert_eq!(t.row_text(3), "");
    assert_eq!(t.row_text(4), "");
}

#[test]
fn insert_characters() {
    let mut t = Term::new(10, 3);
    t.feed_str("ABCDEFGHIJ");
    t.feed(b"\x1B[1;3H"); // col 3 (0-indexed: col 2)
    t.feed(b"\x1B[3@"); // ICH 3 — insert 3 blanks
    assert_eq!(t.row_text(0), "AB   CDEFG");
}

#[test]
fn delete_characters() {
    let mut t = Term::new(10, 3);
    t.feed_str("ABCDEFGHIJ");
    t.feed(b"\x1B[1;3H"); // col 3 (0-indexed: col 2)
    t.feed(b"\x1B[3P"); // DCH 3 — delete 3 chars
    assert_eq!(t.row_text(0), "ABFGHIJ");
}

// -- SGR (Select Graphic Rendition) --

#[test]
fn sgr_bold_sets_flag() {
    let mut t = Term::new(20, 3);
    t.feed(b"\x1B[1mBold\x1B[0m");
    assert!(t.cell(0, 0).flags.contains(CellFlags::BOLD));
    assert!(t.cell(0, 3).flags.contains(CellFlags::BOLD));
}

#[test]
fn sgr_reset_clears_all_attributes() {
    let mut t = Term::new(20, 3);
    t.feed(b"\x1B[1;3;4;7mX\x1B[0mY");
    let x = t.cell(0, 0);
    assert!(x.flags.contains(CellFlags::BOLD));
    assert!(x.flags.contains(CellFlags::ITALIC));
    assert!(x.flags.contains(CellFlags::UNDERLINE));
    assert!(x.flags.contains(CellFlags::INVERSE));
    let y = t.cell(0, 1);
    assert!(y.flags.is_empty());
    assert_eq!(y.fg_index, 7);
    assert_eq!(y.bg_index, 0);
}

#[test]
fn sgr_foreground_colors() {
    let mut t = Term::new(20, 3);
    // ANSI red (31)
    t.feed(b"\x1B[31mR\x1B[0m");
    assert_eq!(t.cell(0, 0).fg_index, 1);
    // 256-color (38;5;200)
    t.feed(b"\x1B[38;5;200mX\x1B[0m");
    assert_eq!(t.cell(0, 1).fg_index, 200);
    // 24-bit RGB (38;2;255;128;0)
    t.feed(b"\x1B[38;2;255;128;0mY\x1B[0m");
    assert_eq!(t.cell(0, 2).fg_index, 0xFF);
    assert_eq!(t.cell(0, 2).fg_rgb, 0x00FF8000);
}

#[test]
fn sgr_background_colors() {
    let mut t = Term::new(20, 3);
    // ANSI blue bg (44)
    t.feed(b"\x1B[44mB\x1B[0m");
    assert_eq!(t.cell(0, 0).bg_index, 4);
    // 256-color bg (48;5;100)
    t.feed(b"\x1B[48;5;100mX\x1B[0m");
    assert_eq!(t.cell(0, 1).bg_index, 100);
}

#[test]
fn sgr_bright_colors() {
    let mut t = Term::new(20, 3);
    // Bright red fg (91)
    t.feed(b"\x1B[91mX\x1B[0m");
    assert_eq!(t.cell(0, 0).fg_index, 9); // 91 - 90 + 8 = 9
    // Bright blue bg (104)
    t.feed(b"\x1B[104mY\x1B[0m");
    assert_eq!(t.cell(0, 1).bg_index, 12); // 104 - 100 + 8 = 12
}

// -- Alt screen --

#[test]
fn alt_screen_preserves_main_content() {
    let mut t = Term::new(10, 3);
    t.feed_str("main");
    assert_eq!(t.row_text(0), "main");
    // Enter alt screen
    t.feed(b"\x1B[?1049h");
    assert!(t.grid.mode.contains(TermMode::ALT_SCREEN));
    assert_eq!(t.row_text(0), ""); // alt screen is blank
    t.feed_str("alt");
    assert_eq!(t.row_text(0), "alt");
    // Exit alt screen — main content restored
    t.feed(b"\x1B[?1049l");
    assert!(!t.grid.mode.contains(TermMode::ALT_SCREEN));
    assert_eq!(t.row_text(0), "main");
}

// -- Mode handling --

#[test]
fn decset_and_decrst_toggle_modes() {
    let mut t = Term::new(10, 3);
    // Enable bracketed paste
    t.feed(b"\x1B[?2004h");
    assert!(t.grid.mode.contains(TermMode::BRACKETED_PASTE));
    // Disable bracketed paste
    t.feed(b"\x1B[?2004l");
    assert!(!t.grid.mode.contains(TermMode::BRACKETED_PASTE));
}

#[test]
fn sync_output_mode_flag() {
    let mut t = Term::new(10, 3);
    t.feed(b"\x1B[?2026h");
    assert!(t.grid.mode.contains(TermMode::SYNC_OUTPUT));
    t.feed(b"\x1B[?2026l");
    assert!(!t.grid.mode.contains(TermMode::SYNC_OUTPUT));
}

// -- Tab stops --

#[test]
fn default_tab_stops_every_8_columns() {
    let mut t = Term::new(40, 3);
    t.feed(b"\t"); // tab from col 0 → col 8
    assert_eq!(t.cursor(), (0, 8));
    t.feed(b"\t"); // col 8 → col 16
    assert_eq!(t.cursor(), (0, 16));
}

#[test]
fn tab_clear_all_and_custom_stops() {
    let mut t = Term::new(40, 3);
    // Clear all tab stops
    t.feed(b"\x1B[3g");
    // Set tab stop at col 5
    t.feed(b"\x1B[1;6H\x1BH"); // move to col 5 (1-indexed: 6), set tab
    // Move home and tab
    t.feed(b"\x1B[1;1H\t");
    assert_eq!(t.cursor(), (0, 5));
}

// -- Device Status Report --

#[test]
fn dsr_cursor_position_response() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1B[10;20H"); // move to row 10, col 20
    t.feed(b"\x1B[6n"); // DSR — query cursor position
    let response = t.take_response();
    assert_eq!(response, b"\x1B[10;20R");
}

#[test]
fn dsr_status_response() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1B[5n"); // DSR — device status
    let response = t.take_response();
    assert_eq!(response, b"\x1B[0n"); // "OK"
}

// -- Save/restore cursor --

#[test]
fn save_and_restore_cursor() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1B[5;10H"); // row 5, col 10
    t.feed(b"\x1B7"); // save cursor
    t.feed(b"\x1B[1;1H"); // home
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1B8"); // restore cursor
    assert_eq!(t.cursor(), (4, 9));
}

// -- Complex scenarios --

#[test]
fn tmux_style_full_screen_repaint() {
    // Simulates what tmux does: enable sync, repaint, disable sync.
    let mut t = Term::new(20, 5);
    // Initial content
    t.feed_str("old content line 1\r\nold content line 2");
    // tmux repaints
    t.feed(b"\x1B[?2026h"); // begin sync
    t.feed(b"\x1B[H"); // home
    t.feed(b"\x1B[2J"); // clear screen
    t.feed_str("new line 1\r\nnew line 2\r\nnew line 3");
    t.feed(b"\x1B[?2026l"); // end sync
    assert_eq!(t.row_text(0), "new line 1");
    assert_eq!(t.row_text(1), "new line 2");
    assert_eq!(t.row_text(2), "new line 3");
    assert_eq!(t.row_text(3), "");
    assert!(!t.grid.mode.contains(TermMode::SYNC_OUTPUT));
}

#[test]
fn vi_style_alt_screen_with_scroll_region() {
    let mut t = Term::new(20, 5);
    t.feed_str("shell prompt here");
    // vi enters: save cursor, enter alt screen
    t.feed(b"\x1B[?1049h");
    assert_eq!(t.row_text(0), ""); // alt screen blank
    // Set scroll region (rows 1-4, leaving row 5 as status bar)
    t.feed(b"\x1B[1;4r");
    assert_eq!(t.grid.scroll_top, 0);
    assert_eq!(t.grid.scroll_bottom, 3);
    // Write content
    t.feed(b"\x1B[1;1H");
    t.feed_str("~\r\n~\r\n~\r\n~");
    t.feed(b"\x1B[5;1H");
    t.feed_str("-- INSERT --");
    assert_eq!(t.row_text(0), "~");
    assert_eq!(t.row_text(4), "-- INSERT --");
    // vi exits: leave alt screen, restore cursor
    t.feed(b"\x1B[?1049l");
    assert_eq!(t.row_text(0), "shell prompt here");
}

#[test]
fn rapid_cursor_positioning_and_overwrites() {
    // Simulates a program writing chars at random positions (like htop/top)
    let mut t = Term::new(10, 3);
    // Fill row 0 with dots
    t.feed_str("..........");
    // Overwrite specific positions
    t.feed(b"\x1B[1;1H"); // home
    t.feed_str("A");
    t.feed(b"\x1B[1;5H");
    t.feed_str("B");
    t.feed(b"\x1B[1;10H");
    t.feed_str("C");
    assert_eq!(t.row_text(0), "A...B....C");
}

#[test]
fn erase_with_colored_background() {
    // ED/EL should use the current SGR background color per VT spec
    let mut t = Term::new(10, 3);
    t.feed_str("XXXXXXXXXX");
    // Set bg to blue (44), erase from cursor to end
    t.feed(b"\x1B[1;5H\x1B[44m\x1B[K");
    // Erased cells should have bg_index = 4
    assert_eq!(t.cell(0, 4).bg_index, 4);
    assert_eq!(t.cell(0, 4).codepoint, b' ' as u16);
    // Non-erased cells keep original bg
    assert_eq!(t.cell(0, 0).bg_index, 0);
}

#[test]
fn reverse_index_at_top_scrolls_down() {
    let mut t = Term::new(10, 3);
    t.feed_str("AAA\r\nBBB\r\nCCC");
    t.feed(b"\x1B[1;1H"); // home
    t.feed(b"\x1BM"); // RI (reverse index)
    assert_eq!(t.row_text(0), "");
    assert_eq!(t.row_text(1), "AAA");
    assert_eq!(t.row_text(2), "BBB");
}

#[test]
fn parser_handles_split_esc_sequence() {
    // ESC dispatch sequences (non-CSI) work across parse() boundaries
    // because the state machine handles them natively.
    let mut t = Term::new(20, 10);
    t.feed(b"\x1B[5;10H"); // move cursor first (single chunk, uses fast path)
    assert_eq!(t.cursor(), (4, 9));
    t.feed(b"\x1B"); // start of ESC sequence
    t.feed(b"7"); // DECSC (save cursor) — dispatched by state machine
    t.feed(b"\x1B[1;1H"); // move home
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1B"); // start of ESC sequence
    t.feed(b"8"); // DECRC (restore cursor) — dispatched by state machine
    assert_eq!(t.cursor(), (4, 9));
}

#[test]
fn parser_handles_multiple_sequences_in_one_chunk() {
    let mut t = Term::new(20, 3);
    // Multiple CSI sequences packed together
    t.feed(b"AB\x1B[1;1H\x1B[KCD\x1B[1;3HEF");
    // AB written, then home + erase line + CD + move to col 3 + EF
    assert_eq!(t.row_text(0), "CDEF");
}

#[test]
fn grid_resize_preserves_content() {
    let mut t = Term::new(10, 5);
    t.feed_str("Hello\r\nWorld");
    t.grid.resize(20, 3);
    assert_eq!(t.grid.cols, 20);
    assert_eq!(t.grid.rows, 3);
    // Content preserved in the smaller of old/new dimensions
    let row0: String = (0..5).map(|c| {
        char::from_u32(t.grid.cell(0, c).codepoint as u32).unwrap_or(' ')
    }).collect();
    assert_eq!(row0, "Hello");
}

#[test]
fn osc_window_title() {
    let mut t = Term::new(20, 3);
    // OSC 2 ; title BEL
    t.feed(b"\x1B]2;My Terminal\x07");
    let response = t.take_response();
    assert_eq!(response, b"\x1B]title:My Terminal\x07");
}

#[test]
fn ris_full_reset() {
    let mut t = Term::new(10, 3);
    // Set some state
    t.feed(b"\x1B[1;31m"); // bold red
    t.feed_str("text");
    t.feed(b"\x1B[?2004h"); // bracketed paste
    // Full reset
    t.feed(b"\x1Bc");
    assert_eq!(t.cursor(), (0, 0));
    assert_eq!(t.row_text(0), "");
    assert!(!t.grid.mode.contains(TermMode::BRACKETED_PASTE));
    assert!(t.grid.mode.contains(TermMode::AUTO_WRAP));
    assert_eq!(t.grid.attr.fg_index, 7);
    assert_eq!(t.grid.attr.bg_index, 0);
    assert!(t.grid.attr.flags.is_empty());
}
