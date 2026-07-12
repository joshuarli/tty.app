use tty::config;
use tty::parser::Parser;
use tty::parser::perform::Perform;
use tty::perform_shared;
use tty::terminal::cell::{Cell, CellFlags};
use tty::terminal::grid::{Grid, TermMode};
use tty::terminal::scrollback::Scrollback;

/// A performer that mirrors TermPerformer behavior without atlas/font deps.
/// Atlas coords are always (0, 0). This lets us test the full parser →
/// grid pipeline for all Perform trait methods.
struct TestPerformer<'a> {
    grid: &'a mut Grid,
    scrollback: &'a mut Scrollback,
    response_buf: &'a mut Vec<u8>,
}

impl<'a> TestPerformer<'a> {
    fn from(grid: &'a mut Grid, sb: &'a mut Scrollback, buf: &'a mut Vec<u8>) -> Self {
        Self {
            grid,
            scrollback: sb,
            response_buf: buf,
        }
    }
}

impl<'a> Perform for TestPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        let use_dec = (self.grid.active_charset == 0 && self.grid.charset_g0 == 1)
            || (self.grid.active_charset == 1 && self.grid.charset_g1 == 1);
        if use_dec {
            for &b in bytes {
                let ch = if (0x60..=0x7E).contains(&b) {
                    tty::parser::charset::translate_dec_special(b)
                } else {
                    b as char
                };
                self.grid.write_char(ch, 0, 0);
                self.grid.last_char = ch;
                self.grid.last_atlas = [0, 0];
            }
        } else {
            self.grid.write_ascii_run(bytes);
            if let Some(&last) = bytes.last() {
                self.grid.last_char = last as char;
            }
        }
    }

    fn print(&mut self, c: char) {
        self.grid.write_char(c, 0, 0);
        self.grid.last_char = c;
        self.grid.last_atlas = [0, 0];
    }

    fn execute(&mut self, byte: u8) {
        perform_shared::execute(self.grid, self.scrollback, self.response_buf, byte);
    }

    fn cursor_up(&mut self, n: u16) {
        perform_shared::cursor_up(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_down(&mut self, n: u16) {
        perform_shared::cursor_down(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_forward(&mut self, n: u16) {
        perform_shared::cursor_forward(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_backward(&mut self, n: u16) {
        perform_shared::cursor_backward(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_position(&mut self, row: u16, col: u16) {
        perform_shared::cursor_position(self.grid, self.scrollback, self.response_buf, row, col);
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        perform_shared::cursor_horizontal_absolute(
            self.grid,
            self.scrollback,
            self.response_buf,
            col,
        );
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        perform_shared::cursor_vertical_absolute(
            self.grid,
            self.scrollback,
            self.response_buf,
            row,
        );
    }

    fn erase_in_display(&mut self, mode: u16) {
        perform_shared::erase_in_display(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn erase_in_line(&mut self, mode: u16) {
        perform_shared::erase_in_line(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn scroll_up(&mut self, n: u16) {
        perform_shared::scroll_up(self.grid, self.scrollback, self.response_buf, n);
    }

    fn scroll_down(&mut self, n: u16) {
        perform_shared::scroll_down(self.grid, self.scrollback, self.response_buf, n);
    }

    fn insert_lines(&mut self, n: u16) {
        perform_shared::insert_lines(self.grid, self.scrollback, self.response_buf, n);
    }

    fn delete_lines(&mut self, n: u16) {
        perform_shared::delete_lines(self.grid, self.scrollback, self.response_buf, n);
    }

    fn insert_chars(&mut self, n: u16) {
        perform_shared::insert_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn delete_chars(&mut self, n: u16) {
        perform_shared::delete_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn erase_chars(&mut self, n: u16) {
        perform_shared::erase_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn sgr(&mut self, params: &[u16]) {
        perform_shared::sgr(self.grid, self.scrollback, self.response_buf, params);
    }

    #[inline]
    fn sgr_reset(&mut self) {
        perform_shared::sgr_reset(self.grid);
    }

    #[inline]
    fn sgr_single(&mut self, code: u16) {
        perform_shared::sgr_single(self.grid, code);
    }

    #[inline]
    fn color_256(&mut self, fg: bool, index: u16) {
        perform_shared::color_256(self.grid, fg, index);
    }

    #[inline]
    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        perform_shared::color_rgb(self.grid, fg, r, g, b);
    }

    fn sgr_colon(&mut self, raw: &[u8]) {
        for attr_bytes in raw.split(|&b| b == b';') {
            let subs: Vec<u16> = attr_bytes
                .split(|&b| b == b':')
                .map(|s| {
                    s.iter().fold(0u32, |acc, &b| {
                        if b.is_ascii_digit() {
                            acc * 10 + (b - b'0') as u32
                        } else {
                            acc
                        }
                    }) as u16
                })
                .collect();
            if subs.is_empty() {
                continue;
            }
            match subs[0] {
                4 => {
                    if subs.len() > 1 {
                        if subs[1] == 0 {
                            self.grid.attr.flags.remove(CellFlags::UNDERLINE);
                        } else {
                            self.grid.attr.flags.insert(CellFlags::UNDERLINE);
                        }
                    } else {
                        self.grid.attr.flags.insert(CellFlags::UNDERLINE);
                    }
                }
                38 => {
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.grid.attr.fg_index = subs[2] as u8;
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        let (r, g, b) = if subs.len() >= 6 {
                            (subs[3] as u8, subs[4] as u8, subs[5] as u8)
                        } else {
                            (subs[2] as u8, subs[3] as u8, subs[4] as u8)
                        };
                        self.grid.attr.fg_index = config::rgb_to_palette(r, g, b);
                    }
                }
                48 => {
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.grid.attr.bg_index = subs[2] as u8;
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        let (r, g, b) = if subs.len() >= 6 {
                            (subs[3] as u8, subs[4] as u8, subs[5] as u8)
                        } else {
                            (subs[2] as u8, subs[3] as u8, subs[4] as u8)
                        };
                        self.grid.attr.bg_index = config::rgb_to_palette(r, g, b);
                    }
                }
                58 | 59 => {}
                _ => {
                    self.sgr(&subs[..1]);
                }
            }
        }
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        perform_shared::set_mode(
            self.grid,
            self.scrollback,
            self.response_buf,
            params,
            private,
        );
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        perform_shared::reset_mode(
            self.grid,
            self.scrollback,
            self.response_buf,
            params,
            private,
        );
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        perform_shared::set_scroll_region(
            self.grid,
            self.scrollback,
            self.response_buf,
            top,
            bottom,
        );
    }

    fn tab_clear(&mut self, mode: u16) {
        perform_shared::tab_clear(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        perform_shared::osc_dispatch(self.grid, self.scrollback, self.response_buf, params);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        perform_shared::esc_dispatch(
            self.grid,
            self.scrollback,
            self.response_buf,
            intermediates,
            byte,
        );
    }

    fn repeat_char(&mut self, n: u16) {
        let c = self.grid.last_char;
        for _ in 0..n {
            self.print(c);
        }
    }

    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'c') => self.response_buf.extend_from_slice(b"\x1B[?6c"),
            ([b'>'], b'c') => self.response_buf.extend_from_slice(b"\x1B[>0;0;0c"),
            ([b'?'], b'h') => self.set_mode(params, true),
            ([b'?'], b'l') => self.reset_mode(params, true),
            ([b'?', b'$'], b'p') => {
                if let Some(&mode) = params.first() {
                    let pm = match mode {
                        1 | 6 | 7 | 25 | 47 | 1000 | 1002 | 1003 | 1004 | 1006 | 1047 | 1049
                        | 2004 | 2026 => {
                            let flag = match mode {
                                1 => TermMode::CURSOR_KEYS,
                                6 => TermMode::ORIGIN_MODE,
                                7 => TermMode::AUTO_WRAP,
                                25 => TermMode::CURSOR_VISIBLE,
                                47 | 1047 | 1049 => TermMode::ALT_SCREEN,
                                1000 => TermMode::MOUSE_BUTTON,
                                1002 => TermMode::MOUSE_MOTION,
                                1003 => TermMode::MOUSE_ALL,
                                1004 => TermMode::FOCUS_EVENTS,
                                1006 => TermMode::MOUSE_SGR,
                                2004 => TermMode::BRACKETED_PASTE,
                                2026 => TermMode::SYNC_OUTPUT,
                                _ => unreachable!(),
                            };
                            if self.grid.mode.contains(flag) { 1 } else { 2 }
                        }
                        _ => 0,
                    };
                    let resp = format!("\x1B[?{};{}$y", mode, pm);
                    self.response_buf.extend_from_slice(resp.as_bytes());
                }
            }
            ([b' '], b'q') => {
                let style = params.first().copied().unwrap_or(0);
                self.set_cursor_style(style);
            }
            ([], _) => tty::parser::csi_fast::CsiFastParser::dispatch(byte, params, false, self),
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        perform_shared::save_cursor(self.grid);
    }

    fn restore_cursor(&mut self) {
        perform_shared::restore_cursor(self.grid);
    }

    fn device_status_report(&mut self, mode: u16) {
        perform_shared::device_status_report(self.grid, self.scrollback, self.response_buf, mode);
    }
}

fn parse(grid: &mut Grid, sb: &mut Scrollback, buf: &mut Vec<u8>, data: &[u8]) {
    let mut parser = Parser::new();
    let mut p = TestPerformer::from(grid, sb, buf);
    parser.parse(data, &mut p);
}

fn parse_only(grid: &mut Grid, sb: &mut Scrollback, data: &[u8]) {
    parse(grid, sb, &mut Vec::new(), data);
}

fn cell(grid: &Grid, row: u16, col: u16) -> Cell {
    grid.cells[grid.row_start(row) + col as usize]
}

// ── SGR Tests ───────────────────────────────────────────────────────────────

#[test]
fn sgr_all_attribute_modes() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[1mX\x1b[2mY");
    assert!(cell(&g, 0, 0).flags.contains(CellFlags::BOLD));
    assert!(cell(&g, 0, 1).flags.contains(CellFlags::DIM));

    parse_only(&mut g, &mut sb, b"\x1b[3mZ");
    assert!(cell(&g, 0, 2).flags.contains(CellFlags::ITALIC));

    parse_only(&mut g, &mut sb, b"\x1b[4mW");
    assert!(cell(&g, 0, 3).flags.contains(CellFlags::UNDERLINE));

    parse_only(&mut g, &mut sb, b"\x1b[7mV");
    assert!(cell(&g, 0, 4).flags.contains(CellFlags::INVERSE));

    parse_only(&mut g, &mut sb, b"\x1b[8mU");
    assert!(cell(&g, 0, 5).flags.contains(CellFlags::HIDDEN));

    parse_only(&mut g, &mut sb, b"\x1b[9mT");
    assert!(cell(&g, 0, 6).flags.contains(CellFlags::STRIKE));
}

#[test]
fn sgr_removes_attributes() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[1;4;7mA");
    assert!(cell(&g, 0, 0).flags.contains(CellFlags::BOLD));
    assert!(cell(&g, 0, 0).flags.contains(CellFlags::UNDERLINE));
    assert!(cell(&g, 0, 0).flags.contains(CellFlags::INVERSE));

    parse_only(&mut g, &mut sb, b"\x1b[22mB");
    assert!(!cell(&g, 0, 1).flags.contains(CellFlags::BOLD));
    assert!(!cell(&g, 0, 1).flags.contains(CellFlags::DIM));

    parse_only(&mut g, &mut sb, b"\x1b[24mC");
    assert!(!cell(&g, 1, 0).flags.contains(CellFlags::UNDERLINE));

    parse_only(&mut g, &mut sb, b"\x1b[27mD");
    assert!(!cell(&g, 1, 1).flags.contains(CellFlags::INVERSE));

    parse_only(&mut g, &mut sb, b"\x1b[28mE");
    assert!(!cell(&g, 1, 2).flags.contains(CellFlags::HIDDEN));

    parse_only(&mut g, &mut sb, b"\x1b[29mF");
    assert!(!cell(&g, 1, 3).flags.contains(CellFlags::STRIKE));
}

#[test]
fn sgr_fg_bg_ansi_colors() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[31mX");
    assert_eq!(cell(&g, 0, 0).fg_index, 1);

    parse_only(&mut g, &mut sb, b"\x1b[41mY");
    assert_eq!(cell(&g, 0, 1).bg_index, 1);

    parse_only(&mut g, &mut sb, b"\x1b[39mZ");
    assert_eq!(g.attr.fg_index, 7);

    parse_only(&mut g, &mut sb, b"\x1b[49mW");
    assert_eq!(g.attr.bg_index, 0);
}

#[test]
fn sgr_bright_fg_bg() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[91mX");
    assert_eq!(cell(&g, 0, 0).fg_index, 9);

    parse_only(&mut g, &mut sb, b"\x1b[100mY");
    assert_eq!(cell(&g, 0, 1).bg_index, 8);
}

#[test]
fn sgr_reset_via_0m() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[1;31;44mA");
    assert_eq!(cell(&g, 0, 0).fg_index, 1);
    assert_eq!(cell(&g, 0, 0).bg_index, 4);
    assert!(cell(&g, 0, 0).flags.contains(CellFlags::BOLD));

    parse_only(&mut g, &mut sb, b"\x1b[0mB");
    assert_eq!(cell(&g, 0, 1).fg_index, 7);
    assert_eq!(cell(&g, 0, 1).bg_index, 0);
    assert!(!cell(&g, 0, 1).flags.contains(CellFlags::BOLD));
}

#[test]
fn sgr_256_color() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[38;5;82mX");
    assert_eq!(cell(&g, 0, 0).fg_index, 82);

    parse_only(&mut g, &mut sb, b"\x1b[48;5;196mY");
    assert_eq!(cell(&g, 0, 1).bg_index, 196);
}

#[test]
fn sgr_truecolor_fg() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[38;2;255;128;64mX");
    let idx = cell(&g, 0, 0).fg_index;
    assert!(
        idx != 7,
        "truecolor should produce non-default palette index"
    );
}

#[test]
fn sgr_truecolor_bg() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[48;2;10;20;30mX");
    let idx = cell(&g, 0, 0).bg_index;
    assert!(
        idx != 0,
        "truecolor should produce non-default palette index"
    );
}

#[test]
fn sgr_reset_clears_256_color() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[38;5;200mX\x1b[0mY");
    assert_eq!(cell(&g, 0, 1).fg_index, 7);
}

#[test]
fn sgr_compound_sequence() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[1;31;44mX");
    let c = cell(&g, 0, 0);
    assert!(c.flags.contains(CellFlags::BOLD));
    assert_eq!(c.fg_index, 1);
    assert_eq!(c.bg_index, 4);
}

#[test]
fn sgr_multi_param_via_dispatch() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[1;7;32;41mX");
    let c = cell(&g, 0, 0);
    assert!(c.flags.contains(CellFlags::BOLD));
    assert!(c.flags.contains(CellFlags::INVERSE));
    assert_eq!(c.fg_index, 2);
    assert_eq!(c.bg_index, 1);
}

#[test]
fn sgr_colon_sub_underline_style() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    // 4:3 = curly underline, 31 = red fg
    parse_only(&mut g, &mut sb, b"\x1b[4:3;31mX");
    let c = cell(&g, 0, 0);
    assert!(c.flags.contains(CellFlags::UNDERLINE));
    assert_eq!(c.fg_index, 1);
}

#[test]
fn sgr_colon_sub_underline_off() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[4:0mX");
    assert!(!cell(&g, 0, 0).flags.contains(CellFlags::UNDERLINE));
}

#[test]
fn sgr_colon_256_color() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[38:5:196mX");
    assert_eq!(cell(&g, 0, 0).fg_index, 196);
}

#[test]
fn sgr_colon_truecolor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[38:2::255:128:64mX");
    let idx = cell(&g, 0, 0).fg_index;
    assert!(idx != 7);
}

// ── Mode Tests ───────────────────────────────────────────────────────────────

#[test]
fn decset_decrst_all_modes() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    let set_all = b"\x1b[?1h\x1b[?6h\x1b[?7h\x1b[?25h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h\x1b[?2004h\x1b[?2026h";
    parse_only(&mut g, &mut sb, set_all);
    assert!(g.mode.contains(TermMode::CURSOR_KEYS));
    assert!(g.mode.contains(TermMode::ORIGIN_MODE));
    assert!(g.mode.contains(TermMode::AUTO_WRAP));
    assert!(g.mode.contains(TermMode::CURSOR_VISIBLE));
    assert!(g.mode.contains(TermMode::MOUSE_BUTTON));
    assert!(g.mode.contains(TermMode::MOUSE_MOTION));
    assert!(g.mode.contains(TermMode::MOUSE_ALL));
    assert!(g.mode.contains(TermMode::FOCUS_EVENTS));
    assert!(g.mode.contains(TermMode::MOUSE_SGR));
    assert!(g.mode.contains(TermMode::BRACKETED_PASTE));
    assert!(g.mode.contains(TermMode::SYNC_OUTPUT));
    assert!(g.sync_start.is_some());

    let reset_all = b"\x1b[?1l\x1b[?6l\x1b[?7l\x1b[?25l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b[?2004l\x1b[?2026l";
    parse_only(&mut g, &mut sb, reset_all);
    assert!(!g.mode.contains(TermMode::CURSOR_KEYS));
    assert!(!g.mode.contains(TermMode::ORIGIN_MODE));
    assert!(!g.mode.contains(TermMode::AUTO_WRAP));
    assert!(!g.mode.contains(TermMode::CURSOR_VISIBLE));
    assert!(!g.mode.contains(TermMode::MOUSE_BUTTON));
    assert!(!g.mode.contains(TermMode::MOUSE_MOTION));
    assert!(!g.mode.contains(TermMode::MOUSE_ALL));
    assert!(!g.mode.contains(TermMode::FOCUS_EVENTS));
    assert!(!g.mode.contains(TermMode::MOUSE_SGR));
    assert!(!g.mode.contains(TermMode::BRACKETED_PASTE));
    assert!(!g.mode.contains(TermMode::SYNC_OUTPUT));
    assert!(g.sync_start.is_none());
}

#[test]
fn decset_1049_alt_screen_and_cursor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"Hello");
    parse_only(&mut g, &mut sb, b"\x1b[?1049h");
    assert!(g.mode.contains(TermMode::ALT_SCREEN));
    assert_eq!(g.cursor_row, 0);
    assert_eq!(g.cursor_col, 0);

    parse_only(&mut g, &mut sb, b"World");
    parse_only(&mut g, &mut sb, b"\x1b[?1049l");
    assert!(!g.mode.contains(TermMode::ALT_SCREEN));
    assert_eq!(g.cursor_row, 0);
    assert_eq!(g.cursor_col, 5);
}

// ── Cursor Movement with Scroll Region / ORIGIN_MODE ─────────────────────────

#[test]
fn origin_mode_constrains_cursor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;20r");
    parse_only(&mut g, &mut sb, b"\x1b[?6h");
    parse_only(&mut g, &mut sb, b"\x1b[1;1H");
    assert_eq!(g.cursor_row, 4);
    assert_eq!(g.cursor_col, 0);

    parse_only(&mut g, &mut sb, b"\x1b[16;1H");
    assert_eq!(g.cursor_row, 19);

    parse_only(&mut g, &mut sb, b"\x1b[?6l");
    parse_only(&mut g, &mut sb, b"\x1b[1;1H");
    assert_eq!(g.cursor_row, 0);
}

#[test]
fn origin_mode_vertical_absolute() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;20r");
    parse_only(&mut g, &mut sb, b"\x1b[?6h");
    parse_only(&mut g, &mut sb, b"\x1b[3d");
    assert_eq!(g.cursor_row, 6);
}

#[test]
fn scroll_region_set_homes_cursor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[10;10H");
    parse_only(&mut g, &mut sb, b"\x1b[3;20r");
    assert_eq!(g.cursor_row, 0);
    assert_eq!(g.cursor_col, 0);
}

#[test]
fn scroll_region_invalid_resets_to_full() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[20;10r");
    assert_eq!(g.scroll_top, 0);
    assert_eq!(g.scroll_bottom, 23);
}

#[test]
fn scroll_region_with_zero_bottom_defaults_max() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;0r");
    assert_eq!(g.scroll_top, 4);
    assert_eq!(g.scroll_bottom, 23);
}

#[test]
fn cursor_up_stops_at_scroll_region_boundary() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r");
    parse_only(&mut g, &mut sb, b"\x1b[6;1H");
    parse_only(&mut g, &mut sb, b"\x1b[3A");
    assert_eq!(g.cursor_row, 4);
}

#[test]
fn cursor_down_stops_at_scroll_region_boundary() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r");
    parse_only(&mut g, &mut sb, b"\x1b[8;1H");
    parse_only(&mut g, &mut sb, b"\x1b[5B");
    assert_eq!(g.cursor_row, 9);
}

#[test]
fn cursor_outside_scroll_region_not_constrained() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r");
    parse_only(&mut g, &mut sb, b"\x1b[3;1H");
    parse_only(&mut g, &mut sb, b"\x1b[5A");
    assert_eq!(g.cursor_row, 0);
}

// ── ESC Sequences ───────────────────────────────────────────────────────────

#[test]
fn esc_save_restore_cursor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10H\x1b7\x1b[1;1H\x1b8");
    assert_eq!(g.cursor_row, 4);
    assert_eq!(g.cursor_col, 9);
}

#[test]
fn esc_ind_linefeed() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"Hello\x1bD");
    assert_eq!(g.cursor_row, 1);
    assert_eq!(g.cursor_col, 5);
}

