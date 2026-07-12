use tty::parser::csi_fast::CsiFastParser;
use tty::parser::perform::Perform;

/// Records the last action performed for assertions.
#[derive(Debug, Default)]
struct Recorder {
    sgr_reset_count: usize,
    sgr_codes: Vec<u16>,
    sgr_colon_raw: Vec<u8>,
    color_256_fg: Vec<u16>,
    color_256_bg: Vec<u16>,
    color_rgb_fg: Vec<(u16, u16, u16)>,
    color_rgb_bg: Vec<(u16, u16, u16)>,
    cursor_position: Vec<(u16, u16)>,
    cursor_up: Vec<u16>,
    cursor_down: Vec<u16>,
    cursor_forward: Vec<u16>,
    cursor_backward: Vec<u16>,
    cursor_horizontal: Vec<u16>,
    cursor_vertical: Vec<u16>,
    erase_display: Vec<u16>,
    erase_line: Vec<u16>,
    scroll_up: Vec<u16>,
    scroll_down: Vec<u16>,
    insert_lines: Vec<u16>,
    delete_lines: Vec<u16>,
    insert_chars: Vec<u16>,
    delete_chars: Vec<u16>,
    erase_chars: Vec<u16>,
    set_mode_params: Vec<Vec<u16>>,
    set_mode_private: Vec<bool>,
    reset_mode_params: Vec<Vec<u16>>,
    reset_mode_private: Vec<bool>,
    scroll_region: Vec<(u16, u16)>,
    tab_clear: Vec<u16>,
    set_tab_stop_count: usize,
    device_status: Vec<u16>,
    save_cursor_count: usize,
    restore_cursor_count: usize,
    set_cursor_style: Vec<u16>,
    repeat_char: Vec<u16>,
    osc_pararms: Vec<Vec<Vec<u8>>>,
    csi_dispatch_params: Vec<Vec<u16>>,
    csi_dispatch_intermediates: Vec<Vec<u8>>,
    csi_dispatch_byte: Vec<u8>,
}

impl Perform for Recorder {
    fn print_ascii_run(&mut self, _bytes: &[u8]) {}
    fn print(&mut self, _c: char) {}
    fn execute(&mut self, _byte: u8) {}

    fn sgr_reset(&mut self) {
        self.sgr_reset_count += 1;
    }

    fn sgr_single(&mut self, code: u16) {
        self.sgr_codes.push(code);
    }

    fn sgr(&mut self, params: &[u16]) {
        self.sgr_codes.extend_from_slice(params);
    }

