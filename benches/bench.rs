//! Benchmark harness for `tty`.
//!
//! Tracks wall time (criterion) and heap allocations (counting allocator).
//! Run: `cargo bench`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use tty::parser::Parser;
use tty::parser::perform::Perform;
use tty::parser::simd::SimdScanner;
use tty::terminal::cell::{Cell, CellFlags};
use tty::terminal::grid::{Grid, TermMode};
use tty::terminal::scrollback::Scrollback;

// ---------------------------------------------------------------------------
// Counting allocator — tracks allocations, live bytes, and peak (high-water)
// ---------------------------------------------------------------------------

struct CountingAlloc;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

fn update_peak() {
    let live = LIVE_BYTES.load(Relaxed);
    let mut peak = PEAK_BYTES.load(Relaxed);
    while live > peak {
        match PEAK_BYTES.compare_exchange_weak(peak, live, Relaxed, Relaxed) {
            Ok(_) => break,
            Err(actual) => peak = actual,
        }
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(layout.size(), Relaxed);
        LIVE_BYTES.fetch_add(layout.size(), Relaxed);
        update_peak();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Relaxed);
        if new_size > layout.size() {
            let delta = new_size - layout.size();
            ALLOC_BYTES.fetch_add(delta, Relaxed);
            LIVE_BYTES.fetch_add(delta, Relaxed);
        } else {
            let delta = layout.size() - new_size;
            LIVE_BYTES.fetch_sub(delta, Relaxed);
        }
        update_peak();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn reset_alloc_counters() {
    ALLOC_COUNT.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    PEAK_BYTES.store(LIVE_BYTES.load(Relaxed), Relaxed);
}

fn alloc_count() -> usize {
    ALLOC_COUNT.load(Relaxed)
}

fn alloc_bytes() -> usize {
    ALLOC_BYTES.load(Relaxed)
}

fn peak_bytes() -> usize {
    PEAK_BYTES.load(Relaxed)
}

#[derive(Clone, Copy)]
struct AllocStats {
    count: usize,
    bytes: usize,
    peak_delta: usize,
}

impl std::fmt::Display for AllocStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn human(n: usize) -> String {
            if n >= 1_048_576 {
                format!("{:.1} MiB", n as f64 / 1_048_576.0)
            } else if n >= 1024 {
                format!("{:.1} KiB", n as f64 / 1024.0)
            } else {
                format!("{n} B")
            }
        }
        write!(
            f,
            "{} allocs, {} total, {} peak",
            self.count,
            human(self.bytes),
            human(self.peak_delta)
        )
    }
}

fn measure_allocs<F: FnOnce()>(f: F) -> AllocStats {
    let baseline = LIVE_BYTES.load(Relaxed);
    reset_alloc_counters();
    f();
    let peak = peak_bytes();
    AllocStats {
        count: alloc_count(),
        bytes: alloc_bytes(),
        peak_delta: peak.saturating_sub(baseline),
    }
}

// ---------------------------------------------------------------------------
// Minimal performer for benchmarks (mirrors TestPerformer from proptest_grid)
// ---------------------------------------------------------------------------

struct BenchPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
}

impl<'a> Perform for BenchPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        self.grid.write_ascii_run(bytes);
        if let Some(&last) = bytes.last() {
            self.grid.last_char = last as char;
        }
    }

    fn print(&mut self, c: char) {
        self.grid.write_char(c, 0, 0);
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

    fn insert_chars(&mut self, n: u16) {
        self.grid.insert_chars(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.grid.delete_chars(n);
    }
    fn erase_chars(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        self.grid
            .clear_cols(row, col, (col + n).min(self.grid.cols));
    }

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.grid.attr.flags = CellFlags::empty();
                    self.grid.attr.fg_index = 7;
                    self.grid.attr.bg_index = 0;
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
                                    self.grid.attr.fg_index = tty::config::rgb_to_palette(
                                        params[i + 1] as u8,
                                        params[i + 2] as u8,
                                        params[i + 3] as u8,
                                    );
                                    i += 3;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                39 => self.grid.attr.fg_index = 7,
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
                                    self.grid.attr.bg_index = tty::config::rgb_to_palette(
                                        params[i + 1] as u8,
                                        params[i + 2] as u8,
                                        params[i + 3] as u8,
                                    );
                                    i += 3;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                49 => self.grid.attr.bg_index = 0,
                90..=97 => self.grid.attr.fg_index = (params[i] - 90 + 8) as u8,
                100..=107 => self.grid.attr.bg_index = (params[i] - 100 + 8) as u8,
                _ => {}
            }
            i += 1;
        }
    }

    #[inline]
    fn sgr_reset(&mut self) {
        self.grid.attr.flags = CellFlags::empty();
        self.grid.attr.fg_index = 7;
        self.grid.attr.bg_index = 0;
    }

    #[inline]
    fn sgr_single(&mut self, code: u16) {
        match code {
            0 => self.sgr_reset(),
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
            30..=37 => self.grid.attr.fg_index = (code - 30) as u8,
            39 => self.grid.attr.fg_index = 7,
            40..=47 => self.grid.attr.bg_index = (code - 40) as u8,
            49 => self.grid.attr.bg_index = 0,
            90..=97 => self.grid.attr.fg_index = (code - 90 + 8) as u8,
            100..=107 => self.grid.attr.bg_index = (code - 100 + 8) as u8,
            _ => {}
        }
    }

    #[inline]
    fn color_256(&mut self, fg: bool, index: u16) {
        if fg {
            self.grid.attr.fg_index = index as u8;
        } else {
            self.grid.attr.bg_index = index as u8;
        }
    }

    #[inline]
    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        let index = tty::config::rgb_to_palette(r as u8, g as u8, b as u8);
        if fg {
            self.grid.attr.fg_index = index;
        } else {
            self.grid.attr.bg_index = index;
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
            self.grid.write_char(c, 0, 0);
        }
    }
    fn sgr_colon(&mut self, _raw: &[u8]) {}
}

fn parse_bytes(grid: &mut Grid, scrollback: &mut Scrollback, data: &[u8]) {
    let mut parser = Parser::new();
    let mut performer = BenchPerformer { grid, scrollback };
    parser.parse(data, &mut performer);
}

// ---------------------------------------------------------------------------
// Test data generators
// ---------------------------------------------------------------------------

/// Pure printable ASCII — exercises SIMD fast path.
fn make_ascii(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 80);
    for i in 0..n {
        // 80-col lines of printable ASCII
        for j in 0..79 {
            buf.push(b' ' + ((i * 79 + j) % 95) as u8);
        }
        buf.push(b'\n');
    }
    buf
}

/// Mixed ASCII + CSI escape sequences — exercises CSI fast path.
fn make_mixed_csi(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 60);
    for i in 0..n {
        // Some text
        buf.extend_from_slice(b"Hello, terminal world! ");
        // SGR: bold + color
        buf.extend_from_slice(b"\x1b[1;32m");
        buf.extend_from_slice(b"green bold");
        buf.extend_from_slice(b"\x1b[0m");
        // Cursor movement
        buf.extend_from_slice(format!("\x1b[{};1H", (i % 24) + 1).as_bytes());
        // Erase to end of line
        buf.extend_from_slice(b"\x1b[K");
        buf.push(b'\n');
    }
    buf
}

/// Heavy UTF-8 multibyte — exercises UTF-8 assembler.
fn make_utf8_heavy(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 60);
    let chars = "日本語テスト 中文测试 한국어 Ñoño café über αβγδ";
    for _ in 0..n {
        buf.extend_from_slice(chars.as_bytes());
        buf.push(b'\n');
    }
    buf
}

