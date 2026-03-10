//! Property-based tests for Grid invariants.
//!
//! These tests verify structural invariants that must hold regardless of the
//! sequence of operations applied to the grid.

use proptest::prelude::*;

use tty::parser::Parser;
use tty::parser::perform::Perform;
use tty::terminal::cell::{Cell, CellFlags};
use tty::terminal::grid::{Grid, TermMode};
use tty::terminal::scrollback::Scrollback;

// ── Minimal performer (same as integration tests, no atlas) ──────────────────

struct TestPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
}

impl<'a> Perform for TestPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        self.grid.write_ascii_run(bytes);
        if let Some(&last) = bytes.last() {
            self.grid.last_char = last as char;
        }
    }

    fn print(&mut self, c: char) {
        self.grid.write_char(c);
        self.grid.last_char = c;
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
                    self.grid.scroll_up_into(1, Some(self.scrollback));
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
        let row = self.grid.cursor_row;
        let top = if row >= self.grid.scroll_top && row <= self.grid.scroll_bottom {
            self.grid.scroll_top
        } else {
            0
        };
        self.grid.cursor_row = row.saturating_sub(n).max(top);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_down(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let bottom = if row >= self.grid.scroll_top && row <= self.grid.scroll_bottom {
            self.grid.scroll_bottom
        } else {
            self.grid.rows - 1
        };
        self.grid.cursor_row = row.saturating_add(n).min(bottom);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_forward(&mut self, n: u16) {
        self.grid.cursor_col = self
            .grid
            .cursor_col
            .saturating_add(n)
            .min(self.grid.cols - 1);
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
            self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
        }
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            let top = self.grid.scroll_top;
            let bottom = self.grid.scroll_bottom;
            self.grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
        } else {
            self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
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
                _ => {}
            }
        }
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    7 => self.grid.mode.insert(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.insert(TermMode::CURSOR_VISIBLE),
                    _ => {}
                }
            }
        }
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    7 => self.grid.mode.remove(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.remove(TermMode::CURSOR_VISIBLE),
                    _ => {}
                }
            }
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = (bottom.saturating_sub(1)).min(self.grid.rows - 1);
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
    fn save_cursor(&mut self) {
        self.grid.save_cursor();
    }
    fn restore_cursor(&mut self) {
        self.grid.restore_cursor();
    }
    fn device_status_report(&mut self, _mode: u16) {}
    fn repeat_char(&mut self, n: u16) {
        let c = self.grid.last_char;
        for _ in 0..n {
            self.grid.write_char(c);
        }
    }
    fn sgr_colon(&mut self, _raw: &[u8]) {}
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn parse_bytes(grid: &mut Grid, scrollback: &mut Scrollback, data: &[u8]) {
    let mut parser = Parser::new();
    let mut performer = TestPerformer {
        grid,
        scrollback,
    };
    parser.parse(data, &mut performer);
}

/// Assert all structural invariants of the grid.
fn assert_grid_invariants(grid: &Grid) {
    let rows = grid.rows;
    let cols = grid.cols;

    // Cells vec has the right length
    assert_eq!(
        grid.cells.len(),
        rows as usize * cols as usize,
        "cells.len() must equal rows * cols"
    );

    // Dirty bitvec has the right length
    assert_eq!(
        grid.dirty.len(),
        rows as usize,
        "dirty.len() must equal rows"
    );

    // Cursor is within bounds
    assert!(
        grid.cursor_row < rows,
        "cursor_row ({}) must be < rows ({})",
        grid.cursor_row,
        rows
    );
    assert!(
        grid.cursor_col < cols,
        "cursor_col ({}) must be < cols ({})",
        grid.cursor_col,
        cols
    );

    // Scroll region is valid
    assert!(
        grid.scroll_top <= grid.scroll_bottom,
        "scroll_top ({}) must be <= scroll_bottom ({})",
        grid.scroll_top,
        grid.scroll_bottom
    );
    assert!(
        grid.scroll_bottom < rows,
        "scroll_bottom ({}) must be < rows ({})",
        grid.scroll_bottom,
        rows
    );

    // Tab stops bitvec has the right length
    assert_eq!(
        grid.tab_stops.len(),
        cols as usize,
        "tab_stops.len() must equal cols"
    );

    // Every WIDE cell must be followed by WIDE_CONT (within the same row)
    for row in 0..rows {
        for col in 0..cols {
            let cell = grid.cell(row, col);
            if cell.flags.contains(CellFlags::WIDE) {
                assert!(
                    col + 1 < cols,
                    "WIDE cell at ({},{}) is in the last column with no room for continuation",
                    row,
                    col
                );
                let next = grid.cell(row, col + 1);
                assert!(
                    next.flags.contains(CellFlags::WIDE_CONT),
                    "WIDE cell at ({},{}) not followed by WIDE_CONT",
                    row,
                    col
                );
            }
        }
    }
}