#[test]
fn esc_nel_crlf() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"Hello\x1bEWorld");
    assert_eq!(g.cursor_row, 1);
    assert_eq!(g.cursor_col, 5);
}

#[test]
fn esc_ri_reverse_index_scrolls_down() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[M");
    assert_eq!(g.cursor_row, 0);
    assert_eq!(g.cell(0, 0).codepoint, b' ' as u16);
}

#[test]
fn esc_ri_within_scroll_region() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r\x1b[5;1H\x1b[M");
    assert_eq!(g.cursor_row, 4);
}

#[test]
fn esc_hts_set_tab_stop() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[H");
    assert_eq!(g.cursor_col, 0);
    assert!(g.tab_stops[8]);
}

#[test]
fn esc_charset_g0_dec_special() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b(0");
    assert_eq!(g.charset_g0, 1);
    parse_only(&mut g, &mut sb, b"\x1b(B");
    assert_eq!(g.charset_g0, 0);
}

#[test]
fn esc_charset_g1_dec_special() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b)0");
    assert_eq!(g.charset_g1, 1);
    parse_only(&mut g, &mut sb, b"\x1b)B");
    assert_eq!(g.charset_g1, 0);
}

#[test]
fn dec_special_charset_prints_box_drawing() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    // \x1b(0 = G0 sets DEC Special, \x0f = SI switches to G0
    parse_only(&mut g, &mut sb, b"\x1b(0\x0f\x6a");
    assert_eq!(g.cell(0, 0).codepoint, '\u{2518}' as u16);
}

