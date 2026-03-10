//! Fuzz the VT parser with arbitrary byte streams.
//!
//! Exercises the full pipeline: SIMD scanner → CSI fast path → state machine →
//! UTF-8 assembler → Grid mutations. The invariant is simple: no panics, no
//! out-of-bounds access, no matter what bytes are fed.

#![no_main]

use libfuzzer_sys::fuzz_target;

use tty::parser::Parser;
use tty::parser::perform::Perform;
use tty::terminal::cell::{Cell, CellFlags};
use tty::terminal::grid::{Grid, TermMode};
use tty::terminal::scrollback::Scrollback;

/// Minimal performer that exercises real Grid mutations without GPU dependencies.
struct FuzzPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
}

impl<'a> Perform for FuzzPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        self.grid.write_ascii_run(bytes);
    }

    fn print(&mut self, c: char) {
        self.grid.write_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
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
                    self.grid.scroll_up_into(1, Some(self.scrollback));
                } else if self.grid.cursor_row < self.grid.rows - 1 {
                    self.grid.cursor_row += 1;
                }
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
        let top = if self.grid.cursor_row >= self.grid.scroll_top
            && self.grid.cursor_row <= self.grid.scroll_bottom
        {
            self.grid.scroll_top
        } else {
            0
        };
        self.grid.cursor_row = self.grid.cursor_row.saturating_sub(n).max(top);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_down(&mut self, n: u16) {
        let bottom = if self.grid.cursor_row >= self.grid.scroll_top
            && self.grid.cursor_row <= self.grid.scroll_bottom
        {
            self.grid.scroll_bottom
        } else {
            self.grid.rows - 1
        };
        self.grid.cursor_row = self.grid.cursor_row.saturating_add(n).min(bottom);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_forward(&mut self, n: u16) {
        self.grid.cursor_col = self.grid.cursor_col.saturating_add(n).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_backward(&mut self, n: u16) {
        self.grid.cursor_col = self.grid.cursor_col.saturating_sub(n);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_position(&mut self, row: u16, col: u16) {
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            let top = self.grid.scroll_top;
            let bottom = self.grid.scroll_bottom;
            self.grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
        } else {
            self.grid.cursor_row = row.saturating_sub(1).min(self.grid.rows - 1);
        }
        self.grid.cursor_col = col.saturating_sub(1).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.cursor_col = col.saturating_sub(1).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            let top = self.grid.scroll_top;
            let bottom = self.grid.scroll_bottom;
            self.grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
        } else {
            self.grid.cursor_row = row.saturating_sub(1).min(self.grid.rows - 1);
        }
        self.grid.cursor_pending_wrap = false;
    }

    fn erase_in_display(&mut self, mode: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        match mode {
            0 => {
                self.grid.clear_cols(row, col, self.grid.cols);
                if row + 1 < self.grid.rows {
                    self.grid.clear_rows(row + 1, self.grid.rows);
                }
            }
            1 => {
                self.grid.clear_rows(0, row);
                self.grid.clear_cols(row, 0, col + 1);
            }
            2 | 3 => self.grid.clear_rows(0, self.grid.rows),
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
        self.grid.scroll_up_into(n, Some(self.scrollback));
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

    fn insert_chars(&mut self, _n: u16) {}
    fn delete_chars(&mut self, _n: u16) {}
    fn erase_chars(&mut self, _n: u16) {}

    fn sgr(&mut self, params: &[u16]) {
        for &p in params {
            match p {
                0 => self.grid.attr = Cell::default(),
                1 => self.grid.attr.flags.insert(CellFlags::BOLD),
                3 => self.grid.attr.flags.insert(CellFlags::ITALIC),
                4 => self.grid.attr.flags.insert(CellFlags::UNDERLINE),
                7 => self.grid.attr.flags.insert(CellFlags::INVERSE),
                9 => self.grid.attr.flags.insert(CellFlags::STRIKE),
                _ => {}
            }
        }
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                let flag = match p {
                    1 => TermMode::CURSOR_KEYS,
                    6 => TermMode::ORIGIN_MODE,
                    7 => TermMode::AUTO_WRAP,
                    25 => TermMode::CURSOR_VISIBLE,
                    1049 => {
                        self.grid.enter_alt_screen();
                        continue;
                    }
                    _ => continue,
                };
                self.grid.mode.insert(flag);
            }
        }
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                let flag = match p {
                    1 => TermMode::CURSOR_KEYS,
                    6 => TermMode::ORIGIN_MODE,
                    7 => TermMode::AUTO_WRAP,
                    25 => TermMode::CURSOR_VISIBLE,
                    1049 => {
                        self.grid.exit_alt_screen();
                        continue;
                    }
                    _ => continue,
                };
                self.grid.mode.remove(flag);
            }
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = bottom.saturating_sub(1).min(self.grid.rows - 1);
        if top < bottom {
            self.grid.scroll_top = top;
            self.grid.scroll_bottom = bottom;
        }
        self.cursor_position(1, 1);
    }

    fn tab_clear(&mut self, _mode: u16) {}
    fn set_tab_stop(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]]) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _byte: u8) {}
    fn csi_dispatch(&mut self, _params: &[u16], _intermediates: &[u8], _ignore: bool, _byte: u8) {}
    fn save_cursor(&mut self) { self.grid.save_cursor(); }
    fn restore_cursor(&mut self) { self.grid.restore_cursor(); }
    fn device_status_report(&mut self, _mode: u16) {}
    fn repeat_char(&mut self, n: u16) {
        let c = self.grid.last_char;
        for _ in 0..n { self.grid.write_char(c); }
    }
    fn sgr_colon(&mut self, _raw: &[u8]) {}
}

fuzz_target!(|data: &[u8]| {
    let mut grid = Grid::new(80, 24);
    let mut scrollback = Scrollback::new(100);
    let mut parser = Parser::new();

    let mut performer = FuzzPerformer {
        grid: &mut grid,
        scrollback: &mut scrollback,
    };
    parser.parse(data, &mut performer);

    // Verify invariants after every fuzz input
    assert!(grid.cursor_row < grid.rows);
    assert!(grid.cursor_col < grid.cols);
    assert!(grid.scroll_top <= grid.scroll_bottom);
    assert!(grid.scroll_bottom < grid.rows);
    assert_eq!(grid.cells.len(), grid.rows as usize * grid.cols as usize);
});
