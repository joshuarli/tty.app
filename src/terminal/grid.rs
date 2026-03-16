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
#[derive(Clone, Copy, Debug, Default)]
pub struct SavedCursor {
    pub row: u16,
    pub col: u16,
    pub fg_index: u8,
    pub bg_index: u8,
    pub flags: CellFlags,
    pub charset_g0: u8,
    pub charset_g1: u8,
}

pub struct Grid {
    pub cells: Vec<Cell>,
    /// Parallel char store for non-BMP codepoints. Only consulted when
    /// cell.codepoint == 0 (the non-BMP sentinel). BMP and ASCII chars are
    /// read directly from Cell.codepoint, so this vec is NOT maintained on
    /// ASCII/BMP write paths — only written for non-BMP codepoints.
    chars: Vec<char>,
    /// True if any non-BMP codepoint has been written since last full clear.
    /// Gates chars maintenance in scroll/insert/delete (skip when false).
    has_non_bmp: bool,
    pub cols: u16,
    pub rows: u16,
    pub dirty: BitVec,

    /// Ring buffer offset: physical row for logical row 0.
    /// Enables O(n*cols) full-screen scroll instead of O(rows*cols) memmove.
    ring_offset: usize,

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
    pub last_atlas: [u8; 2],

    // Synchronized output (mode 2026): when the current sync block began.
    // Set by the parser when 2026 is enabled, cleared when disabled.
    pub sync_start: Option<Instant>,

    // Atlas lookup table for ASCII (set once after atlas preload, never changes)
    ascii_atlas: [[u8; 2]; 128],
    // Atlas position for space character (used by blank cells)
    pub space_atlas: [u8; 2],

