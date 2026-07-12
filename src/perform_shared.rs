use std::time::Instant;

use crate::config;
use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;

pub fn execute(
    grid: &mut Grid,
    scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    byte: u8,
) {
    match byte {
        0x07 => {} // BEL (TODO: visual bell)
        0x08 => grid.backspace(),
        0x09 => {
            // TAB
            let col = grid.cursor_col;
            let cols = grid.cols;
            let mut next = col + 1;
            while next < cols {
                if grid.tab_stops[next as usize] {
                    break;
                }
                next += 1;
            }
            grid.cursor_col = next.min(cols - 1);
            grid.cursor_pending_wrap = false;
        }
        0x0A..=0x0C => grid.linefeed(Some(scrollback)),
        0x0D => grid.carriage_return(),
        0x0E => grid.active_charset = 1, // SO → G1
        0x0F => grid.active_charset = 0, // SI → G0
        _ => {}
    }
}

pub fn cursor_up(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    let row = grid.cursor_row;
    let top = if row >= grid.scroll_top && row <= grid.scroll_bottom {
        grid.scroll_top
    } else {
        0
    };
    grid.cursor_row = row.saturating_sub(n).max(top);
    grid.cursor_pending_wrap = false;
    grid.mark_dirty(grid.cursor_row);
}

pub fn cursor_down(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    let row = grid.cursor_row;
    let bottom = if row >= grid.scroll_top && row <= grid.scroll_bottom {
        grid.scroll_bottom
    } else {
        grid.rows - 1
    };
    grid.cursor_row = row.saturating_add(n).min(bottom);
    grid.cursor_pending_wrap = false;
    grid.mark_dirty(grid.cursor_row);
}

pub fn cursor_forward(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.cursor_col = grid.cursor_col.saturating_add(n).min(grid.cols - 1);
    grid.cursor_pending_wrap = false;
}

pub fn cursor_backward(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.cursor_col = grid.cursor_col.saturating_sub(n);
    grid.cursor_pending_wrap = false;
}

pub fn cursor_position(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    row: u16,
    col: u16,
) {
    if grid.mode.contains(TermMode::ORIGIN_MODE) {
        // DECOM: coordinates are relative to scroll region, clamped within it
        let top = grid.scroll_top;
        let bottom = grid.scroll_bottom;
        grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
    } else {
        grid.cursor_row = (row.saturating_sub(1)).min(grid.rows - 1);
    }
    grid.cursor_col = (col.saturating_sub(1)).min(grid.cols - 1);
    grid.cursor_pending_wrap = false;
}

pub fn cursor_horizontal_absolute(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    col: u16,
) {
    grid.cursor_col = (col.saturating_sub(1)).min(grid.cols - 1);
    grid.cursor_pending_wrap = false;
}

pub fn cursor_vertical_absolute(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    row: u16,
) {
    if grid.mode.contains(TermMode::ORIGIN_MODE) {
        let top = grid.scroll_top;
        let bottom = grid.scroll_bottom;
        grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
    } else {
        grid.cursor_row = (row.saturating_sub(1)).min(grid.rows - 1);
    }
    grid.cursor_pending_wrap = false;
}

pub fn erase_in_display(
    grid: &mut Grid,
    scrollback: &mut Scrollback,
    response_buf: &mut Vec<u8>,
    mode: u16,
) {
    let row = grid.cursor_row;
    let col = grid.cursor_col;
    match mode {
        0 => {
            grid.clear_cols(row, col, grid.cols);
            grid.clear_rows(row + 1, grid.rows);
        }
        1 => {
            grid.clear_rows(0, row);
            grid.clear_cols(row, 0, col + 1);
        }
        2 => {
            grid.clear_rows(0, grid.rows);
        }
        3 => {
            grid.clear_rows(0, grid.rows);
            scrollback_clear(response_buf, scrollback);
        }
        _ => {}
    }
}

fn scrollback_clear(_response_buf: &mut Vec<u8>, scrollback: &mut Scrollback) {
    scrollback.clear();
}

pub fn erase_in_line(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    mode: u16,
) {
    let row = grid.cursor_row;
    let col = grid.cursor_col;
    match mode {
        0 => grid.clear_cols(row, col, grid.cols),
        1 => grid.clear_cols(row, 0, col + 1),
        2 => grid.clear_cols(row, 0, grid.cols),
        _ => {}
    }
}