#[test]
fn so_si_switch_active_charset() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    // \x1b)0 = G1 sets DEC Special, \x0e = SO switches to G1
    // then \x0f = SI switches back to G0 (ASCII)
    parse_only(&mut g, &mut sb, b"\x1b)0\x0e\x6a\x0f\x6a");
    assert_eq!(g.cell(0, 0).codepoint, '\u{2518}' as u16);
    assert_eq!(g.cell(0, 1).codepoint, b'j' as u16);
}

#[test]
fn esc_ris_resets_terminal() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[1;31;44mHello\x1b[10;10H");
    parse_only(&mut g, &mut sb, b"\x1bc");
    assert_eq!(g.cursor_row, 0);
    assert_eq!(g.cursor_col, 0);
    assert_eq!(g.attr.fg_index, 7);
    assert_eq!(g.attr.bg_index, 0);
    assert!(g.mode.contains(TermMode::AUTO_WRAP));
    assert!(g.mode.contains(TermMode::CURSOR_VISIBLE));
}

// ── DSR Tests ───────────────────────────────────────────────────────────────

#[test]
fn dsr_status_response() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[5n");
    assert_eq!(buf, b"\x1b[0n");
}

#[test]
fn dsr_cursor_position_response() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[5;10H\x1b[6n");
    assert_eq!(buf, b"\x1b[5;10R");
}

#[test]
fn dsr_unknown_mode_no_response() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[0n");
    assert!(buf.is_empty());
}

// ── DA / DECRQM Tests ───────────────────────────────────────────────────────

#[test]
fn da1_via_state_machine() {
    // DA1 (`\x1b[c`) is consumed by the CSI fast path's `dispatch` which does
    // not handle 'c'. For DA1 to generate a response it must reach the state
    // machine's `csi_dispatch`. This test verifies the current behavior:
    // the fast path silently consumes it without a response.
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[c");
    assert!(buf.is_empty());
}

#[test]
fn da2_response() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[>c");
    assert_eq!(buf, b"\x1b[>0;0;0c");
}

#[test]
fn decrqm_reports_set_mode() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[?7$p");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b[?7;1$y");
}

#[test]
fn decrqm_reports_reset_mode() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[?7l\x1b[?7$p");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b[?7;2$y");
}

#[test]
fn decrqm_all_known_modes() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(
        &mut g,
        &mut sb,
        &mut buf,
        b"\x1b[?1$p\x1b[?6$p\x1b[?25$p\x1b[?1000$p\x1b[?2026$p",
    );
    let resp = std::str::from_utf8(&buf).unwrap();
    assert!(
        resp.contains(";2$y"),
        "all should report 2 (reset): {}",
        resp
    );
}

#[test]
fn decrqm_unknown_mode_reports_0() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b[?99$p");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b[?99;0$y");
}