    // Alternate screen buffer
    alt_cells: Vec<Cell>,
    alt_chars: Vec<char>,
    alt_ring_offset: usize,
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
            chars: vec![' '; total],
            has_non_bmp: false,
            cells,
            cols,
            rows,
            dirty,
            ring_offset: 0,
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
            last_atlas: [0, 0],
            sync_start: None,
            saved_cursor: SavedCursor::default(),
            ascii_atlas: [[0; 2]; 128],
            space_atlas: [0, 0],
            alt_cells: Vec::new(),
            alt_chars: Vec::new(),
            alt_ring_offset: 0,
            main_cursor: SavedCursor::default(),
        }
    }

    /// Set the ASCII atlas lookup table (called once after atlas preload).
    pub fn set_ascii_atlas(&mut self, table: &[[u8; 2]; 128]) {
        self.ascii_atlas = *table;
        self.space_atlas = table[b' ' as usize];
    }

    /// Map a logical row to the start index in the cells vec.
    #[inline(always)]
    pub fn row_start(&self, logical_row: u16) -> usize {
        ((logical_row as usize + self.ring_offset) % self.rows as usize) * self.cols as usize
    }

    /// Get a slice of cells for a logical row.
    #[inline]
    pub fn row_slice(&self, logical_row: u16) -> &[Cell] {
        let start = self.row_start(logical_row);
        &self.cells[start..start + self.cols as usize]
    }

    #[inline]
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[self.row_start(row) + col as usize]
    }

    #[inline]
    pub fn cell_mut(&mut self, row: u16, col: u16) -> &mut Cell {
        let idx = self.row_start(row) + col as usize;
        &mut self.cells[idx]
    }

    /// Real codepoint for a cell (supports non-BMP unlike Cell.codepoint).
    /// BMP chars are read from Cell.codepoint; non-BMP (sentinel 0) from chars vec.
    #[inline]
    pub fn char_at(&self, row: u16, col: u16) -> char {
        let idx = self.row_start(row) + col as usize;
        let cp = self.cells[idx].codepoint;
        if cp != 0 {
            // SAFETY: BMP codepoints stored in Cell are always valid Unicode.
            unsafe { char::from_u32_unchecked(cp as u32) }
        } else {
            self.chars[idx]
        }
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

    #[inline]
    fn blank(&self) -> Cell {
        Cell::blank(&self.attr, self.space_atlas)
    }

    /// Clear row range [from..to) using current SGR background color.
    pub fn clear_rows(&mut self, from: u16, to: u16) {
        let blank = self.blank();
        let cols = self.cols as usize;
        for row in from..to.min(self.rows) {
            let start = self.row_start(row);
            self.cells[start..start + cols].fill(blank);
            // blank.codepoint = 0x20 (non-zero) → char_at reads from cell, chars is stale-safe
            self.mark_dirty(row);
        }
    }

    /// Clear columns [from_col..to_col) using current SGR background color.
    pub fn clear_cols(&mut self, row: u16, from_col: u16, to_col: u16) {
        let blank = self.blank();
        let start = self.row_start(row) + from_col as usize;
        let end = self.row_start(row) + to_col.min(self.cols) as usize;
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
        if self.scroll_top == 0
            && let Some(sb) = scrollback
        {
            for i in 0..n {
                let start = self.row_start(i);
                sb.push_slice(&self.cells[start..start + cols]);
            }
        }

        if self.scroll_top == 0 && self.scroll_bottom == self.rows - 1 {
            // Full-screen scroll: O(n*cols) ring buffer bump
            self.ring_offset = (self.ring_offset + n as usize) % self.rows as usize;

            // Clear the new bottom rows (old top rows, now at logical bottom)
            let blank = self.blank();
            for i in 0..n {
                let row = self.rows - n + i;
                let start = self.row_start(row);
                self.cells[start..start + cols].fill(blank);
            }
        } else if n > self.scroll_bottom - self.scroll_top {
            // Scroll entire region: just clear it
            let blank = self.blank();
            for i in self.scroll_top..=self.scroll_bottom {
                let start = self.row_start(i);
                self.cells[start..start + cols].fill(blank);
            }
        } else {
            // Partial scroll region: copy row by row
            let sync_chars = self.has_non_bmp;
            for i in self.scroll_top..=self.scroll_bottom - n {
                let src = self.row_start(i + n);
                let dst = self.row_start(i);
                self.cells.copy_within(src..src + cols, dst);
                if sync_chars {
                    self.chars.copy_within(src..src + cols, dst);
                }
            }

            // Clear new rows at bottom of scroll region
            let blank = self.blank();
            for i in (self.scroll_bottom + 1 - n)..=self.scroll_bottom {
                let start = self.row_start(i);
                self.cells[start..start + cols].fill(blank);
            }
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

        if self.scroll_top == 0 && self.scroll_bottom == self.rows - 1 {
            // Full-screen: ring buffer bump backward
            self.ring_offset =
                (self.ring_offset + self.rows as usize - n as usize) % self.rows as usize;

            // Clear the new top rows
            let blank = self.blank();
            for i in 0..n {
                let start = self.row_start(i);
                self.cells[start..start + cols].fill(blank);
            }
        } else {
            // Partial: copy row by row (from bottom to top to avoid clobbering)
            let sync_chars = self.has_non_bmp;
            for i in (self.scroll_top + n..=self.scroll_bottom).rev() {
                let src = self.row_start(i - n);
                let dst = self.row_start(i);
                self.cells.copy_within(src..src + cols, dst);
                if sync_chars {
                    self.chars.copy_within(src..src + cols, dst);
                }
            }

            // Clear new rows at top of scroll region
            let blank = self.blank();
            for i in self.scroll_top..self.scroll_top + n {
                let start = self.row_start(i);
                self.cells[start..start + cols].fill(blank);
            }
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
        self.main_cursor = self.current_saved_cursor();

        // Swap buffers (reuse existing allocation when possible)
        let total = self.cols as usize * self.rows as usize;
        self.alt_cells.clear();
        self.alt_cells.resize(total, Cell::default());
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        self.alt_chars.clear();
        self.alt_chars.resize(total, ' ');
        std::mem::swap(&mut self.chars, &mut self.alt_chars);

        // Save main ring offset, reset for alt screen
        self.alt_ring_offset = self.ring_offset;
        self.ring_offset = 0;

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
        std::mem::swap(&mut self.chars, &mut self.alt_chars);
        self.alt_chars.clear();

        // Restore main ring offset
        self.ring_offset = self.alt_ring_offset;
        self.alt_ring_offset = 0;

        let saved = self.main_cursor;
        self.restore_cursor_from(&saved);
        self.mark_all_dirty();
    }

    fn current_saved_cursor(&self) -> SavedCursor {
        SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg_index: self.attr.fg_index,
            bg_index: self.attr.bg_index,
            flags: self.attr.flags,
            charset_g0: self.charset_g0,
            charset_g1: self.charset_g1,
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.current_saved_cursor();
    }

    pub fn restore_cursor(&mut self) {
        let saved = self.saved_cursor;
        self.restore_cursor_from(&saved);
    }

    fn restore_cursor_from(&mut self, saved: &SavedCursor) {
        self.cursor_row = saved.row.min(self.rows.saturating_sub(1));
        self.cursor_col = saved.col.min(self.cols.saturating_sub(1));
        self.attr.fg_index = saved.fg_index;
        self.attr.bg_index = saved.bg_index;
        self.attr.flags = saved.flags;
        self.charset_g0 = saved.charset_g0;
        self.charset_g1 = saved.charset_g1;
    }

    /// Resize the grid. Existing content is preserved where possible.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        let new_total = new_cols as usize * new_rows as usize;
        let mut new_cells = vec![Cell::default(); new_total];
        let mut new_chars = vec![' '; new_total];
        let copy_rows = self.rows.min(new_rows) as usize;
        let copy_cols = self.cols.min(new_cols) as usize;

        for row in 0..copy_rows {
            // Use row_start to handle ring buffer offset
            let src_start = self.row_start(row as u16);
            let dst_start = row * new_cols as usize;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
            new_chars[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.chars[src_start..src_start + copy_cols]);
        }

        self.cells = new_cells;
        self.chars = new_chars;
        self.cols = new_cols;
        self.rows = new_rows;
        self.ring_offset = 0; // Flatten ring on resize
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

    /// Bulk-write a run of printable ASCII bytes at the current cursor position.
    /// Atlas coords are resolved from the internal ascii_atlas table.
    pub fn write_ascii_run(&mut self, bytes: &[u8]) {
        let cols = self.cols as usize;
        let attr = self.attr;

        let mut i = 0;
        while i < bytes.len() {
            // Handle pending wrap from a previous write
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
            let col = self.cursor_col as usize;

            // How many chars fit on the remainder of this row?
            let space = cols - col;
            let n = space.min(bytes.len() - i);

            // Write cells in a tight loop — one index increment per char
            // No chars[] update needed: cell.codepoint is set to the ASCII value
            // (non-zero), so char_at() reads directly from the cell.
            let base = self.row_start(row) + col;
            for j in 0..n {
                let b = bytes[i + j];
                let ap = self.ascii_atlas[b as usize];
                let cell = &mut self.cells[base + j];
                cell.codepoint = b as u16;
                cell.flags = attr.flags;
                cell.fg_index = attr.fg_index;
                cell.bg_index = attr.bg_index;
                cell.atlas_x = ap[0];
                cell.atlas_y = ap[1];
            }
            self.mark_dirty(row);

            i += n;

            // Advance cursor
            if col + n >= cols {
                // Filled to end of row — park cursor on last col, set pending wrap
                self.cursor_col = self.cols - 1;
                self.cursor_pending_wrap = true;
            } else {
                self.cursor_col = (col + n) as u16;
            }
        }
    }

    /// Write a character at the current cursor position with current attributes.
    /// Atlas coords are provided by the caller (resolved from atlas at parse time).
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

        let idx = self.row_start(row) + col as usize;
        let attr = self.attr;
        let cp = c as u32;
        let cell = &mut self.cells[idx];
        // BMP: store codepoint directly (char_at reads from cell).
        // Non-BMP: store 0 sentinel, write real char to chars vec.
        if cp <= 0xFFFF {
            cell.codepoint = cp as u16;
        } else {
            cell.codepoint = 0;
            self.chars[idx] = c;
            self.has_non_bmp = true;
        }
        cell.flags = attr.flags;
        cell.fg_index = attr.fg_index;
        cell.bg_index = attr.bg_index;
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

    /// Insert n blank characters at the cursor, shifting existing content right.
    pub fn insert_chars(&mut self, n: u16) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cols = self.cols;
        let n = n.min(cols - col);

        let row_start = self.row_start(row);
        let src = row_start + col as usize;
        let dst = row_start + (col + n) as usize;
        let count = (cols - col - n) as usize;
        self.cells.copy_within(src..src + count, dst);
        if self.has_non_bmp {
            self.chars.copy_within(src..src + count, dst);
        }
        let blank = Cell::blank(&self.attr, self.space_atlas);
        self.cells[src..src + n as usize].fill(blank);
        self.mark_dirty(row);
    }

    /// Delete n characters at the cursor, shifting content left and filling right with blanks.
    pub fn delete_chars(&mut self, n: u16) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cols = self.cols;
        let n = n.min(cols - col);

        let row_start = self.row_start(row);
        let dst = row_start + col as usize;
        let src = row_start + (col + n) as usize;
        let count = (cols - col - n) as usize;
        self.cells.copy_within(src..src + count, dst);
        if self.has_non_bmp {
            self.chars.copy_within(src..src + count, dst);
        }
        let blank = Cell::blank(&self.attr, self.space_atlas);
        let fill_start = row_start + (cols - n) as usize;
        self.cells[fill_start..fill_start + n as usize].fill(blank);
        self.mark_dirty(row);
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
        let idx = self.row_start(row) + col as usize;
        let cp = c as u32;

        let cp16 = if cp <= 0xFFFF { cp as u16 } else { 0 };
        if cp > 0xFFFF {
            self.chars[idx] = c;
            self.chars[idx + 1] = c;
            self.has_non_bmp = true;
        }

        // First cell
        let cell = &mut self.cells[idx];
        cell.codepoint = cp16;
        cell.flags = attr.flags | CellFlags::WIDE;
        cell.fg_index = attr.fg_index;
        cell.bg_index = attr.bg_index;
        cell.atlas_x = atlas_x;
        cell.atlas_y = atlas_y;

        // Continuation cell
        let cell2 = &mut self.cells[idx + 1];
        cell2.codepoint = cp16;
        cell2.flags = CellFlags::WIDE_CONT;
        cell2.fg_index = attr.fg_index;
        cell2.bg_index = attr.bg_index;
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