pub fn scroll_up(
    grid: &mut Grid,
    scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.scroll_up_into(n, Some(scrollback));
}

pub fn scroll_down(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.scroll_down(n);
}

pub fn insert_lines(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    let row = grid.cursor_row;
    if row < grid.scroll_top || row > grid.scroll_bottom {
        return;
    }
    let old_top = grid.scroll_top;
    grid.scroll_top = row;
    grid.scroll_down(n);
    grid.scroll_top = old_top;
    grid.cursor_col = 0;
}

pub fn delete_lines(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    let row = grid.cursor_row;
    if row < grid.scroll_top || row > grid.scroll_bottom {
        return;
    }
    let old_top = grid.scroll_top;
    grid.scroll_top = row;
    grid.scroll_up(n);
    grid.scroll_top = old_top;
    grid.cursor_col = 0;
}

pub fn insert_chars(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.insert_chars(n);
}

pub fn delete_chars(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    grid.delete_chars(n);
}

pub fn erase_chars(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    n: u16,
) {
    let row = grid.cursor_row;
    let col = grid.cursor_col;
    grid.clear_cols(row, col, (col + n).min(grid.cols));
}

pub fn sgr(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    params: &[u16],
) {
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            38 => {
                // Extended fg color (38;5;N or 38;2;R;G;B)
                i += 1;
                if i < params.len() {
                    match params[i] {
                        5 => {
                            i += 1;
                            if i < params.len() {
                                grid.attr.fg_index = params[i] as u8;
                            }
                        }
                        2 if i + 3 < params.len() => {
                            grid.attr.fg_index = config::rgb_to_palette(
                                params[i + 1] as u8,
                                params[i + 2] as u8,
                                params[i + 3] as u8,
                            );
                            i += 3;
                        }
                        2 => {}
                        _ => {}
                    }
                }
            }
            48 => {
                // Extended bg color (48;5;N or 48;2;R;G;B)
                i += 1;
                if i < params.len() {
                    match params[i] {
                        5 => {
                            i += 1;
                            if i < params.len() {
                                grid.attr.bg_index = params[i] as u8;
                            }
                        }
                        2 if i + 3 < params.len() => {
                            grid.attr.bg_index = config::rgb_to_palette(
                                params[i + 1] as u8,
                                params[i + 2] as u8,
                                params[i + 3] as u8,
                            );
                            i += 3;
                        }
                        2 => {}
                        _ => {}
                    }
                }
            }
            code => sgr_single(grid, code),
        }
        i += 1;
    }
}

#[inline]
pub fn sgr_reset(grid: &mut Grid) {
    grid.attr.flags = CellFlags::empty();
    grid.attr.fg_index = 7;
    grid.attr.bg_index = 0;
}

#[inline]
pub fn sgr_single(grid: &mut Grid, code: u16) {
    match code {
        0 => sgr_reset(grid),
        1 => grid.attr.flags.insert(CellFlags::BOLD),
        2 => grid.attr.flags.insert(CellFlags::DIM),
        3 => grid.attr.flags.insert(CellFlags::ITALIC),
        4 => grid.attr.flags.insert(CellFlags::UNDERLINE),
        7 => grid.attr.flags.insert(CellFlags::INVERSE),
        8 => grid.attr.flags.insert(CellFlags::HIDDEN),
        9 => grid.attr.flags.insert(CellFlags::STRIKE),
        21 | 22 => {
            grid.attr.flags.remove(CellFlags::BOLD);
            grid.attr.flags.remove(CellFlags::DIM);
        }
        23 => grid.attr.flags.remove(CellFlags::ITALIC),
        24 => grid.attr.flags.remove(CellFlags::UNDERLINE),
        27 => grid.attr.flags.remove(CellFlags::INVERSE),
        28 => grid.attr.flags.remove(CellFlags::HIDDEN),
        29 => grid.attr.flags.remove(CellFlags::STRIKE),
        30..=37 => grid.attr.fg_index = (code - 30) as u8,
        39 => grid.attr.fg_index = 7,
        40..=47 => grid.attr.bg_index = (code - 40) as u8,
        49 => grid.attr.bg_index = 0,
        90..=97 => grid.attr.fg_index = (code - 90 + 8) as u8,
        100..=107 => grid.attr.bg_index = (code - 100 + 8) as u8,
        _ => {}
    }
}

