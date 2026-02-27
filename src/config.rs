// Compile-time configuration constants.
// Change values here and recompile — no runtime config parsing.

/// Font family name (must be installed on the system)
pub const FONT_FAMILY: &str = "Hack";

/// Font size in points
pub const FONT_SIZE: f64 = 16.0;

/// Font smoothing weight: 0.0 = thinnest (min channel), 1.0 = medium (avg channel)
pub const FONT_SMOOTH_WEIGHT: f32 = 0.3;

/// Padding in logical pixels between window edge and cell grid
pub const PADDING: u32 = 8;

/// Maximum scrollback lines
pub const SCROLLBACK_LINES: usize = 10_000;

/// Default foreground color (palette index 7)
pub const DEFAULT_FG: u32 = 0x00ffffff;

/// Default background color (palette index 0)
pub const DEFAULT_BG: u32 = 0x00000000;

/// Cursor blink interval in milliseconds
pub const CURSOR_BLINK_MS: u64 = 500;

/// xterm-256color palette: ANSI 0-15 (tweaked), 16-231 (6x6x6 cube), 232-255 (grayscale)
pub const PALETTE: [u32; 256] = {
    let mut p = [0u32; 256];

    // ANSI 0-7 (normal) — Dracula-inspired
    p[0] = 0x00000000; // black (= background)
    p[1] = 0x00ff5555; // red
    p[2] = 0x0050fa7b; // green
    p[3] = 0x00f1fa8c; // yellow
    p[4] = 0x00caa9fa; // blue
    p[5] = 0x00ff79c6; // magenta
    p[6] = 0x008be9fd; // cyan
    p[7] = 0x00ffffff; // white (= foreground)

    // ANSI 8-15 (bright)
    p[8] = 0x00666666; // bright black
    p[9] = 0x00ff6e6e; // bright red
    p[10] = 0x0069ff94; // bright green
    p[11] = 0x00ffffa5; // bright yellow
    p[12] = 0x00d6bfff; // bright blue
    p[13] = 0x00ff92df; // bright magenta
    p[14] = 0x00a4ffff; // bright cyan
    p[15] = 0x00ffffff; // bright white

    // 216 color cube (indices 16-231)
    let levels: [u8; 6] = [0, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let mut i = 16usize;
    let mut r = 0usize;
    while r < 6 {
        let mut g = 0usize;
        while g < 6 {
            let mut b = 0usize;
            while b < 6 {
                p[i] = ((levels[r] as u32) << 16) | ((levels[g] as u32) << 8) | (levels[b] as u32);
                i += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    // Grayscale ramp (indices 232-255)
    let mut j = 0usize;
    while j < 24 {
        let v = (8 + 10 * j) as u32;
        p[232 + j] = (v << 16) | (v << 8) | v;
        j += 1;
    }

    p
};