    fn color_256(&mut self, fg: bool, index: u16) {
        if fg {
            self.color_256_fg.push(index);
        } else {
            self.color_256_bg.push(index);
        }
    }

    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        if fg {
            self.color_rgb_fg.push((r, g, b));
        } else {
            self.color_rgb_bg.push((r, g, b));
        }
    }

    fn cursor_up(&mut self, n: u16) {
        self.cursor_up.push(n);
    }
    fn cursor_down(&mut self, n: u16) {
        self.cursor_down.push(n);
    }
    fn cursor_forward(&mut self, n: u16) {
        self.cursor_forward.push(n);
    }
    fn cursor_backward(&mut self, n: u16) {
        self.cursor_backward.push(n);
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.cursor_position.push((row, col));
    }
    fn cursor_horizontal_absolute(&mut self, col: u16) {
        self.cursor_horizontal.push(col);
    }
    fn cursor_vertical_absolute(&mut self, row: u16) {
        self.cursor_vertical.push(row);
    }
    fn erase_in_display(&mut self, mode: u16) {
        self.erase_display.push(mode);
    }
    fn erase_in_line(&mut self, mode: u16) {
        self.erase_line.push(mode);
    }
    fn scroll_up(&mut self, n: u16) {
        self.scroll_up.push(n);
    }
    fn scroll_down(&mut self, n: u16) {
        self.scroll_down.push(n);
    }
    fn insert_lines(&mut self, n: u16) {
        self.insert_lines.push(n);
    }
    fn delete_lines(&mut self, n: u16) {
        self.delete_lines.push(n);
    }
    fn insert_chars(&mut self, n: u16) {
        self.insert_chars.push(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.delete_chars.push(n);
    }
    fn erase_chars(&mut self, n: u16) {
        self.erase_chars.push(n);
    }

    fn sgr_colon(&mut self, raw: &[u8]) {
        self.sgr_colon_raw = raw.to_vec();
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        self.set_mode_params.push(params.to_vec());
        self.set_mode_private.push(private);
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        self.reset_mode_params.push(params.to_vec());
        self.reset_mode_private.push(private);
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.scroll_region.push((top, bottom));
    }

    fn tab_clear(&mut self, mode: u16) {
        self.tab_clear.push(mode);
    }
    fn set_tab_stop(&mut self) {
        self.set_tab_stop_count += 1;
    }
    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        self.osc_pararms
            .push(params.iter().map(|p| p.to_vec()).collect());
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _byte: u8) {}
    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], _ignore: bool, byte: u8) {
        self.csi_dispatch_params.push(params.to_vec());
        self.csi_dispatch_intermediates.push(intermediates.to_vec());
        self.csi_dispatch_byte.push(byte);
    }

    fn save_cursor(&mut self) {
        self.save_cursor_count += 1;
    }
    fn restore_cursor(&mut self) {
        self.restore_cursor_count += 1;
    }
    fn device_status_report(&mut self, mode: u16) {
        self.device_status.push(mode);
    }
    fn set_cursor_style(&mut self, style: u16) {
        self.set_cursor_style.push(style);
    }
    fn repeat_char(&mut self, n: u16) {
        self.repeat_char.push(n);
    }
}

#[test]
fn inline_sgr_reset() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"0m", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.sgr_reset_count, 1);
}

#[test]
fn inline_sgr_single_digit() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"1m", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.sgr_codes, vec![1]);
}

#[test]
fn inline_sgr_two_digit() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"32m", &mut r);
    assert_eq!(consumed, Some(3));
    assert_eq!(r.sgr_codes, vec![32]);
}

#[test]
fn inline_sgr_compound_digit_semicolon() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"1;7m", &mut r);
    assert_eq!(consumed, Some(4));
    assert_eq!(r.sgr_codes, vec![1, 7]);
}

#[test]
fn inline_sgr_compound_digit_two_digit() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"1;32m", &mut r);
    assert_eq!(consumed, Some(5));
    assert_eq!(r.sgr_codes, vec![1, 32]);
}

#[test]
fn inline_sgr_two_digit_compound() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"41;7m", &mut r);
    assert_eq!(consumed, Some(5));
    assert_eq!(r.sgr_codes, vec![41, 7]);
}

#[test]
fn inline_sgr_two_digit_two_digit_compound() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"41;37m", &mut r);
    assert_eq!(consumed, Some(6));
    assert_eq!(r.sgr_codes, vec![41, 37]);
}

#[test]
fn inline_erase_in_line_k() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"K", &mut r);
    assert_eq!(consumed, Some(1));
    assert_eq!(r.erase_line, vec![0]);
}

#[test]
fn inline_erase_in_line_digit_k() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"1K", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.erase_line, vec![1]);
}

#[test]
fn inline_erase_in_display_digit_j() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"2J", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.erase_display, vec![2]);
}

#[test]
fn inline_cursor_home_h() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"H", &mut r);
    assert_eq!(consumed, Some(1));
    assert_eq!(r.cursor_position, vec![(1, 1)]);
}

#[test]
fn inline_cup_digit_semicolon_h() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"5;10H", &mut r);
    assert_eq!(consumed, Some(5));
    assert_eq!(r.cursor_position, vec![(5, 10)]);
}