#[inline]
pub fn color_256(grid: &mut Grid, fg: bool, index: u16) {
    if fg {
        grid.attr.fg_index = index as u8;
    } else {
        grid.attr.bg_index = index as u8;
    }
}

#[inline]
pub fn color_rgb(grid: &mut Grid, fg: bool, r: u16, g: u16, b: u16) {
    let index = config::rgb_to_palette(r as u8, g as u8, b as u8);
    if fg {
        grid.attr.fg_index = index;
    } else {
        grid.attr.bg_index = index;
    }
}

pub fn set_mode(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    params: &[u16],
    private: bool,
) {
    for &p in params {
        if private {
            match p {
                1 => grid.mode.insert(TermMode::CURSOR_KEYS),
                6 => {
                    grid.mode.insert(TermMode::ORIGIN_MODE);
                    grid.cursor_row = grid.scroll_top;
                    grid.cursor_col = 0;
                    grid.cursor_pending_wrap = false;
                }
                7 => grid.mode.insert(TermMode::AUTO_WRAP),
                25 => grid.mode.insert(TermMode::CURSOR_VISIBLE),
                47 | 1047 => grid.enter_alt_screen(),
                1049 => {
                    grid.save_cursor();
                    grid.enter_alt_screen();
                }
                1000 => grid.mode.insert(TermMode::MOUSE_BUTTON),
                1002 => grid.mode.insert(TermMode::MOUSE_MOTION),
                1003 => grid.mode.insert(TermMode::MOUSE_ALL),
                1004 => grid.mode.insert(TermMode::FOCUS_EVENTS),
                1006 => grid.mode.insert(TermMode::MOUSE_SGR),
                2004 => grid.mode.insert(TermMode::BRACKETED_PASTE),
                2026 if !grid.mode.contains(TermMode::SYNC_OUTPUT) => {
                    grid.mode.insert(TermMode::SYNC_OUTPUT);
                    grid.sync_start = Some(Instant::now());
                }
                2026 => {}
                _ => {}
            }
        }
    }
}

pub fn reset_mode(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    params: &[u16],
    private: bool,
) {
    for &p in params {
        if private {
            match p {
                1 => grid.mode.remove(TermMode::CURSOR_KEYS),
                6 => {
                    grid.mode.remove(TermMode::ORIGIN_MODE);
                    grid.cursor_row = 0;
                    grid.cursor_col = 0;
                    grid.cursor_pending_wrap = false;
                }
                7 => grid.mode.remove(TermMode::AUTO_WRAP),
                25 => grid.mode.remove(TermMode::CURSOR_VISIBLE),
                47 | 1047 => grid.exit_alt_screen(),
                1049 => {
                    grid.exit_alt_screen();
                    grid.restore_cursor();
                }
                1000 => grid.mode.remove(TermMode::MOUSE_BUTTON),
                1002 => grid.mode.remove(TermMode::MOUSE_MOTION),
                1003 => grid.mode.remove(TermMode::MOUSE_ALL),
                1004 => grid.mode.remove(TermMode::FOCUS_EVENTS),
                1006 => grid.mode.remove(TermMode::MOUSE_SGR),
                2004 => grid.mode.remove(TermMode::BRACKETED_PASTE),
                2026 => {
                    grid.mode.remove(TermMode::SYNC_OUTPUT);
                    grid.sync_start = None;
                }
                _ => {}
            }
        }
    }
}

pub fn set_scroll_region(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    top: u16,
    bottom: u16,
) {
    let top = top.saturating_sub(1);
    let bottom = if bottom == 0 {
        grid.rows - 1
    } else {
        (bottom.saturating_sub(1)).min(grid.rows - 1)
    };
    if top < bottom {
        grid.scroll_top = top;
        grid.scroll_bottom = bottom;
    } else {
        // Invalid region — reset to full screen
        grid.scroll_top = 0;
        grid.scroll_bottom = grid.rows - 1;
    }
    // DECSTBM homes cursor to (1,1). With DECOM, that's top of scroll region.
    if grid.mode.contains(TermMode::ORIGIN_MODE) {
        grid.cursor_row = grid.scroll_top;
    } else {
        grid.cursor_row = 0;
    }
    grid.cursor_col = 0;
    grid.cursor_pending_wrap = false;
}

