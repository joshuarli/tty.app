use std::time::Instant;

use bitvec::prelude::*;

use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::scrollback::Scrollback;

// Terminal mode flags (DECSET/DECRST)
bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct TermMode: u32 {
        const CURSOR_KEYS    = 0x0001;  // DECCKM: application cursor keys
        const AUTO_WRAP      = 0x0002;  // DECAWM
        const CURSOR_VISIBLE = 0x0004;  // DECTCEM
        const ALT_SCREEN     = 0x0008;  // Mode 1049
        const MOUSE_BUTTON   = 0x0010;  // Mode 1000
        const MOUSE_MOTION   = 0x0020;  // Mode 1002
        const MOUSE_ALL      = 0x0040;  // Mode 1003
        const MOUSE_SGR      = 0x0080;  // Mode 1006
        const FOCUS_EVENTS   = 0x0100;  // Mode 1004
        const BRACKETED_PASTE = 0x0200; // Mode 2004
        const SYNC_OUTPUT    = 0x0400;  // Mode 2026
        const ORIGIN_MODE    = 0x0800;  // DECOM
    }
}

/// Saved cursor state for DECSC/DECRC
#[derive(Clone, Debug, Default)]
pub struct SavedCursor {
    pub row: u16,
    pub col: u16,
    pub fg_index: u8,
    pub bg_index: u8,
    pub fg_rgb: u32,
    pub bg_rgb: u32,
    pub flags: CellFlags,
    pub charset_g0: u8,
    pub charset_g1: u8,
}

pub struct Grid {
    pub cells: Vec<Cell>,
    pub cols: u16,
    pub rows: u16,
    pub dirty: BitVec,

    // Cursor
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_pending_wrap: bool, // DECAWM deferred wrap

    // Current SGR attributes
    pub attr: Cell,

    // Terminal modes
    pub mode: TermMode,

    // Scroll region (top, bottom) — inclusive, 0-indexed
    pub scroll_top: u16,
    pub scroll_bottom: u16,

    // Character set state
    pub charset_g0: u8, // 0 = ASCII, 1 = DEC Special
    pub charset_g1: u8,
    pub active_charset: u8, // 0 = G0, 1 = G1

    // Tab stops
    pub tab_stops: BitVec,

    // Saved cursor
    pub saved_cursor: SavedCursor,

    // Last printed character (for REP / CSI b)
    pub last_char: char,

    // Synchronized output (mode 2026): when the current sync block began.
    // Set by the parser when 2026 is enabled, cleared when disabled.
    pub sync_start: Option<Instant>,

    // Alternate screen buffer
    alt_cells: Vec<Cell>,
    main_cursor: SavedCursor,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        let total = cols as usize * rows as usize;
        let cells = vec![Cell::default(); total];
        let dirty = bitvec![1; rows as usize]; // all dirty initially

        // Default tab stops every 8 columns
        let mut tab_stops = bitvec![0; cols as usize];
        for i in (8..cols as usize).step_by(8) {
            tab_stops.set(i, true);
        }

        let attr = Cell {
            codepoint: b' ' as u16,
            ..Cell::default()
        };

