// Compile-time configuration constants.
// Change values here and recompile — no runtime config parsing.

/// Font family name (must be installed on the system)
pub const FONT_FAMILY: &str = "Hack";

/// Font size in points
pub const FONT_SIZE: f64 = 16.0;

/// Font smoothing weight: 0.0 = thinnest (min channel), 1.0 = medium (avg channel)
pub const FONT_SMOOTH_WEIGHT: f32 = 0.3;

/// Padding in logical pixels between window edge and cell grid
pub const PADDING: u32 = 0;

/// Maximum scrollback lines
pub const SCROLLBACK_LINES: usize = 10_000;

/// Default background color (palette index 0) — used for frame padding in shader
pub const DEFAULT_BG: u32 = 0x00181818;

/// xterm-256color palette: ANSI 0-15 (tweaked), 16-231 (6x6x6 cube), 232-255 (grayscale)
pub const PALETTE: [u32; 256] = {
    let mut p = [0u32; 256];

    // ANSI 0-7 (normal) — Alacritty's default theme
    p[0] = 0x00181818; // black (= background)
    p[1] = 0x00ac4242; // red
    p[2] = 0x0090a959; // green
    p[3] = 0x00f4bf75; // yellow
    p[4] = 0x006a9fb5; // blue
    p[5] = 0x00aa759f; // magenta
    p[6] = 0x0075b5aa; // cyan
    p[7] = 0x00d8d8d8; // white (= foreground)

    // ANSI 8-15 (bright)
    p[8] = 0x006b6b6b; // bright black
    p[9] = 0x00c55555; // bright red
    p[10] = 0x00aac474; // bright green
    p[11] = 0x00feca88; // bright yellow
    p[12] = 0x0082b8c8; // bright blue
    p[13] = 0x00c28cb8; // bright magenta
    p[14] = 0x0093d3c3; // bright cyan
    p[15] = 0x00f8f8f8; // bright white

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

/// Map an RGB color to the nearest palette entry.
/// Used when truecolor sequences (38;2;R;G;B) are received — we degrade to palette.
pub fn rgb_to_palette(r: u8, g: u8, b: u8) -> u8 {
    let mut best_index = 0;
    let mut best_distance = u32::MAX;

    for (index, &rgb) in PALETTE.iter().enumerate() {
        let pr = ((rgb >> 16) & 0xFF) as i32;
        let pg = ((rgb >> 8) & 0xFF) as i32;
        let pb = (rgb & 0xFF) as i32;
        let dr = r as i32 - pr;
        let dg = g as i32 - pg;
        let db = b as i32 - pb;
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_distance = distance;
            best_index = index as u8;
        }
    }

    best_index
}