// ── Property tests ───────────────────────────────────────────────────────────

proptest! {
    /// Arbitrary byte sequences through the parser must never violate grid invariants.
    #[test]
    fn arbitrary_bytes_preserve_grid_invariants(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut grid = Grid::new(80, 24);
        let mut scrollback = Scrollback::new(100);
        parse_bytes(&mut grid, &mut scrollback, &data);
        assert_grid_invariants(&grid);
    }

    /// Grid invariants hold after feeding data in multiple chunks of random sizes.
    #[test]
    fn chunked_input_preserves_invariants(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        chunk_sizes in proptest::collection::vec(1u16..256, 1..32),
    ) {
        let mut grid = Grid::new(80, 24);
        let mut scrollback = Scrollback::new(100);
        let mut parser = Parser::new();

        let mut pos = 0;
        for &size in &chunk_sizes {
            if pos >= data.len() { break; }
            let end = (pos + size as usize).min(data.len());
            let mut performer = TestPerformer {
                grid: &mut grid,
                scrollback: &mut scrollback,
            };
            parser.parse(&data[pos..end], &mut performer);
            pos = end;
        }

        // Feed any remaining data
        if pos < data.len() {
            let mut performer = TestPerformer {
                grid: &mut grid,
                scrollback: &mut scrollback,
            };
            parser.parse(&data[pos..], &mut performer);
        }

        assert_grid_invariants(&grid);
    }

    /// Resize after arbitrary input must maintain invariants.
    #[test]
    fn resize_after_input_preserves_invariants(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        new_cols in 1u16..200,
        new_rows in 1u16..100,
    ) {
        let mut grid = Grid::new(80, 24);
        let mut scrollback = Scrollback::new(100);
        parse_bytes(&mut grid, &mut scrollback, &data);
        grid.resize(new_cols, new_rows);
        assert_grid_invariants(&grid);
    }

    /// Scroll region operations must keep cursor and region in bounds.
    #[test]
    fn scroll_region_invariants(
        top in 1u16..24,
        bottom in 1u16..24,
        scroll_n in 1u16..10,
    ) {
        let mut grid = Grid::new(80, 24);
        let mut scrollback = Scrollback::new(100);

        // Set scroll region (1-indexed like VT)
        let actual_top = top.min(bottom);
        let actual_bottom = top.max(bottom);
        let cmd = format!("\x1B[{};{}r", actual_top, actual_bottom);
        parse_bytes(&mut grid, &mut scrollback, cmd.as_bytes());

        // Scroll up and down
        let cmd = format!("\x1B[{}S\x1B[{}T", scroll_n, scroll_n);
        parse_bytes(&mut grid, &mut scrollback, cmd.as_bytes());

        assert_grid_invariants(&grid);
    }

    /// Alt screen enter/exit preserves invariants.
    #[test]
    fn alt_screen_preserves_invariants(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let mut grid = Grid::new(80, 24);
        let mut scrollback = Scrollback::new(100);

        // Write some data, enter alt screen, write more, exit
        parse_bytes(&mut grid, &mut scrollback, b"Hello main screen");
        parse_bytes(&mut grid, &mut scrollback, b"\x1B[?1049h"); // enter alt
        assert_grid_invariants(&grid);

        parse_bytes(&mut grid, &mut scrollback, &data);
        assert_grid_invariants(&grid);

        parse_bytes(&mut grid, &mut scrollback, b"\x1B[?1049l"); // exit alt
        assert_grid_invariants(&grid);
    }

    /// Scrollback ring buffer length is bounded by capacity.
    #[test]
    fn scrollback_bounded_by_capacity(
        line_count in 0usize..500,
        capacity in 1usize..200,
    ) {
        let mut scrollback = Scrollback::new(capacity);
        let row = vec![Cell::default(); 80];
        for _ in 0..line_count {
            scrollback.push_slice(&row);
        }
        // Scrollback length must never exceed capacity
        assert!(scrollback.len() <= capacity);
    }
}