        Grid {
            cells,
            cols,
            rows,
            dirty,
            cursor_row: 0,
            cursor_col: 0,
            cursor_pending_wrap: false,
            attr,
            mode: TermMode::AUTO_WRAP | TermMode::CURSOR_VISIBLE | TermMode::BRACKETED_PASTE,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            charset_g0: 0,
            charset_g1: 0,
            active_charset: 0,
            tab_stops,
            last_char: ' ',
            sync_start: None,
            saved_cursor: SavedCursor::default(),
            alt_cells: Vec::new(),
            main_cursor: SavedCursor::default(),
        }
    }

    #[inline]
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[row as usize * self.cols as usize + col as usize]
    }

    #[inline]
    pub fn cell_mut(&mut self, row: u16, col: u16) -> &mut Cell {
        let idx = row as usize * self.cols as usize + col as usize;
        &mut self.cells[idx]
    }

    #[inline]
    pub fn mark_dirty(&mut self, row: u16) {
        if (row as usize) < self.dirty.len() {
            self.dirty.set(row as usize, true);
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.fill(true);
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.fill(false);
    }

    /// Clear row range [from..to) using current SGR background color.
    pub fn clear_rows(&mut self, from: u16, to: u16) {
        let blank = Cell::blank(&self.attr);
        for row in from..to.min(self.rows) {
            let start = row as usize * self.cols as usize;
            let end = start + self.cols as usize;
            self.cells[start..end].fill(blank);
            self.mark_dirty(row);
        }
    }

    /// Clear columns [from_col..to_col) using current SGR background color.
    pub fn clear_cols(&mut self, row: u16, from_col: u16, to_col: u16) {
        let blank = Cell::blank(&self.attr);
        let cols = self.cols;
        let start = row as usize * cols as usize + from_col as usize;
        let end = row as usize * cols as usize + to_col.min(cols) as usize;
        self.cells[start..end].fill(blank);
        self.mark_dirty(row);
    }

    /// Scroll the region [scroll_top..=scroll_bottom] up by n lines.
    /// New lines at the bottom are cleared.
    /// If scrollback is provided and scroll_top == 0, evicted rows are pushed directly.
    pub fn scroll_up_into(&mut self, n: u16, scrollback: Option<&mut Scrollback>) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let cols = self.cols as usize;

        // Push evicted rows directly into scrollback (no intermediate Vec)
        if self.scroll_top == 0 {
            if let Some(sb) = scrollback {
                for row in 0..n {
                    let start = row as usize * cols;
                    sb.push_slice(&self.cells[start..start + cols]);
                }
            }
        }

        // memmove cells up
        let src_start = (self.scroll_top + n) as usize * cols;
        let dst_start = self.scroll_top as usize * cols;
        let count = ((self.scroll_bottom - self.scroll_top + 1 - n) as usize) * cols;
        self.cells
            .copy_within(src_start..src_start + count, dst_start);

        // Clear new rows at bottom of scroll region (use current bg color)
        let blank = Cell::blank(&self.attr);
        let clear_from = self.scroll_bottom + 1 - n;
        for row in clear_from..=self.scroll_bottom {
            let start = row as usize * cols;
            self.cells[start..start + cols].fill(blank);
        }

        // Mark affected rows dirty
        for row in self.scroll_top..=self.scroll_bottom {
            self.mark_dirty(row);
        }
    }

    /// Scroll up without scrollback (used by insert_lines/delete_lines).
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_up_into(n, None);
    }

    /// Scroll the region [scroll_top..=scroll_bottom] down by n lines.
    /// New lines at the top are cleared.
    pub fn scroll_down(&mut self, n: u16) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let cols = self.cols as usize;

        // memmove cells down
        let src_start = self.scroll_top as usize * cols;
        let dst_start = (self.scroll_top + n) as usize * cols;
        let count = ((self.scroll_bottom - self.scroll_top + 1 - n) as usize) * cols;
        self.cells
            .copy_within(src_start..src_start + count, dst_start);

        // Clear new rows at top of scroll region (use current bg color)
        let blank = Cell::blank(&self.attr);
        for row in self.scroll_top..self.scroll_top + n {
            let start = row as usize * cols;
            self.cells[start..start + cols].fill(blank);
        }

        for row in self.scroll_top..=self.scroll_bottom {
            self.mark_dirty(row);
        }
    }

    /// Switch to alternate screen buffer (mode 1049)
    pub fn enter_alt_screen(&mut self) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        self.mode.insert(TermMode::ALT_SCREEN);
        self.save_cursor_to(&mut self.main_cursor.clone());
        self.main_cursor = self.current_saved_cursor();

        // Swap buffers (reuse existing allocation when possible)
        let total = self.cols as usize * self.rows as usize;
        self.alt_cells.clear();
        self.alt_cells.resize(total, Cell::default());
        std::mem::swap(&mut self.cells, &mut self.alt_cells);

        self.cursor_row = 0;
        self.cursor_col = 0;
        self.mark_all_dirty();
    }

    /// Switch back to main screen buffer
    pub fn exit_alt_screen(&mut self) {
        if !self.mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        self.mode.remove(TermMode::ALT_SCREEN);
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        self.alt_cells.clear();

        self.restore_cursor_from(&self.main_cursor.clone());
        self.mark_all_dirty();
    }

    fn current_saved_cursor(&self) -> SavedCursor {
        SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg_index: self.attr.fg_index,
            bg_index: self.attr.bg_index,
            fg_rgb: self.attr.fg_rgb,
            bg_rgb: self.attr.bg_rgb,
            flags: self.attr.flags,
            charset_g0: self.charset_g0,
            charset_g1: self.charset_g1,
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.current_saved_cursor();
    }

    fn save_cursor_to(&self, target: &mut SavedCursor) {
        *target = self.current_saved_cursor();
    }

    pub fn restore_cursor(&mut self) {
        let saved = self.saved_cursor.clone();
        self.restore_cursor_from(&saved);
    }

    fn restore_cursor_from(&mut self, saved: &SavedCursor) {
        self.cursor_row = saved.row.min(self.rows.saturating_sub(1));
        self.cursor_col = saved.col.min(self.cols.saturating_sub(1));
        self.attr.fg_index = saved.fg_index;
        self.attr.bg_index = saved.bg_index;
        self.attr.fg_rgb = saved.fg_rgb;
        self.attr.bg_rgb = saved.bg_rgb;
        self.attr.flags = saved.flags;
        self.charset_g0 = saved.charset_g0;
        self.charset_g1 = saved.charset_g1;
    }

    /// Resize the grid. Existing content is preserved where possible.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        let mut new_cells = vec![Cell::default(); new_cols as usize * new_rows as usize];
        let copy_rows = self.rows.min(new_rows) as usize;
        let copy_cols = self.cols.min(new_cols) as usize;

        for row in 0..copy_rows {
            let src_start = row * self.cols as usize;
            let dst_start = row * new_cols as usize;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
        }

        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;
        self.dirty = bitvec![1; new_rows as usize];

        // Reset scroll region to full screen
        self.scroll_top = 0;
        self.scroll_bottom = new_rows.saturating_sub(1);

        // Clamp cursor
        self.cursor_row = self.cursor_row.min(new_rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(new_cols.saturating_sub(1));

        // Rebuild tab stops
        self.tab_stops = bitvec![0; new_cols as usize];
        for i in (8..new_cols as usize).step_by(8) {
            self.tab_stops.set(i, true);
        }
    }

    /// Write a character at the current cursor position with current attributes.
    pub fn write_char(&mut self, c: char, atlas_x: u8, atlas_y: u8) {
        if self.cursor_pending_wrap {
            if self.mode.contains(TermMode::AUTO_WRAP) {
                self.cursor_col = 0;
                if self.cursor_row == self.scroll_bottom {
                    self.scroll_up(1);
                } else if self.cursor_row < self.rows - 1 {
                    self.cursor_row += 1;
                }
            }
            self.cursor_pending_wrap = false;
        }

        let row = self.cursor_row;
        let col = self.cursor_col;

        let idx = row as usize * self.cols as usize + col as usize;
        let attr = self.attr;
        let cell = &mut self.cells[idx];
        cell.codepoint = c as u16;
        cell.flags = attr.flags;
        cell.fg_index = attr.fg_index;
        cell.bg_index = attr.bg_index;
        cell.fg_rgb = attr.fg_rgb;
        cell.bg_rgb = attr.bg_rgb;
        cell.atlas_x = atlas_x;
        cell.atlas_y = atlas_y;
        self.mark_dirty(row);

        // Advance cursor
        if self.cursor_col >= self.cols - 1 {
            self.cursor_pending_wrap = true;
        } else {
            self.cursor_col += 1;
        }
    }

    /// Write a wide character (2 cells). Sets WIDE flag on first cell, WIDE_CONT on second.
    pub fn write_wide_char(&mut self, c: char, atlas_x: u8, atlas_y: u8) {
        if self.cursor_pending_wrap {
            if self.mode.contains(TermMode::AUTO_WRAP) {
                self.cursor_col = 0;
                if self.cursor_row == self.scroll_bottom {
                    self.scroll_up(1);
                } else if self.cursor_row < self.rows - 1 {
                    self.cursor_row += 1;
                }
            }
            self.cursor_pending_wrap = false;
        }

        // If at the last column, we need to wrap first
        if self.cursor_col >= self.cols - 1 {
            if self.mode.contains(TermMode::AUTO_WRAP) {
                // Clear the last column and wrap
                self.clear_cols(self.cursor_row, self.cursor_col, self.cols);
                self.cursor_col = 0;
                if self.cursor_row == self.scroll_bottom {
                    self.scroll_up(1);
                } else if self.cursor_row < self.rows - 1 {
                    self.cursor_row += 1;
                }
            } else {
                return; // can't fit wide char
            }
        }

        let row = self.cursor_row;
        let col = self.cursor_col;

        let attr = self.attr;
        let idx = row as usize * self.cols as usize + col as usize;

        // First cell
        let cell = &mut self.cells[idx];
        cell.codepoint = c as u16;
        cell.flags = attr.flags | CellFlags::WIDE;
        cell.fg_index = attr.fg_index;
        cell.bg_index = attr.bg_index;
        cell.fg_rgb = attr.fg_rgb;
        cell.bg_rgb = attr.bg_rgb;
        cell.atlas_x = atlas_x;
        cell.atlas_y = atlas_y;

        // Continuation cell — carry atlas coords so the shader can sample directly
        let cell2 = &mut self.cells[idx + 1];
        cell2.codepoint = b' ' as u16;
        cell2.flags = CellFlags::WIDE_CONT;
        cell2.fg_index = attr.fg_index;
        cell2.bg_index = attr.bg_index;
        cell2.fg_rgb = attr.fg_rgb;
        cell2.bg_rgb = attr.bg_rgb;
        cell2.atlas_x = atlas_x;
        cell2.atlas_y = atlas_y;

        self.mark_dirty(row);

        // Advance cursor by 2
        if self.cursor_col + 2 >= self.cols {
            self.cursor_pending_wrap = true;
            self.cursor_col = self.cols - 1;
        } else {
            self.cursor_col += 2;
        }
    }
}