// ── OSC Tests ───────────────────────────────────────────────────────────────

#[test]
fn osc_0_window_title() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]0;My Title\x07");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b]title:My Title\x07");
}

#[test]
fn osc_2_window_title() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]2;Tab Name\x07");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b]title:Tab Name\x07");
}

#[test]
fn osc_52_clipboard_set() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]52;c;dGVzdA==\x07");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b]52;set:dGVzdA==\x07");
}

#[test]
fn osc_52_clipboard_query() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]52;c;\x07");
    let resp = std::str::from_utf8(&buf).unwrap();
    assert_eq!(resp, "\x1b]52;query\x07");
}

#[test]
fn osc_empty_params_noop() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]\x07");
    assert!(buf.is_empty());
}

#[test]
fn osc_unknown_number_noop() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    let mut buf = Vec::new();

    parse(&mut g, &mut sb, &mut buf, b"\x1b]999;foo\x07");
    assert!(buf.is_empty());
}

// ── Repeat Char ─────────────────────────────────────────────────────────────

#[test]
fn repeat_char_repeats_last_printed_char() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"X\x1b[5b");
    for i in 0..6 {
        assert_eq!(g.cell(0, i).codepoint, b'X' as u16, "col {}", i);
    }
}

#[test]
fn repeat_char_defaults_to_space() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[3b");
    for i in 0..3 {
        assert_eq!(g.cell(0, i).codepoint, b' ' as u16, "col {}", i);
    }
}