/// Pure box-drawing characters — worst case for UTF-8 per-char overhead.
fn make_box_drawing(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 240);
    // ─ = U+2500 = E2 94 80 (3 bytes each, 79 chars + newline per line)
    for _ in 0..n {
        for _ in 0..79 {
            buf.extend_from_slice("─".as_bytes()); // 3 bytes
        }
        buf.push(b'\n');
    }
    buf
}

/// TUI-like output: box borders with ASCII content inside.
fn make_tui_mixed(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 120);
    for i in 0..n {
        match i % 4 {
            0 => {
                // Top border: ┌─────────────────────────────────────────────┐
                buf.extend_from_slice("┌".as_bytes());
                for _ in 0..77 {
                    buf.extend_from_slice("─".as_bytes());
                }
                buf.extend_from_slice("┐".as_bytes());
            }
            3 => {
                // Bottom border
                buf.extend_from_slice("└".as_bytes());
                for _ in 0..77 {
                    buf.extend_from_slice("─".as_bytes());
                }
                buf.extend_from_slice("┘".as_bytes());
            }
            _ => {
                // Content: │ text content here                              │
                buf.extend_from_slice("│".as_bytes());
                buf.extend_from_slice(format!(" item {i:<73}").as_bytes());
                buf.extend_from_slice("│".as_bytes());
            }
        }
        buf.push(b'\n');
    }
    buf
}

/// SGR-dense colored output — simulates `git diff --color` or compiler errors.
/// Every line has multiple color changes, stressing SGR parsing throughput.
fn make_git_diff_color(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 120);
    for i in 0..n {
        match i % 5 {
            0 => {
                // Diff header: bold white
                buf.extend_from_slice(
                    b"\x1b[1;37mdiff --git a/src/parser/mod.rs b/src/parser/mod.rs\x1b[0m",
                );
            }
            1 => {
                // Hunk header: cyan
                buf.extend_from_slice(b"\x1b[36m@@ -120,7 +120,9 @@ impl Parser {\x1b[0m");
            }
            2 => {
                // Context line: default
                buf.extend_from_slice(b"         let byte = data[pos];");
            }
            3 => {
                // Removed line: red bg + red fg
                buf.extend_from_slice(b"\x1b[31m-        ");
                buf.extend_from_slice(b"\x1b[41;37mold_code\x1b[31m(foo, bar, baz);\x1b[0m");
            }
            _ => {
                // Added line: green bg + green fg
                buf.extend_from_slice(b"\x1b[32m+        ");
                buf.extend_from_slice(b"\x1b[42;37mnew_code\x1b[32m(foo, bar, baz, qux);\x1b[0m");
            }
        }
        buf.extend_from_slice(b"\r\n");
    }
    buf
}

/// Dense scroll — short lines that force a scroll on nearly every line.
/// Isolates scroll_up + scrollback push throughput (the `cat huge.log` workload).
fn make_dense_scroll(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 20);
    for i in 0..n {
        // Short log-style lines: "[INFO] message_1234"
        buf.extend_from_slice(b"[INFO] message_");
        buf.extend_from_slice(format!("{i}").as_bytes());
        buf.push(b'\n');
    }
    buf
}

/// Full-screen redraw — CUP to each row, write content, clear to EOL.
/// Simulates htop/vim/tmux screen refresh (the dominant TUI rendering pattern).
fn make_fullscreen_redraw(n: usize, cols: u16, rows: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * rows as usize * (cols as usize + 20));
    for frame in 0..n {
        // Hide cursor during redraw (common optimization)
        buf.extend_from_slice(b"\x1b[?25l");
        for row in 1..=rows {
            // CUP to row start
            buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
            // Write content — alternate between status-like and content-like
            if row == 1 || row == rows {
                // Status bars: SGR color + text + reset
                buf.extend_from_slice(b"\x1b[7m"); // inverse
                for j in 0..cols {
                    buf.push(b' ' + ((frame * 79 + j as usize) % 95) as u8);
                }
                buf.extend_from_slice(b"\x1b[0m");
            } else {
                // Content: plain text
                for j in 0..(cols - 1) {
                    buf.push(b' ' + ((row as usize * 79 + j as usize + frame) % 95) as u8);
                }
            }
            // Erase to end of line (clear any leftover from previous frame)
            buf.extend_from_slice(b"\x1b[K");
        }
        // Show cursor
        buf.extend_from_slice(b"\x1b[?25h");
    }
    buf
}

/// Claude Code CLI-style TUI: streaming markdown with spinner updates and status bar.
/// Interleaves: streaming text (ASCII + emoji/UTF-8), spinner animation at a fixed
/// position, and periodic status bar redraws with save/restore cursor.
fn make_claude_code_tui(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 200);
    let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    for i in 0..n {
        match i % 8 {
            0..=3 => {
                // Streaming markdown text line (mixed ASCII + occasional UTF-8/emoji)
                match i % 4 {
                    0 => {
                        // Code block line
                        buf.extend_from_slice(b"\x1b[38;5;245m");
                        buf.extend_from_slice(
                            b"  fn process_data(&mut self, input: &[u8]) -> Result<()> {",
                        );
                        buf.extend_from_slice(b"\x1b[0m\r\n");
                    }
                    1 => {
                        // Bullet point with emoji
                        buf.extend_from_slice(b"  ");
                        buf.extend_from_slice("• ".as_bytes());
                        buf.extend_from_slice(b"\x1b[1m");
                        buf.extend_from_slice(b"Fixed the parser");
                        buf.extend_from_slice(b"\x1b[0m");
                        buf.extend_from_slice(" — reduced latency by ".as_bytes());
                        buf.extend_from_slice(b"\x1b[32m");
                        buf.extend_from_slice(b"42%");
                        buf.extend_from_slice(b"\x1b[0m ");
                        buf.extend_from_slice("✓".as_bytes());
                        buf.extend_from_slice(b"\r\n");
                    }
                    2 => {
                        // Plain text continuation
                        buf.extend_from_slice(b"    The implementation uses SIMD intrinsics for");
                        buf.extend_from_slice(b" bulk classification of byte ranges.\r\n");
                    }
                    _ => {
                        // Heading with bold
                        buf.extend_from_slice(b"\x1b[1;4m");
                        buf.extend_from_slice(b"## Performance Results");
                        buf.extend_from_slice(b"\x1b[0m\r\n");
                    }
                }
            }
            4 | 5 => {
                // Spinner update: save cursor, jump to spinner position, write, restore
                let spin = spinners[i % spinners.len()];
                buf.extend_from_slice(b"\x1b7"); // DECSC (save cursor)
                buf.extend_from_slice(b"\x1b[1;1H"); // CUP to row 1
                buf.extend_from_slice(b"\x1b[36m"); // cyan
                buf.extend_from_slice(spin.as_bytes());
                buf.extend_from_slice(b" Thinking");
                buf.extend(std::iter::repeat_n(b'.', i % 4));
                buf.extend_from_slice(b"\x1b[K"); // clear rest of line
                buf.extend_from_slice(b"\x1b[0m");
                buf.extend_from_slice(b"\x1b8"); // DECRC (restore cursor)
            }
            6 => {
                // Status bar redraw: save, jump to bottom, inverse video, write, restore
                buf.extend_from_slice(b"\x1b7"); // save
                buf.extend_from_slice(b"\x1b[24;1H"); // jump to row 24
                buf.extend_from_slice(b"\x1b[7m"); // inverse
                buf.extend_from_slice(
                    format!(
                        " tokens: {:<6} | cost: ${:.4} | elapsed: {}s ",
                        i * 47,
                        i as f64 * 0.0003,
                        i / 10
                    )
                    .as_bytes(),
                );
                buf.extend_from_slice(b"\x1b[K"); // fill rest with inverse
                buf.extend_from_slice(b"\x1b[0m");
                buf.extend_from_slice(b"\x1b8"); // restore
            }
            _ => {
                // OSC title update (window title with progress)
                buf.extend_from_slice(b"\x1b]0;");
                buf.extend_from_slice(format!("Claude Code - working ({i}/{n})").as_bytes());
                buf.push(0x07); // BEL terminates OSC
            }
        }
    }
    buf
}

