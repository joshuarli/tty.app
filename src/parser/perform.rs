/// Interface between the VT parser and the terminal grid.
/// The parser calls these methods as it decodes escape sequences.
pub trait Perform {
    /// Bulk printable ASCII — hot path.
    fn print_ascii_run(&mut self, bytes: &[u8]);

    /// Single Unicode character (after charset translation).
    fn print(&mut self, c: char);

    /// C0 control character (CR, LF, BS, TAB, BEL, etc.)
    fn execute(&mut self, byte: u8);

    /// Cursor movement
    fn cursor_up(&mut self, n: u16);
    fn cursor_down(&mut self, n: u16);
    fn cursor_forward(&mut self, n: u16);
    fn cursor_backward(&mut self, n: u16);
    fn cursor_position(&mut self, row: u16, col: u16);
    fn cursor_horizontal_absolute(&mut self, col: u16);
    fn cursor_vertical_absolute(&mut self, row: u16);

    /// Erase
    fn erase_in_display(&mut self, mode: u16);
    fn erase_in_line(&mut self, mode: u16);

    /// Scroll
    fn scroll_up(&mut self, n: u16);
    fn scroll_down(&mut self, n: u16);

    /// Insert/delete
    fn insert_lines(&mut self, n: u16);
    fn delete_lines(&mut self, n: u16);
    fn insert_chars(&mut self, n: u16);
    fn delete_chars(&mut self, n: u16);
    fn erase_chars(&mut self, n: u16);

    /// SGR (Select Graphic Rendition) with raw params
    fn sgr(&mut self, params: &[u16]);

    /// SGR reset (\x1b[0m). Default delegates to sgr(&[0]).
    /// Override for zero-overhead attribute reset.
    #[inline]
    fn sgr_reset(&mut self) {
        self.sgr(&[0]);
    }

    /// Single-param SGR (\x1b[Nm or \x1b[NNm). Default delegates to sgr(&[code]).
    /// Override to avoid the param-slice + loop overhead.
    #[inline]
    fn sgr_single(&mut self, code: u16) {
        self.sgr(&[code]);
    }

    /// 256-color: \x1b[38;5;Nm (fg=true) or \x1b[48;5;Nm (fg=false).
    /// Default delegates to sgr(); override for zero-overhead color setting.
    #[inline]
    fn color_256(&mut self, fg: bool, index: u16) {
        if fg {
            self.sgr(&[38, 5, index]);
        } else {
            self.sgr(&[48, 5, index]);
        }
    }

    /// Truecolor: \x1b[38;2;R;G;Bm (fg=true) or \x1b[48;2;R;G;Bm (fg=false).
    /// Default delegates to sgr(); override for zero-overhead color setting.
    #[inline]
    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        if fg {
            self.sgr(&[38, 2, r, g, b]);
        } else {
            self.sgr(&[48, 2, r, g, b]);
        }
    }

    /// Set/reset modes
    fn set_mode(&mut self, params: &[u16], private: bool);
    fn reset_mode(&mut self, params: &[u16], private: bool);

    /// Set scroll region
    fn set_scroll_region(&mut self, top: u16, bottom: u16);

    /// Tab clear / set
    fn tab_clear(&mut self, mode: u16);
    fn set_tab_stop(&mut self);

    /// OSC (Operating System Command)
    fn osc_dispatch(&mut self, params: &[&[u8]]);

    /// ESC dispatch (non-CSI escape sequences)
    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8);

    /// General CSI dispatch (fallback from state machine)
    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], ignore: bool, byte: u8);

    /// Save/restore cursor
    fn save_cursor(&mut self);
    fn restore_cursor(&mut self);

    /// Device status report
    fn device_status_report(&mut self, mode: u16);

    /// Cursor style (DECSCUSR) — we only support block, so this is a no-op
    fn set_cursor_style(&mut self, _style: u16) {}

    /// REP (CSI Ps b) — repeat the last printed character Ps times
    fn repeat_char(&mut self, n: u16);

    /// SGR with colon sub-parameters (raw param bytes, e.g. "4:3" or "38:2::255:0:0;1")
    fn sgr_colon(&mut self, raw: &[u8]);
}
