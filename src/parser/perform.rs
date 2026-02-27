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

    /// SGR (Select Graphic Rendition) with raw params
    fn sgr(&mut self, params: &[u16]);

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
}