/// tmux 2-pane redraw: CUP to each row per pane, text with SGR colors, EL,
/// box-drawing pane borders. Simulates what tmux sends on every screen refresh.
fn make_tmux_pane_redraw(frames: usize, cols: u16, rows: u16) -> Vec<u8> {
    let pane_cols = (cols / 2 - 1) as usize; // each pane minus border
    let mut buf = Vec::with_capacity(frames * rows as usize * (cols as usize + 40));

    for frame in 0..frames {
        // Hide cursor during redraw
        buf.extend_from_slice(b"\x1b[?25l");

        for row in 1..=rows {
            // --- Left pane content ---
            buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
            if row == 1 {
                // Colored prompt line
                buf.extend_from_slice(b"\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/dev\x1b[0m$ ");
                buf.extend_from_slice(b"cargo build");
            } else if row == rows {
                // Status-like line
                buf.extend_from_slice(b"\x1b[38;5;240m");
                for j in 0..pane_cols {
                    buf.push(b' ' + ((frame + j) % 95) as u8);
                }
                buf.extend_from_slice(b"\x1b[0m");
            } else {
                // Regular output with occasional color
                if (row as usize + frame).is_multiple_of(3) {
                    buf.extend_from_slice(b"\x1b[33mwarning\x1b[0m: unused variable `x`");
                } else {
                    buf.extend_from_slice(b"   Compiling tty v0.1.4 (/Users/josh/dev/tty)");
                }
            }
            buf.extend_from_slice(b"\x1b[K"); // erase to EOL

            // --- Pane border (box-drawing, positioned mid-screen) ---
            let border_col = pane_cols + 1;
            buf.extend_from_slice(format!("\x1b[{row};{border_col}H").as_bytes());
            buf.extend_from_slice(b"\x1b[90m"); // dim gray
            buf.extend_from_slice("│".as_bytes());
            buf.extend_from_slice(b"\x1b[0m");

            // --- Right pane content ---
            let right_col = pane_cols + 2;
            buf.extend_from_slice(format!("\x1b[{row};{right_col}H").as_bytes());
            if row == 1 {
                buf.extend_from_slice(b"\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/dev\x1b[0m$ ");
                buf.extend_from_slice(b"vim src/main.rs");
            } else {
                // Syntax-highlighted code using 256 colors
                buf.extend_from_slice(b"\x1b[38;5;245m");
                buf.extend_from_slice(format!("{:>4} ", row).as_bytes());
                buf.extend_from_slice(b"\x1b[38;5;203m");
                buf.extend_from_slice(b"fn ");
                buf.extend_from_slice(b"\x1b[38;5;149m");
                buf.extend_from_slice(format!("func_{frame}").as_bytes());
                buf.extend_from_slice(b"\x1b[0m() {{");
            }
            buf.extend_from_slice(b"\x1b[K");
        }

        // tmux status bar at bottom
        buf.extend_from_slice(format!("\x1b[{};1H", rows + 1).as_bytes());
        buf.extend_from_slice(b"\x1b[7m"); // inverse
        buf.extend_from_slice(b" [0] bash  [1] vim* ");
        buf.extend(std::iter::repeat_n(b' ', 40));
        buf.extend_from_slice(b"\x1b[0m");

        // Show cursor
        buf.extend_from_slice(b"\x1b[?25h");
    }
    buf
}

