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

    for &size in &[1_000, 10_000] {
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
        .map(|i| if i % 8 == 0 { 0x0A } else { b'A' + (i % 26) as u8 })
        .collect();
    group.throughput(Throughput::Bytes(control_heavy.len() as u64));
    group.bench_function("control_every_8", |b| {
        b.iter(|| {
            black_box(SimdScanner::scan(black_box(&control_heavy)));
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
    for &(cols, rows) in &[(80, 24), (120, 40), (200, 50)] {
        group.bench_function(format!("new_{cols}x{rows}"), |b| {
            b.iter(|| {
                black_box(Grid::new(cols, rows));
            });
        });
    }

    // write_char throughput — fill entire grid
    for &(cols, rows) in &[(80, 24), (200, 50)] {
        let total = cols as u64 * rows as u64;
        group.throughput(Throughput::Elements(total));
        group.bench_function(format!("write_char_{cols}x{rows}"), |b| {
            b.iter(|| {
                let mut grid = Grid::new(cols, rows);
                for _ in 0..total {
                    grid.write_char('A');
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
                grid.write_char('X');
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

criterion_group!(
    benches,
    bench_parser,
    bench_simd,
    bench_grid,
    bench_scrollback,
    bench_end_to_end,
    bench_alloc_audit,
);
criterion_main!(benches);