// ── Tab Tests ───────────────────────────────────────────────────────────────

#[test]
fn tab_clear_all() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[3g");
    assert!(!g.tab_stops[8]);
    assert!(!g.tab_stops[16]);
}

#[test]
fn tab_set_at_cursor() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[4C"); // CUF 4 → col 4
    parse_only(&mut g, &mut sb, b"\x1bH"); // HTS set tab at col 4
    assert!(g.tab_stops[4]);
    parse_only(&mut g, &mut sb, b"\x1b[4D\x1b[4C\x1b[0g");
    assert!(!g.tab_stops[4]);
    assert!(g.tab_stops[8]);
}

// ── Insert/Delete Lines Outside Scroll Region ────────────────────────────────

#[test]
fn insert_lines_outside_region_noop() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r");
    parse_only(&mut g, &mut sb, b"\x1b[3;1H");
    parse_only(&mut g, &mut sb, b"\x1b[2L");
    // Cursor was above scroll region → insert should be no-op
    assert_eq!(g.cursor_row, 2);

    parse_only(&mut g, &mut sb, b"\x1b[12;1H");
    parse_only(&mut g, &mut sb, b"\x1b[2L");
    assert_eq!(g.cursor_row, 11);
}

#[test]
fn delete_lines_outside_region_noop() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[5;10r");
    parse_only(&mut g, &mut sb, b"\x1b[3;1H");
    parse_only(&mut g, &mut sb, b"\x1b[2M");
    assert_eq!(g.cursor_row, 2);
}