/// 256-color heavy output — simulates bat/delta/syntax-highlighted code.
/// Dense \x1b[38;5;Nm sequences (3-param SGR) on every token.
fn make_256color_heavy(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 120);
    let colors: &[u8] = &[203, 149, 245, 39, 214, 170, 81, 222, 156, 197];
    for i in 0..n {
        // Line number in dim
        buf.extend_from_slice(b"\x1b[38;5;240m");
        buf.extend_from_slice(format!("{:>4} ", i + 1).as_bytes());
        // Keyword in one color
        let c1 = colors[i % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;5;{c1}m").as_bytes());
        buf.extend_from_slice(b"fn ");
        // Identifier in another color
        let c2 = colors[(i + 3) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;5;{c2}m").as_bytes());
        buf.extend_from_slice(b"process_data");
        // Punctuation in default
        buf.extend_from_slice(b"\x1b[0m(");
        // Type in another color
        let c3 = colors[(i + 5) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;5;{c3}m").as_bytes());
        buf.extend_from_slice(b"&[u8]");
        // Reset + closing
        buf.extend_from_slice(b"\x1b[0m) -> ");
        let c4 = colors[(i + 7) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;5;{c4}m").as_bytes());
        buf.extend_from_slice(b"Result");
        buf.extend_from_slice(b"\x1b[0m<()> {{\r\n");
    }
    buf
}

/// Truecolor heavy output — simulates modern syntax highlighters using
/// \x1b[38;2;R;G;Bm (5-param SGR) on every token.
fn make_truecolor_heavy(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 160);
    let colors: &[(u8, u8, u8)] = &[
        (255, 85, 85),   // red
        (85, 255, 85),   // green
        (85, 85, 255),   // blue
        (255, 255, 85),  // yellow
        (255, 85, 255),  // magenta
        (85, 255, 255),  // cyan
        (200, 160, 100), // tan
        (140, 200, 255), // light blue
    ];
    for i in 0..n {
        // Line number with truecolor
        let (r, g, b) = (100, 100, 100);
        buf.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
        buf.extend_from_slice(format!("{:>4} ", i + 1).as_bytes());
        // Keyword
        let (r, g, b) = colors[i % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
        buf.extend_from_slice(b"let ");
        // Variable
        let (r, g, b) = colors[(i + 2) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
        buf.extend_from_slice(b"result");
        // Operator
        buf.extend_from_slice(b"\x1b[0m = ");
        // Function call
        let (r, g, b) = colors[(i + 4) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
        buf.extend_from_slice(b"parse");
        buf.extend_from_slice(b"\x1b[0m(");
        // String literal
        let (r, g, b) = colors[(i + 6) % colors.len()];
        buf.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
        buf.extend_from_slice(b"\"hello world\"");
        buf.extend_from_slice(b"\x1b[0m);\r\n");
    }
    buf
}

/// Realistic terminal output: ls-like colored listing.
fn make_ls_output(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 100);
    for i in 0..n {
        // Permission bits
        buf.extend_from_slice(b"\x1b[0m-rw-r--r-- ");
        // User/group
        buf.extend_from_slice(b"user staff ");
        // Size (right-justified with cursor movement)
        buf.extend_from_slice(format!("{:>8} ", i * 1024).as_bytes());
        // Date
        buf.extend_from_slice(b"Mar  9 12:00 ");
        // Colored filename
        match i % 4 {
            0 => {
                buf.extend_from_slice(b"\x1b[1;34m");
                buf.extend_from_slice(format!("directory_{i}/").as_bytes());
            }
            1 => {
                buf.extend_from_slice(b"\x1b[1;32m");
                buf.extend_from_slice(format!("executable_{i}").as_bytes());
            }
            2 => {
                buf.extend_from_slice(b"\x1b[0;36m");
                buf.extend_from_slice(format!("symlink_{i} -> target").as_bytes());
            }
            _ => {
                buf.extend_from_slice(format!("file_{i}.txt").as_bytes());
            }
        }
        buf.extend_from_slice(b"\x1b[0m\r\n");
    }
    buf
}

// ---------------------------------------------------------------------------
// Parser benchmarks
// ---------------------------------------------------------------------------

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser");

    for &size in &[10_000] {
        // Pure ASCII throughput
        let ascii = make_ascii(size);
        group.throughput(Throughput::Bytes(ascii.len() as u64));
        group.bench_with_input(BenchmarkId::new("ascii", size), &ascii, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // Mixed ASCII + CSI
        let mixed = make_mixed_csi(size);
        group.throughput(Throughput::Bytes(mixed.len() as u64));
        group.bench_with_input(BenchmarkId::new("mixed_csi", size), &mixed, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // UTF-8 heavy
        let utf8 = make_utf8_heavy(size);
        group.throughput(Throughput::Bytes(utf8.len() as u64));
        group.bench_with_input(BenchmarkId::new("utf8_heavy", size), &utf8, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // Pure box-drawing (worst case for per-char UTF-8 overhead)
        let boxes = make_box_drawing(size);
        group.throughput(Throughput::Bytes(boxes.len() as u64));
        group.bench_with_input(BenchmarkId::new("box_drawing", size), &boxes, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // TUI-like: box borders + ASCII content
        let tui = make_tui_mixed(size);
        group.throughput(Throughput::Bytes(tui.len() as u64));
        group.bench_with_input(BenchmarkId::new("tui_mixed", size), &tui, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // Realistic ls output
        let ls = make_ls_output(size);
        group.throughput(Throughput::Bytes(ls.len() as u64));
        group.bench_with_input(BenchmarkId::new("ls_output", size), &ls, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(120, 40);
                let mut sb = Scrollback::new(1000);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });

        // SGR-dense: git diff --color style (frequent color changes per line)
        let diff = make_git_diff_color(size);
        group.throughput(Throughput::Bytes(diff.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("git_diff_color", size),
            &diff,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(120, 40);
                    let mut sb = Scrollback::new(1000);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );

        // Dense scroll: short lines, isolates scroll_up + scrollback throughput
        let scroll = make_dense_scroll(size);
        group.throughput(Throughput::Bytes(scroll.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("dense_scroll", size),
            &scroll,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(80, 24);
                    let mut sb = Scrollback::new(1000);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );

        // Claude Code CLI-style TUI: streaming text + spinner + status bar
        let claude = make_claude_code_tui(size);
        group.throughput(Throughput::Bytes(claude.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("claude_code_tui", size),
            &claude,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(120, 24);
                    let mut sb = Scrollback::new(1000);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );
    }

    // Full-screen redraw (outside the size loop — measured in frames, not lines)
    for &frames in &[100] {
        let redraw = make_fullscreen_redraw(frames, 120, 40);
        group.throughput(Throughput::Bytes(redraw.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("fullscreen_redraw_120x40", frames),
            &redraw,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(120, 40);
                    let mut sb = Scrollback::new(0);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );
    }

    // tmux 2-pane redraw (outside size loop — measured in frames)
    for &frames in &[100] {
        let tmux = make_tmux_pane_redraw(frames, 160, 40);
        group.throughput(Throughput::Bytes(tmux.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("tmux_pane_redraw_160x40", frames),
            &tmux,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(160, 40);
                    let mut sb = Scrollback::new(0);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );
    }

    // 256-color heavy (bat/delta style syntax highlighting)
    for &size in &[10_000] {
        let c256 = make_256color_heavy(size);
        group.throughput(Throughput::Bytes(c256.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("256color_heavy", size),
            &c256,
            |b, data| {
                b.iter(|| {
                    let mut grid = Grid::new(120, 40);
                    let mut sb = Scrollback::new(1000);
                    parse_bytes(&mut grid, &mut sb, data);
                    black_box(&grid);
                });
            },
        );
    }

    // Truecolor heavy (modern syntax highlighters)
    for &size in &[10_000] {
        let tc = make_truecolor_heavy(size);
        group.throughput(Throughput::Bytes(tc.len() as u64));
        group.bench_with_input(BenchmarkId::new("truecolor_heavy", size), &tc, |b, data| {
            b.iter(|| {
                let mut grid = Grid::new(120, 40);
                let mut sb = Scrollback::new(1000);
                parse_bytes(&mut grid, &mut sb, data);
                black_box(&grid);
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// SIMD scanner benchmarks
// ---------------------------------------------------------------------------

fn bench_simd(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_scanner");

    // Pure ASCII — best case (all printable)
    let ascii: Vec<u8> = (0..4096).map(|i| b' ' + (i % 95) as u8).collect();
    group.throughput(Throughput::Bytes(ascii.len() as u64));
    group.bench_function("pure_ascii_4k", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan(black_box(&ascii)));
        });
    });

    // Early escape — special byte at position 0
    let mut early_esc = ascii.clone();
    early_esc[0] = 0x1B;
    group.bench_function("early_escape", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan(black_box(&early_esc)));
        });
    });

    // Escape at position 63 — just before the 64-byte boundary
    let mut mid_esc = ascii.clone();
    mid_esc[63] = 0x1B;
    group.bench_function("escape_at_63", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan(black_box(&mid_esc)));
        });
    });

    // Control-heavy input (every 8th byte is a control)
    let control_heavy: Vec<u8> = (0..4096)
        .map(|i| {
            if i % 8 == 0 {
                0x0A
            } else {
                b'A' + (i % 26) as u8
            }
        })
        .collect();
    group.throughput(Throughput::Bytes(control_heavy.len() as u64));
    group.bench_function("control_every_8", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan(black_box(&control_heavy)));
        });
    });

    // scan_text: mixed ASCII + UTF-8 (no control chars, no DEL)
    let mixed_text: Vec<u8> = "ABCDabcd日本語テスト café über"
        .as_bytes()
        .iter()
        .cycle()
        .take(4096)
        .copied()
        .collect();
    group.throughput(Throughput::Bytes(mixed_text.len() as u64));
    group.bench_function("scan_text_mixed_4k", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan_text(black_box(&mixed_text)));
        });
    });

    // scan_text: pure high bytes (UTF-8 continuation-heavy)
    let utf8_text: Vec<u8> = "日本語テスト中文测试한국어αβγδ"
        .as_bytes()
        .iter()
        .cycle()
        .take(4096)
        .copied()
        .collect();
    group.throughput(Throughput::Bytes(utf8_text.len() as u64));
    group.bench_function("scan_text_utf8_4k", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan_text(black_box(&utf8_text)));
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Grid benchmarks
// ---------------------------------------------------------------------------