pub fn tab_clear(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    _response_buf: &mut Vec<u8>,
    mode: u16,
) {
    match mode {
        0 => {
            let col = grid.cursor_col as usize;
            if col < grid.tab_stops.len() {
                grid.tab_stops.set(col, false);
            }
        }
        3 => grid.tab_stops.fill(false),
        _ => {}
    }
}

pub fn set_tab_stop(grid: &mut Grid, _scrollback: &mut Scrollback, _response_buf: &mut Vec<u8>) {
    let col = grid.cursor_col as usize;
    if col < grid.tab_stops.len() {
        grid.tab_stops.set(col, true);
    }
}

pub fn osc_dispatch(
    _grid: &mut Grid,
    _scrollback: &mut Scrollback,
    response_buf: &mut Vec<u8>,
    params: &[&[u8]],
) {
    if params.is_empty() {
        return;
    }
    let num = std::str::from_utf8(params[0])
        .ok()
        .and_then(|s| s.parse::<u16>().ok());

    match num {
        Some(0) | Some(2) if params.len() > 1 => {
            let title: Vec<u8> = params[1..].join(&b';');
            response_buf.extend_from_slice(b"\x1B]title:");
            response_buf.extend_from_slice(&title);
            response_buf.push(0x07);
        }
        Some(0) | Some(2) => {}
        Some(52) if params.len() >= 3 => {
            let data = params[2];
            if data.is_empty() {
                response_buf.extend_from_slice(b"\x1B]52;query\x07");
            } else {
                response_buf.extend_from_slice(b"\x1B]52;set:");
                response_buf.extend_from_slice(data);
                response_buf.push(0x07);
            }
        }
        Some(52) => {}
        _ => {}
    }
}

pub fn esc_dispatch(
    grid: &mut Grid,
    scrollback: &mut Scrollback,
    response_buf: &mut Vec<u8>,
    intermediates: &[u8],
    byte: u8,
) {
    match (intermediates, byte) {
        ([], b'7') => grid.save_cursor(),
        ([], b'8') => grid.restore_cursor(),
        ([], b'D') => grid.linefeed(Some(scrollback)), // IND
        ([], b'E') => {
            // NEL
            grid.carriage_return();
            grid.linefeed(Some(scrollback));
        }
        ([], b'H') => set_tab_stop(grid, scrollback, response_buf),
        ([], b'M') => {
            // RI
            if grid.cursor_row == grid.scroll_top {
                scroll_down(grid, scrollback, response_buf, 1);
            } else if grid.cursor_row > 0 {
                grid.cursor_row -= 1;
            }
        }
        ([], b'c') => {
            // RIS
            let rows = grid.rows;
            grid.clear_rows(0, rows);
            grid.cursor_row = 0;
            grid.cursor_col = 0;
            grid.attr = Cell::default();
            grid.mode = TermMode::AUTO_WRAP | TermMode::CURSOR_VISIBLE;
            grid.scroll_top = 0;
            grid.scroll_bottom = rows - 1;
            grid.charset_g0 = 0;
            grid.charset_g1 = 0;
            grid.active_charset = 0;
        }
        ([b'('], b'0') => grid.charset_g0 = 1,
        ([b'('], b'B') => grid.charset_g0 = 0,
        ([b')'], b'0') => grid.charset_g1 = 1,
        ([b')'], b'B') => grid.charset_g1 = 0,
        _ => {}
    }
}

pub fn save_cursor(grid: &mut Grid) {
    grid.save_cursor();
}

pub fn restore_cursor(grid: &mut Grid) {
    grid.restore_cursor();
}

pub fn device_status_report(
    grid: &mut Grid,
    _scrollback: &mut Scrollback,
    response_buf: &mut Vec<u8>,
    mode: u16,
) {
    match mode {
        5 => response_buf.extend_from_slice(b"\x1B[0n"),
        6 => {
            let row = grid.cursor_row + 1;
            let col = grid.cursor_col + 1;
            let resp = format!("\x1B[{};{}R", row, col);
            response_buf.extend_from_slice(resp.as_bytes());
        }
        _ => {}
    }
}