#[test]
fn inline_cup_two_digit_semicolon_h() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"12;34H", &mut r);
    assert_eq!(consumed, Some(6));
    assert_eq!(r.cursor_position, vec![(12, 34)]);
}

#[test]
fn inline_empty_buffer_returns_none() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_csi_inline(b"", &mut r).is_none());
}

#[test]
fn inline_unrecognized_returns_none() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_csi_inline(b"??", &mut r).is_none());
}

#[test]
fn inline_unknown_non_digit_returns_none() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_csi_inline(b"X", &mut r).is_none());
}

#[test]
fn main_csi_sgr_reset_dispatch() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"0m", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.sgr_reset_count, 1);
}

#[test]
fn main_csi_sgr_single() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"1m", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.sgr_codes, vec![1]);
}

#[test]
fn main_csi_sgr_multi() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"1;7;32m", &mut r);
    assert_eq!(consumed, Some(7));
    assert_eq!(r.sgr_codes, vec![1, 7, 32]);
}

#[test]
fn main_csi_256_color_fg() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"38;5;196m", &mut r);
    assert_eq!(consumed, Some(9));
    assert!(r.sgr_codes.contains(&38));
    assert!(r.sgr_codes.contains(&196));
}

#[test]
fn main_csi_256_color_bg() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"48;5;42m", &mut r);
    assert_eq!(consumed, Some(8));
    assert!(r.sgr_codes.contains(&48));
    assert!(r.sgr_codes.contains(&42));
}

#[test]
fn main_csi_truecolor_fg() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"38;2;255;128;64m", &mut r);
    assert_eq!(consumed, Some(16));
    assert!(r.sgr_codes.contains(&255));
    assert!(r.sgr_codes.contains(&128));
    assert!(r.sgr_codes.contains(&64));
}

#[test]
fn main_csi_truecolor_bg() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"48;2;10;20;30m", &mut r);
    assert_eq!(consumed, Some(14));
    assert!(r.sgr_codes.contains(&48));
    assert!(r.sgr_codes.contains(&10));
    assert!(r.sgr_codes.contains(&20));
    assert!(r.sgr_codes.contains(&30));
}

#[test]
fn main_csi_incomplete_buf_returns_none() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_parse(b"38;5;", &mut r).is_none());
}

#[test]
fn main_csi_empty_buf_returns_none() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_parse(b"", &mut r).is_none());
}

#[test]
fn main_csi_private_mode_prefix() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"?25l", &mut r);
    assert_eq!(consumed, Some(4));
    assert_eq!(r.reset_mode_params.last(), Some(&vec![25]));
    assert_eq!(r.reset_mode_private.last().copied(), Some(true));
}

#[test]
fn main_csi_intermediate_byte_bails() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_parse(b"  q", &mut r).is_none());
    assert!(CsiFastParser::try_parse(b"/q", &mut r).is_none());
}

#[test]
fn main_csi_uos_dispatch_cap() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"5n", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.device_status, vec![5]);

    let consumed = CsiFastParser::try_parse(b"6n", &mut r);
    assert_eq!(consumed, Some(2));
    assert_eq!(r.device_status, vec![5, 6]);
}

#[test]
fn main_csi_scroll_up_down() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3S", &mut r);
    assert_eq!(r.scroll_up, vec![3]);

    CsiFastParser::try_parse(b"5T", &mut r);
    assert_eq!(r.scroll_down, vec![5]);
}

#[test]
fn main_csi_insert_delete_lines() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"2L", &mut r);
    assert_eq!(r.insert_lines, vec![2]);

    CsiFastParser::try_parse(b"4M", &mut r);
    assert_eq!(r.delete_lines, vec![4]);
}

#[test]
fn main_csi_insert_delete_chars() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3@", &mut r);
    assert_eq!(r.insert_chars, vec![3]);

    CsiFastParser::try_parse(b"5P", &mut r);
    assert_eq!(r.delete_chars, vec![5]);
}