fn bench_grid(c: &mut Criterion) {
    let mut group = c.benchmark_group("grid");

    // Grid construction
    for &(cols, rows) in &[(80, 24), (200, 50)] {
        group.bench_function(format!("new_{cols}x{rows}"), |b| {
            b.iter(|| {
                black_box(Grid::new(cols, rows));
            });
        });
    }

    // write_char throughput — fill entire grid
    for &(cols, rows) in &[(80, 24)] {
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function(format!("write_char_{cols}x{rows}"), |b| {
            b.iter(|| {
                let mut grid = Grid::new(cols, rows);
                for _ in 0..total {
                    grid.write_char('A', 0, 0);
                }
                black_box(&grid);
            });
        });
    }

    // write_ascii_run throughput — bulk write path (the hot path for ASCII content)
    for &(cols, rows) in &[(80, 24)] {
        let line: Vec<u8> = (0..cols).map(|i| b' ' + (i % 95) as u8).collect();
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function(format!("write_ascii_run_{cols}x{rows}"), |b| {
            b.iter(|| {
                let mut grid = Grid::new(cols, rows);
                for _ in 0..rows {
                    grid.write_ascii_run(&line);
                    // Newline: CR + LF
                    grid.cursor_col = 0;
                    grid.cursor_pending_wrap = false;
                    if grid.cursor_row < grid.rows - 1 {
                        grid.cursor_row += 1;
                    }
                }
                black_box(&grid);
            });
        });
    }

    // scroll_up — bulk scrolling
    for &n in &[1, 10] {
        group.bench_function(format!("scroll_up_{n}_80x24"), |b| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(1000);
            // Fill grid first
            let data = make_ascii(100);
            parse_bytes(&mut grid, &mut sb, &data);
            b.iter(|| {
                grid.scroll_up_into(n, Some(&mut sb));
                black_box(&grid);
            });
        });
    }

    // scroll_down
    group.bench_function("scroll_down_1_80x24", |b| {
        let mut grid = Grid::new(80, 24);
        let mut sb = Scrollback::new(0);
        let data = make_ascii(100);
        parse_bytes(&mut grid, &mut sb, &data);
        b.iter(|| {
            grid.scroll_down(1);
            black_box(&grid);
        });
    });

    // clear_rows
    group.bench_function("clear_rows_full_80x24", |b| {
        let mut grid = Grid::new(80, 24);
        b.iter(|| {
            grid.clear_rows(0, 24);
            black_box(&grid);
        });
    });

    // resize
    group.bench_function("resize_80x24_to_120x40", |b| {
        b.iter(|| {
            let mut grid = Grid::new(80, 24);
            grid.resize(120, 40);
            black_box(&grid);
        });
    });

    group.bench_function("resize_200x50_to_80x24", |b| {
        b.iter(|| {
            let mut grid = Grid::new(200, 50);
            grid.resize(80, 24);
            black_box(&grid);
        });
    });

    // alt screen enter/exit
    group.bench_function("alt_screen_roundtrip_80x24", |b| {
        let mut grid = Grid::new(80, 24);
        let mut sb = Scrollback::new(0);
        let data = make_ascii(50);
        parse_bytes(&mut grid, &mut sb, &data);
        b.iter(|| {
            grid.enter_alt_screen();
            grid.exit_alt_screen();
            black_box(&grid);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Pipeline benchmarks — isolate individual stages of parse → attr → write → dirty
// ---------------------------------------------------------------------------

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline");

    // --- Stage 1: Cell write cost (grid.write_ascii_run) ---

    // Uniform attributes: same attr for entire run. Best case for the write loop
    // since attr fields are identical every iteration.
    {
        let cols: u16 = 120;
        let rows: u16 = 40;
        let line: Vec<u8> = (0..cols).map(|i| b' ' + (i % 95) as u8).collect();
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function("write_uniform_attr_120x40", |b| {
            b.iter(|| {
                let mut grid = Grid::new(cols, rows);
                grid.attr.fg_index = 2; // green
                for _ in 0..rows {
                    grid.write_ascii_run(&line);
                    grid.cursor_col = 0;
                    grid.cursor_pending_wrap = false;
                    if grid.cursor_row < grid.rows - 1 {
                        grid.cursor_row += 1;
                    }
                }
                black_box(&grid);
            });
        });
    }

    // Alternating attributes: change fg color every 10 chars.
    // Shows cost of attr-change overhead in the write path.
    {
        let cols: u16 = 120;
        let rows: u16 = 40;
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function("write_alternating_attr_120x40", |b| {
            b.iter(|| {
                let mut grid = Grid::new(cols, rows);
                let chunk: Vec<u8> = (0..10).map(|i| b'A' + i).collect();
                for row in 0..rows {
                    for seg in 0..12 {
                        grid.attr.fg_index = (((row * 12 + seg as u16) % 8) + 30) as u8;
                        grid.write_ascii_run(&chunk);
                    }
                    grid.cursor_col = 0;
                    grid.cursor_pending_wrap = false;
                    if grid.cursor_row < grid.rows - 1 {
                        grid.cursor_row += 1;
                    }
                }
                black_box(&grid);
            });
        });
    }

    // --- Stage 2: SGR dispatch cost ---

    // Measure sgr_single throughput for basic fg colors (30-37).
    {
        let n = 100_000u64;
        group.throughput(Throughput::Elements(n));
        group.bench_function("sgr_single_fg_100k", |b| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            b.iter(|| {
                let mut perf = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                for i in 0..n {
                    perf.sgr_single(black_box(30 + (i % 8) as u16));
                }
                black_box(perf.grid.attr);
            });
        });
    }

    // Measure sgr() with multi-param slice (the generic path for \x1b[1;32m etc).
    {
        let n = 100_000u64;
        group.throughput(Throughput::Elements(n));
        group.bench_function("sgr_multi_param_100k", |b| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            let params_list: Vec<[u16; 2]> = (0..8).map(|i| [1, 30 + i]).collect();
            b.iter(|| {
                let mut perf = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                for i in 0..n {
                    perf.sgr(black_box(&params_list[(i % 8) as usize]));
                }
                black_box(perf.grid.attr);
            });
        });
    }

    // Measure color_256 throughput (the dedicated path).
    {
        let n = 100_000u64;
        group.throughput(Throughput::Elements(n));
        group.bench_function("color_256_100k", |b| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            b.iter(|| {
                let mut perf = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                for i in 0..n {
                    perf.color_256(black_box(true), black_box((i % 256) as u16));
                }
                black_box(perf.grid.attr);
            });
        });
    }

    // Measure color_rgb throughput (the dedicated path).
    {
        let n = 100_000u64;
        group.throughput(Throughput::Elements(n));
        group.bench_function("color_rgb_100k", |b| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            b.iter(|| {
                let mut perf = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                for i in 0..n {
                    let c = (i % 256) as u16;
                    perf.color_rgb(
                        black_box(true),
                        black_box(c),
                        black_box(128),
                        black_box(255 - c),
                    );
                }
                black_box(perf.grid.attr);
            });
        });
    }

    // --- Stage 3: SGR + write cycle (the real inner loop) ---

    // The common TUI pattern: set color, write a few chars, reset.
    // Measures the combined cost of attribute dispatch + cell writes.
    {
        let iterations = 1_000u64;
        let text = b"Hello, world!"; // 13 chars
        // Per iteration: sgr_single(32) + write_ascii_run(13) + sgr_reset
        let total_chars = iterations * text.len() as u64;
        group.throughput(Throughput::Elements(total_chars));
        group.bench_function("sgr_write_reset_cycle_1k", |b| {
            b.iter(|| {
                let mut grid = Grid::new(120, 40);
                let mut sb = Scrollback::new(0);
                let mut perf = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                for i in 0..iterations {
                    perf.sgr_single(30 + (i % 8) as u16);
                    perf.grid.write_ascii_run(text);
                    perf.sgr_reset();
                    // newline every ~9 segments
                    if i % 9 == 8 {
                        perf.grid.cursor_col = 0;
                        perf.grid.cursor_pending_wrap = false;
                        if perf.grid.cursor_row < perf.grid.rows - 1 {
                            perf.grid.cursor_row += 1;
                        }
                    }
                }
                black_box(&perf.grid);
            });
        });
    }

    // --- Stage 4: Cell → CellData conversion (render upload simulation) ---

    // Simulates the CPU side of render_frame: iterating dirty rows and
    // converting Cell to a packed GPU struct.
    // CellData isn't in lib.rs, so we replicate the conversion inline.
    {
        let cols: u16 = 120;
        let rows: u16 = 40;
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function("cell_to_gpu_all_dirty_120x40", |b| {
            // Pre-fill grid with varied content
            let mut grid = Grid::new(cols, rows);
            let mut sb = Scrollback::new(0);
            let data = make_tmux_pane_redraw(1, cols, rows);
            parse_bytes(&mut grid, &mut sb, &data);
            grid.mark_all_dirty();

            // Destination buffer (simulates Metal shared buffer)
            let mut gpu_buf = vec![0u64; total as usize]; // 8 bytes per cell

            b.iter(|| {
                let dst = gpu_buf.as_mut_ptr() as *mut u8;
                for row in 0..rows {
                    if !grid.dirty[row as usize] {
                        continue;
                    }
                    let src_row = grid.row_slice(row);
                    let offset = row as usize * cols as usize * 8;
                    // Cell IS the GPU format — straight memcpy per row
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            src_row.as_ptr() as *const u8,
                            dst.add(offset),
                            src_row.len() * 8,
                        );
                    }
                }
                black_box(&gpu_buf);
            });
        });

        // Same but only 25% of rows dirty (typical partial update)
        let dirty_cells = (rows / 4 + 1) as u64 * cols as u64;
        group.throughput(Throughput::Elements(dirty_cells));
        group.bench_function("cell_to_gpu_25pct_dirty_120x40", |b| {
            let mut grid = Grid::new(cols, rows);
            let mut sb = Scrollback::new(0);
            let data = make_tmux_pane_redraw(1, cols, rows);
            parse_bytes(&mut grid, &mut sb, &data);
            // Mark only every 4th row dirty
            grid.clear_dirty();
            for row in (0..rows).step_by(4) {
                grid.mark_dirty(row);
            }

            let mut gpu_buf = vec![0u64; total as usize];

            b.iter(|| {
                let dst = gpu_buf.as_mut_ptr() as *mut u8;
                for row in 0..rows {
                    if !grid.dirty[row as usize] {
                        continue;
                    }
                    let src_row = grid.row_slice(row);
                    let offset = row as usize * cols as usize * 8;
                    // Cell IS the GPU format — straight memcpy per row
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            src_row.as_ptr() as *const u8,
                            dst.add(offset),
                            src_row.len() * 8,
                        );
                    }
                }
                black_box(&gpu_buf);
            });
        });
    }

    // --- Stage 5: mark_dirty overhead ---
    {
        let rows: u16 = 40;
        group.bench_function("mark_dirty_scattered_10_of_40", |b| {
            let mut grid = Grid::new(120, rows);
            b.iter(|| {
                for row in (0..rows).step_by(4) {
                    grid.mark_dirty(row);
                }
                black_box(&grid.dirty);
                grid.clear_dirty();
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Scrollback benchmarks
// ---------------------------------------------------------------------------

fn bench_scrollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrollback");
    let row: Vec<Cell> = vec![Cell::default(); 80];

    // Push into growing ring buffer
    group.bench_function("push_1k_rows_cap_1k", |b| {
        b.iter(|| {
            let mut sb = Scrollback::new(1000);
            for _ in 0..1000 {
                sb.push_slice(&row);
            }
            black_box(&sb);
        });
    });

    // Steady-state push (ring buffer full, reusing allocations)
    group.bench_function("push_steady_state", |b| {
        let mut sb = Scrollback::new(1000);
        // Fill it up first
        for _ in 0..1000 {
            sb.push_slice(&row);
        }
        b.iter(|| {
            sb.push_slice(&row);
            black_box(&sb);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// End-to-end: chunked parsing (simulates PTY read sizes)
// ---------------------------------------------------------------------------

fn bench_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end");

    let ls_data = make_ls_output(10_000);
    group.throughput(Throughput::Bytes(ls_data.len() as u64));

    // Single large parse
    group.bench_function("ls_10k_single_parse", |b| {
        b.iter(|| {
            let mut grid = Grid::new(120, 40);
            let mut sb = Scrollback::new(10_000);
            parse_bytes(&mut grid, &mut sb, &ls_data);
            black_box(&grid);
        });
    });

    // Chunked parse (4 KiB chunks, like real PTY reads)
    group.bench_function("ls_10k_4k_chunks", |b| {
        b.iter(|| {
            let mut grid = Grid::new(120, 40);
            let mut sb = Scrollback::new(10_000);
            let mut parser = Parser::new();
            for chunk in ls_data.chunks(4096) {
                let mut performer = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                parser.parse(chunk, &mut performer);
            }
            black_box(&grid);
        });
    });

    // Chunked parse with small chunks (simulates slow/fragmented reads)
    group.bench_function("ls_10k_64b_chunks", |b| {
        b.iter(|| {
            let mut grid = Grid::new(120, 40);
            let mut sb = Scrollback::new(10_000);
            let mut parser = Parser::new();
            for chunk in ls_data.chunks(64) {
                let mut performer = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                parser.parse(chunk, &mut performer);
            }
            black_box(&grid);
        });
    });

    // UTF-8 heavy content in 4K chunks — exercises Utf8Assembler cross-chunk path.
    // Multi-byte chars will be split at arbitrary 4096-byte boundaries, forcing the
    // assembler to buffer partial sequences across parse() calls.
    let utf8_data = make_utf8_heavy(10_000);
    group.throughput(Throughput::Bytes(utf8_data.len() as u64));
    group.bench_function("utf8_10k_4k_chunks", |b| {
        b.iter(|| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(1000);
            let mut parser = Parser::new();
            for chunk in utf8_data.chunks(4096) {
                let mut performer = BenchPerformer {
                    grid: &mut grid,
                    scrollback: &mut sb,
                };
                parser.parse(chunk, &mut performer);
            }
            black_box(&grid);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Allocation audit
// ---------------------------------------------------------------------------

fn bench_alloc_audit(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_audit");

    let ascii_1k = make_ascii(1_000);
    let ls_1k = make_ls_output(1_000);

    // One-shot allocation reports
    {
        eprintln!();
        eprintln!("  -- allocation audit --");

        // Grid construction
        let stats = measure_allocs(|| {
            black_box(Grid::new(80, 24));
        });
        eprintln!("  [alloc] Grid::new(80x24):          {stats}");

        let stats = measure_allocs(|| {
            black_box(Grid::new(200, 50));
        });
        eprintln!("  [alloc] Grid::new(200x50):         {stats}");

        // Parse 1k ASCII lines
        let stats = measure_allocs(|| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            parse_bytes(&mut grid, &mut sb, &ascii_1k);
            black_box(&grid);
        });
        eprintln!("  [alloc] parse_ascii_1k:            {stats}");

        // Parse 1k ls output (with scrollback)
        let stats = measure_allocs(|| {
            let mut grid = Grid::new(120, 40);
            let mut sb = Scrollback::new(1000);
            parse_bytes(&mut grid, &mut sb, &ls_1k);
            black_box(&grid);
        });
        eprintln!("  [alloc] parse_ls_1k_with_sb:       {stats}");

        // write_char fill (should be 0 — grid pre-allocated)
        let mut grid = Grid::new(80, 24);
        let stats = measure_allocs(|| {
            for _ in 0..80 * 24 {
                grid.write_char('X', 0, 0);
            }
        });
        eprintln!("  [alloc] write_char_fill_80x24:     {stats}");

        // scroll_up (should be 0 for grid, may alloc scrollback)
        let stats = measure_allocs(|| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(100);
            for _ in 0..100 {
                grid.scroll_up_into(1, Some(&mut sb));
            }
            black_box(&grid);
        });
        eprintln!("  [alloc] scroll_up_100x:            {stats}");

        // Scrollback steady state (should be 0 — reuses allocations)
        let mut sb = Scrollback::new(100);
        let row: Vec<Cell> = vec![Cell::default(); 80];
        for _ in 0..100 {
            sb.push_slice(&row);
        }
        let stats = measure_allocs(|| {
            for _ in 0..100 {
                sb.push_slice(&row);
            }
        });
        eprintln!("  [alloc] scrollback_steady_100:     {stats}");

        // Resize
        let stats = measure_allocs(|| {
            let mut grid = Grid::new(80, 24);
            grid.resize(120, 40);
            black_box(&grid);
        });
        eprintln!("  [alloc] resize_80x24_to_120x40:    {stats}");

        eprintln!();
    }

    // Criterion timing for key allocation-sensitive operations
    group.bench_function("grid_new_80x24", |b| {
        b.iter(|| {
            black_box(Grid::new(80, 24));
        });
    });

    group.bench_function("parse_ascii_1k", |b| {
        b.iter(|| {
            let mut grid = Grid::new(80, 24);
            let mut sb = Scrollback::new(0);
            parse_bytes(&mut grid, &mut sb, &ascii_1k);
            black_box(&grid);
        });
    });

    group.bench_function("parse_ls_1k_with_sb", |b| {
        b.iter(|| {
            let mut grid = Grid::new(120, 40);
            let mut sb = Scrollback::new(1000);
            parse_bytes(&mut grid, &mut sb, &ls_1k);
            black_box(&grid);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// TUI redraw benchmarks — measures cost of re-rendering over existing content.
// This is the dominant pattern for apps like htop/top/vim where most of the
// screen is static between frames.
// ---------------------------------------------------------------------------

/// Generate a single full-screen frame of htop-like content.
/// Deterministic given a frame index, so frame 0 == frame 0.
fn make_htop_frame(frame: usize, cols: u16, rows: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(rows as usize * (cols as usize + 30));
    buf.extend_from_slice(b"\x1b[?25l"); // hide cursor

    for row in 1..=rows {
        buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        if row == 1 {
            // CPU bar — changes every frame
            buf.extend_from_slice(b"\x1b[7m"); // inverse
            let pct = (frame * 7 + 13) % 100;
            let filled = (pct as usize * cols as usize) / 100;
            buf.extend_from_slice(format!(" CPU [{:>3}%] ", pct).as_bytes());
            for j in 12..cols as usize {
                if j < filled {
                    buf.push(b'|');
                } else {
                    buf.push(b' ');
                }
            }
            buf.extend_from_slice(b"\x1b[0m");
        } else if row == 2 {
            // Memory bar — changes every frame
            buf.extend_from_slice(b"\x1b[7m");
            let pct = (frame * 3 + 42) % 100;
            buf.extend_from_slice(format!(" Mem [{:>3}%] ", pct).as_bytes());
            for j in 12..cols as usize {
                if j < (pct as usize * cols as usize) / 100 {
                    buf.push(b'#');
                } else {
                    buf.push(b' ');
                }
            }
            buf.extend_from_slice(b"\x1b[0m");
        } else if row == rows {
            // Status bar — static
            buf.extend_from_slice(b"\x1b[7m");
            buf.extend_from_slice(b" F1Help F2Setup F3Search F9Kill F10Quit");
            for _ in 38..cols {
                buf.push(b' ');
            }
            buf.extend_from_slice(b"\x1b[0m");
        } else if row == 3 {
            // Column headers — static
            buf.extend_from_slice(b"\x1b[1;32m");
            buf.extend_from_slice(
                format!(
                    "  PID {:>7} {:>4} {:>4} {:>9} {:>9}  {:<20}",
                    "USER", "PR", "NI", "VIRT", "RES", "COMMAND"
                )
                .as_bytes(),
            );
            buf.extend_from_slice(b"\x1b[0m");
        } else {
            // Process rows — static (only a few change per frame)
            let pid = (row as usize - 3) * 100 + 1;
            buf.extend_from_slice(format!("\x1b[0m{pid:>5} ").as_bytes());
            buf.extend_from_slice(b"root     20    0 ");
            buf.extend_from_slice(format!("{:>9} {:>9}  ", pid * 1024, pid * 512).as_bytes());
            // Process name — static
            let names = [
                "systemd",
                "kthreadd",
                "rcu_sched",
                "migration",
                "watchdog",
                "netns",
                "kworker",
                "kdevtmpfs",
                "inet_frag",
                "kauditd",
            ];
            let name = names[(row as usize - 4) % names.len()];
            buf.extend_from_slice(name.as_bytes());
        }
        buf.extend_from_slice(b"\x1b[K"); // clear to EOL
    }
    buf.extend_from_slice(b"\x1b[?25h"); // show cursor
    buf
}

fn bench_tui_redraw(c: &mut Criterion) {
    let mut group = c.benchmark_group("tui_redraw");
    let cols: u16 = 120;
    let rows: u16 = 40;

    // --- 100% static: re-render the exact same frame ---
    // This is the best case for the skip optimization.
    {
        let frame_data = make_htop_frame(0, cols, rows);
        group.throughput(Throughput::Bytes(frame_data.len() as u64));
        group.bench_function("htop_100pct_static_120x40", |b| {
            // Setup: parse the frame once
            let mut grid = Grid::new(cols, rows);
            let mut sb = Scrollback::new(0);
            parse_bytes(&mut grid, &mut sb, &frame_data);
            grid.clear_dirty();

            b.iter(|| {
                // Re-render the exact same content
                parse_bytes(&mut grid, &mut sb, &frame_data);
                black_box(&grid);
                grid.clear_dirty();
            });
        });
    }

    // --- ~5% changed: only CPU/mem bars update (rows 1-2 of 40) ---
    // Typical htop between refreshes: bars animate, process list is static.
    {
        let setup_frame = make_htop_frame(0, cols, rows);
        // Generate multiple "next" frames for the bench loop
        let frames: Vec<Vec<u8>> = (1..=30).map(|f| make_htop_frame(f, cols, rows)).collect();
        group.throughput(Throughput::Bytes(setup_frame.len() as u64));
        group.bench_function("htop_5pct_changed_120x40", |b| {
            let mut grid = Grid::new(cols, rows);
            let mut sb = Scrollback::new(0);
            parse_bytes(&mut grid, &mut sb, &setup_frame);
            grid.clear_dirty();

            let mut frame_idx = 0;
            b.iter(|| {
                parse_bytes(&mut grid, &mut sb, &frames[frame_idx % frames.len()]);
                black_box(&grid);
                grid.clear_dirty();
                frame_idx += 1;
            });
        });
    }

    // --- 100% changed: every cell different (worst case — no skipping) ---
    // Baseline to show cost when the optimization can't help.
    {
        let frame0 = make_fullscreen_redraw(1, cols, rows);
        let frame1 = make_fullscreen_redraw(2, cols, rows); // different content
        group.throughput(Throughput::Bytes(frame0.len() as u64));
        group.bench_function("fullscreen_100pct_changed_120x40", |b| {
            let mut grid = Grid::new(cols, rows);
            let mut sb = Scrollback::new(0);
            parse_bytes(&mut grid, &mut sb, &frame0);
            grid.clear_dirty();

            b.iter(|| {
                parse_bytes(&mut grid, &mut sb, &frame1);
                black_box(&grid);
                // Reset for next iteration so frame1 is always "different"
                parse_bytes(&mut grid, &mut sb, &frame0);
                grid.clear_dirty();
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Slow-path benchmarks — operations that are infrequent but potentially expensive.
// insert/delete chars, wide characters, non-BMP scrolling, vim-style editing.
// ---------------------------------------------------------------------------

/// Vim-like editing: insert mode typing with cursor movements and line insertions.
/// Exercises insert_chars, delete_chars, and CSI L/M (insert/delete lines).
fn make_vim_insert_mode(n: usize, cols: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 60);
    for i in 0..n {
        match i % 5 {
            0 => {
                // Position cursor mid-line, insert characters
                let col = (i % (cols as usize - 10)) + 5;
                buf.extend_from_slice(format!("\x1b[1;{col}H").as_bytes());
                buf.extend_from_slice(b"\x1b[4@"); // ICH: insert 4 blanks
                buf.extend_from_slice(b"edit"); // type into them
            }
            1 => {
                // Delete characters at cursor
                let col = (i % (cols as usize - 10)) + 5;
                buf.extend_from_slice(format!("\x1b[1;{col}H").as_bytes());
                buf.extend_from_slice(b"\x1b[3P"); // DCH: delete 3 chars
            }
            2 => {
                // Erase characters at cursor
                buf.extend_from_slice(b"\x1b[10X"); // ECH: erase 10 chars
            }
            3 => {
                // Insert a line (scroll region content down)
                let row = (i % 20) + 2;
                buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
                buf.extend_from_slice(b"\x1b[L"); // IL: insert 1 line
                buf.extend_from_slice(b"new line content here");
            }
            _ => {
                // Delete a line (scroll region content up)
                let row = (i % 20) + 2;
                buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
                buf.extend_from_slice(b"\x1b[M"); // DL: delete 1 line
            }
        }
    }
    buf
}

/// Heavy wide-character output — CJK/emoji filling the screen.
/// Exercises write_wide_char, including EOL wrapping (wide char at last col).
fn make_wide_char_heavy(n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n * 120);
    let cjk = "你好世界テスト한국語中文";
    let emoji = "🚀🎉🔥💻🌍";
    for i in 0..n {
        if i % 3 == 0 {
            // CJK line
            buf.extend_from_slice(cjk.as_bytes());
            buf.extend_from_slice(cjk.as_bytes());
            buf.extend_from_slice(cjk.as_bytes());
        } else if i % 3 == 1 {
            // Emoji line (non-BMP, triggers chars[] path)
            buf.extend_from_slice(emoji.as_bytes());
            buf.extend_from_slice(emoji.as_bytes());
            buf.extend_from_slice(emoji.as_bytes());
            buf.extend_from_slice(emoji.as_bytes());
        } else {
            // Mixed: CJK + ASCII + emoji
            buf.extend_from_slice("│ ".as_bytes());
            buf.extend_from_slice(cjk.as_bytes());
            buf.extend_from_slice(b" status: ");
            buf.extend_from_slice(emoji.as_bytes());
        }
        buf.push(b'\n');
    }
    buf
}

fn bench_slow_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path");

    // --- Insert/delete chars (ICH/DCH — vim insert mode, text editors) ---
    {
        let data = make_vim_insert_mode(1_000, 80);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function("vim_insert_mode_1k_ops", |b| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(0);
                // Pre-fill grid so insert/delete have content to shift
                let fill = make_ascii(50);
                parse_bytes(&mut grid, &mut sb, &fill);
                grid.cursor_row = 0;
                grid.cursor_col = 0;
                parse_bytes(&mut grid, &mut sb, &data);
                black_box(&grid);
            });
        });
    }

    // --- Isolated insert_chars throughput ---
    group.bench_function("insert_chars_mid_row_80", |b| {
        let mut grid = Grid::new(80, 24);
        let fill = make_ascii(50);
        let mut sb = Scrollback::new(0);
        parse_bytes(&mut grid, &mut sb, &fill);
        b.iter(|| {
            grid.cursor_col = 40; // mid-row
            grid.cursor_row = 0;
            grid.insert_chars(10);
            black_box(&grid);
        });
    });

    // --- Isolated delete_chars throughput ---
    group.bench_function("delete_chars_mid_row_80", |b| {
        let mut grid = Grid::new(80, 24);
        let fill = make_ascii(50);
        let mut sb = Scrollback::new(0);
        parse_bytes(&mut grid, &mut sb, &fill);
        b.iter(|| {
            grid.cursor_col = 40;
            grid.cursor_row = 0;
            grid.delete_chars(10);
            black_box(&grid);
        });
    });

    // --- Wide character (CJK) throughput ---
    {
        let data = make_wide_char_heavy(1_000);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function("wide_char_cjk_1k_lines", |b| {
            b.iter(|| {
                let mut grid = Grid::new(80, 24);
                let mut sb = Scrollback::new(1000);
                parse_bytes(&mut grid, &mut sb, &data);
                black_box(&grid);
            });
        });
    }

    // --- Wide char at large terminal (emoji-heavy Slack/Discord output) ---
    {
        let data = make_wide_char_heavy(10_000);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function("wide_char_emoji_10k_lines", |b| {
            b.iter(|| {
                let mut grid = Grid::new(120, 40);
                let mut sb = Scrollback::new(10_000);
                parse_bytes(&mut grid, &mut sb, &data);
                black_box(&grid);
            });
        });
    }

    // --- Scroll with non-BMP content (has_non_bmp=true doubles copy_within) ---
    group.bench_function("scroll_up_with_emoji_80x24", |b| {
        let mut grid = Grid::new(80, 24);
        let mut sb = Scrollback::new(1000);
        // Fill with emoji to set has_non_bmp=true
        let emoji_data = make_wide_char_heavy(100);
        parse_bytes(&mut grid, &mut sb, &emoji_data);
        b.iter(|| {
            grid.scroll_up_into(1, Some(&mut sb));
            black_box(&grid);
        });
    });

    // --- Insert/delete lines (IL/DL — vim scrolling, tmux pane resize) ---
    group.bench_function("insert_delete_lines_80x24", |b| {
        let mut grid = Grid::new(80, 24);
        let mut sb = Scrollback::new(0);
        let fill = make_ascii(50);
        parse_bytes(&mut grid, &mut sb, &fill);
        b.iter(|| {
            // Insert at row 10, delete at row 5 — exercises scroll region ops
            grid.cursor_row = 10;
            let old_top = grid.scroll_top;
            grid.scroll_top = grid.cursor_row;
            grid.scroll_down(1);
            grid.scroll_top = old_top;

            grid.cursor_row = 5;
            let old_top2 = grid.scroll_top;
            grid.scroll_top = grid.cursor_row;
            grid.scroll_up(1);
            grid.scroll_top = old_top2;

            black_box(&grid);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(15)
        .warm_up_time(std::time::Duration::from_millis(500))
        .measurement_time(std::time::Duration::from_secs(1));
    targets =
        bench_parser,
        bench_simd,
        bench_grid,
        bench_pipeline,
        bench_scrollback,
        bench_end_to_end,
        bench_alloc_audit,
        bench_tui_redraw,
        bench_slow_paths,
}
criterion_main!(benches);