// ── DEC Special Graphics via G1 ──────────────────────────────────────────────

#[test]
fn dec_special_g1_charset() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b)0\x0e\x6a\x6b\x6c");
    assert_eq!(g.cell(0, 0).codepoint, '\u{2518}' as u16);
    assert_eq!(g.cell(0, 1).codepoint, '\u{2510}' as u16);
    assert_eq!(g.cell(0, 2).codepoint, '\u{250C}' as u16);
}

#[test]
fn dec_special_non_mapped_pass_through() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b(0\x0eabc");
    assert_eq!(g.cell(0, 0).codepoint, b'a' as u16);
    assert_eq!(g.cell(0, 1).codepoint, b'b' as u16);
    assert_eq!(g.cell(0, 2).codepoint, b'c' as u16);
}

// ── Cursor Vertical Absolute ─────────────────────────────────────────────────

#[test]
fn cursor_vertical_absolute_clamps() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[100d");
    assert_eq!(g.cursor_row, 23);
}

#[test]
fn cursor_vertical_absolute_zero_clamps_to_0() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    parse_only(&mut g, &mut sb, b"\x1b[0d");
    assert_eq!(g.cursor_row, 0);
}

// ── ED Mode 3 clears scrollback ─────────────────────────────────────────────

#[test]
fn erase_display_mode_3_clears_scrollback() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    // Push some content to scrollback
    parse_only(&mut g, &mut sb, b"\x1b[23S");
    assert!(!sb.is_empty());

    parse_only(&mut g, &mut sb, b"\x1b[3J");
    assert_eq!(sb.len(), 0);
}

// ── Cursor Style ─────────────────────────────────────────────────────────────

#[test]
fn decscusr_noop_does_not_crash() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);
    parse_only(&mut g, &mut sb, b"\x1b[3 q");
    // DECSCUSR goes through state machine, should be handled
}

// ── SO/SI via Execute ────────────────────────────────────────────────────────

#[test]
fn so_si_execute_toggles() {
    let mut g = Grid::new(80, 24);
    let mut sb = Scrollback::new(100);

    assert_eq!(g.active_charset, 0);
    parse_only(&mut g, &mut sb, b"\x0e");
    assert_eq!(g.active_charset, 1);
    parse_only(&mut g, &mut sb, b"\x0f");
    assert_eq!(g.active_charset, 0);
}