#[test]
fn main_csi_erase_chars() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"7X", &mut r);
    assert_eq!(r.erase_chars, vec![7]);
}

#[test]
fn main_csi_set_reset_mode_non_private() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"5h", &mut r);
    assert_eq!(r.set_mode_params.last(), Some(&vec![5]));
    assert_eq!(r.set_mode_private.last().copied(), Some(false));

    CsiFastParser::try_parse(b"5l", &mut r);
    assert_eq!(r.reset_mode_params.last(), Some(&vec![5]));
    assert_eq!(r.reset_mode_private.last().copied(), Some(false));
}

#[test]
fn main_csi_scroll_region() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3;20r", &mut r);
    assert_eq!(r.scroll_region, vec![(3, 20)]);
}

#[test]
fn main_csi_scroll_region_default_bottom() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"5;0r", &mut r);
    assert_eq!(r.scroll_region, vec![(5, 0)]);
}

#[test]
fn main_csi_tab_clear() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"0g", &mut r);
    assert_eq!(r.tab_clear, vec![0]);

    CsiFastParser::try_parse(b"3g", &mut r);
    assert_eq!(r.tab_clear, vec![0, 3]);
}

#[test]
fn main_csi_set_cursor_style() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3q", &mut r);
    assert_eq!(r.set_cursor_style, vec![3]);
}

#[test]
fn main_csi_repeat_char() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"5b", &mut r);
    assert_eq!(r.repeat_char, vec![5]);
}

#[test]
fn main_csi_save_restore_cursor() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"s", &mut r);
    assert_eq!(r.save_cursor_count, 1);

    CsiFastParser::try_parse(b"u", &mut r);
    assert_eq!(r.restore_cursor_count, 1);
}

#[test]
fn main_csi_private_set_reset() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"?25h", &mut r);
    assert_eq!(r.set_mode_private.last().copied(), Some(true));
    assert_eq!(r.set_mode_params.last(), Some(&vec![25]));

    CsiFastParser::try_parse(b"?25l", &mut r);
    assert_eq!(r.reset_mode_private.last().copied(), Some(true));
    assert_eq!(r.reset_mode_params.last(), Some(&vec![25]));
}

#[test]
fn main_csi_cursor_movement_letters() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3A", &mut r);
    assert_eq!(r.cursor_up, vec![3]);

    CsiFastParser::try_parse(b"4B", &mut r);
    assert_eq!(r.cursor_down, vec![4]);

    CsiFastParser::try_parse(b"5C", &mut r);
    assert_eq!(r.cursor_forward, vec![5]);

    CsiFastParser::try_parse(b"6D", &mut r);
    assert_eq!(r.cursor_backward, vec![6]);
}

#[test]
fn main_csi_cursor_next_prev_line() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"3E", &mut r);
    assert_eq!(r.cursor_down, vec![3]);
    assert_eq!(r.cursor_horizontal, vec![1]);

    CsiFastParser::try_parse(b"4F", &mut r);
    assert_eq!(r.cursor_up, vec![4]);
    assert_eq!(r.cursor_horizontal, vec![1, 1]);
}

#[test]
fn main_csi_cursor_position_alternate_f() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"10;20f", &mut r);
    assert_eq!(r.cursor_position, vec![(10, 20)]);
}

#[test]
fn main_csi_cursor_horizontal_vertical_absolute() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"15G", &mut r);
    assert_eq!(r.cursor_horizontal, vec![15]);

    CsiFastParser::try_parse(b"8d", &mut r);
    assert_eq!(r.cursor_vertical, vec![8]);
}

#[test]
fn main_csi_erase_alternates() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"0J", &mut r);
    assert_eq!(r.erase_display, vec![0]);

    CsiFastParser::try_parse(b"1J", &mut r);
    assert_eq!(r.erase_display, vec![0, 1]);

    CsiFastParser::try_parse(b"3J", &mut r);
    assert_eq!(r.erase_display, vec![0, 1, 3]);
}

#[test]
fn main_csi_erase_line_alternates() {
    let mut r = Recorder::default();
    CsiFastParser::try_parse(b"0K", &mut r);
    assert_eq!(r.erase_line, vec![0]);

    CsiFastParser::try_parse(b"1K", &mut r);
    assert_eq!(r.erase_line, vec![0, 1]);

    CsiFastParser::try_parse(b"2K", &mut r);
    assert_eq!(r.erase_line, vec![0, 1, 2]);
}

// Dispatch table tests for letter variants

#[test]
fn dispatch_cursor_down_alternate_e() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'e', &[3], false, &mut r);
    assert_eq!(r.cursor_down, vec![3]);
}

#[test]
fn dispatch_cursor_forward_alternate_a() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'a', &[5], false, &mut r);
    assert_eq!(r.cursor_forward, vec![5]);
}

#[test]
fn dispatch_cursor_horizontal_alternate_backtick() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'`', &[7], false, &mut r);
    assert_eq!(r.cursor_horizontal, vec![7]);
}

#[test]
fn dispatch_cursor_position_h_and_f() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'H', &[3, 5], false, &mut r);
    assert_eq!(r.cursor_position, vec![(3, 5)]);

    CsiFastParser::dispatch(b'f', &[], false, &mut r);
    assert_eq!(r.cursor_position, vec![(3, 5), (1, 1)]);
}

#[test]
fn dispatch_default_params_min_1() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'A', &[], false, &mut r);
    assert_eq!(r.cursor_up, vec![1]);

    CsiFastParser::dispatch(b'B', &[], false, &mut r);
    assert_eq!(r.cursor_down, vec![1]);
}

#[test]
fn dispatch_private_mode() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'h', &[25], true, &mut r);
    assert_eq!(r.set_mode_private.last().copied(), Some(true));
    assert_eq!(r.set_mode_params.last(), Some(&vec![25]));

    CsiFastParser::dispatch(b'l', &[25], true, &mut r);
    assert_eq!(r.reset_mode_private.last().copied(), Some(true));
    assert_eq!(r.reset_mode_params.last(), Some(&vec![25]));
}

#[test]
fn dispatch_private_unknown_byte_is_noop() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'X', &[], true, &mut r);
}

#[test]
fn dispatch_unknown_final_byte_is_noop() {
    let mut r = Recorder::default();
    CsiFastParser::dispatch(b'&', &[], false, &mut r);
}

#[test]
fn parse_rgb_full_parameters() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"38;2;255;128;64m", &mut r);
    assert_eq!(consumed, Some(16));
    assert!(r.sgr_codes.contains(&255));
}

#[test]
fn parse_rgb_missing_semicolon_parses_as_single_param() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"38;2;25512864m", &mut r);
    assert!(consumed.is_some());
    assert!(r.sgr_codes.contains(&65535));
}

#[test]
fn parse_rgb_missing_m() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_parse(b"38;2;255;128;64", &mut r).is_none());
}

#[test]
fn try_inline_sgr_two_digit_three_params() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"1;2;3m", &mut r);
    assert_eq!(consumed, None);
}

#[test]
fn try_inline_non_digit_non_standard() {
    let mut r = Recorder::default();
    assert!(CsiFastParser::try_csi_inline(b"?25h", &mut r).is_none());
}

#[test]
fn try_inline_two_digit_first_semicolon() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_csi_inline(b"41;37m", &mut r);
    assert_eq!(consumed, Some(6));
    assert_eq!(r.sgr_codes, vec![41, 37]);
}

#[test]
fn try_parse_tab_clear_with_params() {
    let mut r = Recorder::default();
    let consumed = CsiFastParser::try_parse(b"3;0;1;3;0g", &mut r);
    assert_eq!(consumed, Some(10));
    assert_eq!(r.tab_clear, vec![3]);
}
