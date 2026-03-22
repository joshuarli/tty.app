#include <metal_stdlib>
using namespace metal;

// Must match Rust Cell layout exactly (8 bytes)
struct CellData {
    ushort codepoint;
    ushort flags;
    uchar  fg_index;
    uchar  bg_index;
    uchar  atlas_x;
    uchar  atlas_y;
};

struct Uniforms {
    uint cols;
    uint rows;
    uint cell_width;
    uint cell_height;
    uint atlas_cell_width;
    uint atlas_cell_height;
    uint padding;
    uint padding_top;
    uint cursor_row;
    uint cursor_col;
    uint cursor_visible;
    uint frame_bg;       // 0x00RRGGBB default background
};

// Cell flag bits
constant ushort FLAG_WIDE       = 0x0001;
constant ushort FLAG_WIDE_CONT  = 0x0002;
constant ushort FLAG_UNDERLINE  = 0x0004;
constant ushort FLAG_STRIKE     = 0x0008;
constant ushort FLAG_INVERSE    = 0x0010;
constant ushort FLAG_SELECTED   = 0x0040;
constant ushort FLAG_BOLD       = 0x0080;
constant ushort FLAG_HIDDEN     = 0x0400;

// ── Box drawing lookup ──────────────────────────────────────────────
// Each entry encodes edge connectivity for U+2500..U+257F
// Bits: [0] right, [1] left, [2] down, [3] up
//       [4] right-heavy, [5] left-heavy, [6] down-heavy, [7] up-heavy
// 0 = no line on that edge
constant uchar BOX_EDGES[128] = {
    // U+2500 ─  U+2501 ━  U+2502 │  U+2503 ┃
    0x03, 0x33, 0x0C, 0xCC,
    // U+2504..U+250B (dashed variants, treat as solid)
    0x03, 0x33, 0x03, 0x33, 0x0C, 0xCC, 0x0C, 0xCC,
    // U+250C ┌  U+250D ┍  U+250E ┎  U+250F ┏
    0x05, 0x15, 0x45, 0x55,
    // U+2510 ┐  U+2511 ┑  U+2512 ┒  U+2513 ┓
    0x06, 0x26, 0x46, 0x66,
    // U+2514 └  U+2515 ┕  U+2516 ┖  U+2517 ┗
    0x09, 0x19, 0x89, 0x99,
    // U+2518 ┘  U+2519 ┙  U+251A ┚  U+251B ┛
    0x0A, 0x2A, 0x8A, 0xAA,
    // U+251C ├  U+251D ┝  U+251E ┞  U+251F ┟
    0x0D, 0x1D, 0x8D, 0x4D,
    // U+2520 ┠  U+2521 ┡  U+2522 ┢  U+2523 ┣
    0xCD, 0x9D, 0x5D, 0xDD,
    // U+2524 ┤  U+2525 ┥  U+2526 ┦  U+2527 ┧
    0x0E, 0x2E, 0x8E, 0x4E,
    // U+2528 ┨  U+2529 ┩  U+252A ┪  U+252B ┫
    0xCE, 0xAE, 0x6E, 0xEE,
    // U+252C ┬  U+252D ┭  U+252E ┮  U+252F ┯
    0x07, 0x27, 0x17, 0x37,
    // U+2530 ┰  U+2531 ┱  U+2532 ┲  U+2533 ┳
    0x47, 0x67, 0x57, 0x77,
    // U+2534 ┴  U+2535 ┵  U+2536 ┶  U+2537 ┷
    0x0B, 0x2B, 0x1B, 0x3B,
    // U+2538 ┸  U+2539 ┹  U+253A ┺  U+253B ┻
    0x8B, 0xAB, 0x9B, 0xBB,
    // U+253C ┼  U+253D ┽  U+253E ┾  U+253F ┿
    0x0F, 0x2F, 0x1F, 0x3F,
    // U+2540 ╀  U+2541 ╁  U+2542 ╂  U+2543 ╃
    0x8F, 0x4F, 0xCF, 0xAF,
    // U+2544 ╄  U+2545 ╅  U+2546 ╆  U+2547 ╇
    0x9F, 0x6F, 0x5F, 0xBF,
    // U+2548 ╈  U+2549 ╉  U+254A ╊  U+254B ╋
    0x7F, 0xEF, 0xDF, 0xFF,
    // U+254C..U+254F (dashed, treat as solid light)
    0x03, 0x33, 0x0C, 0xCC,
    // U+2550 ═  U+2551 ║  U+2552 ╒  U+2553 ╓
    0x03, 0x0C, 0x05, 0x05,
    // U+2554 ╔  U+2555 ╕  U+2556 ╖  U+2557 ╗
    0x05, 0x06, 0x06, 0x06,
    // U+2558 ╘  U+2559 ╙  U+255A ╚  U+255B ╛
    0x09, 0x09, 0x09, 0x0A,
    // U+255C ╜  U+255D ╝  U+255E ╞  U+255F ╟
    0x0A, 0x0A, 0x0D, 0x0D,
    // U+2560 ╠  U+2561 ╡  U+2562 ╢  U+2563 ╣
    0x0D, 0x0E, 0x0E, 0x0E,
    // U+2564 ╤  U+2565 ╥  U+2566 ╦  U+2567 ╧
    0x07, 0x07, 0x07, 0x0B,
    // U+2568 ╨  U+2569 ╩  U+256A ╪  U+256B ╫
    0x0B, 0x0B, 0x0F, 0x0F,
    // U+256C ╬  U+256D ╭  U+256E ╮  U+256F ╯
    0x0F, 0x05, 0x06, 0x0A,
    // U+2570 ╰  U+2571 ╱  U+2572 ╲  U+2573 ╳
    0x09, 0x00, 0x00, 0x00,
    // U+2574 ╴  U+2575 ╵  U+2576 ╶  U+2577 ╷
    0x02, 0x08, 0x01, 0x04,
    // U+2578 ╸  U+2579 ╹  U+257A ╺  U+257B ╻
    0x22, 0x88, 0x11, 0x44,
    // U+257C ╼  U+257D ╽  U+257E ╾  U+257F ╿
    0x13, 0x4C, 0x31, 0xC4,
};

// ── Arrow drawing ─────────────────────────────────────────────────
// Procedural arrows for U+2190..U+2195 (← ↑ → ↓ ↔ ↕)
// Shaft uses identical dimensions to box-drawing light lines so they
// connect seamlessly with adjacent ─ or │ characters.
static inline bool draw_arrow(uint cp, uint px, uint py,
                               uint cw, uint ch) {
    if (cp < 0x2190 || cp > 0x2195) return false;

    uint cx = cw / 2;
    uint cy = ch / 2;
    uint light_w = max(1u, cw / 8);

    bool hit = false;

    // Horizontal arrows: ← (2190), → (2192), ↔ (2194)
    if (cp == 0x2190 || cp == 0x2192 || cp == 0x2194) {
        // Shaft — same as box-drawing light horizontal line
        if (py >= cy - light_w/2 && py < cy + (light_w+1)/2) hit = true;

        uint head_len  = max(3u, cw / 2);
        uint head_half = max(3u, ch / 4);
        uint dy = uint(abs(int(py) - int(cy)));

        // Left arrowhead (← or ↔): tip at px=0, base at px=head_len
        if ((cp == 0x2190 || cp == 0x2194) && px < head_len) {
            if (dy * head_len <= head_half * px) hit = true;
        }
        // Right arrowhead (→ or ↔): tip at px=cw-1, base at px=cw-head_len
        if ((cp == 0x2192 || cp == 0x2194) && px >= cw - head_len) {
            uint rdx = cw - 1 - px;
            if (dy * head_len <= head_half * rdx) hit = true;
        }
    }

    // Vertical arrows: ↑ (2191), ↓ (2193), ↕ (2195)
    if (cp == 0x2191 || cp == 0x2193 || cp == 0x2195) {
        // Shaft — same as box-drawing light vertical line
        if (px >= cx - light_w/2 && px < cx + (light_w+1)/2) hit = true;

        uint vhead_len  = max(3u, ch / 3);
        uint vhead_half = max(2u, cw / 3);
        uint dx = uint(abs(int(px) - int(cx)));

        // Up arrowhead (↑ or ↕): tip at py=0, base at py=vhead_len
        if ((cp == 0x2191 || cp == 0x2195) && py < vhead_len) {
            if (dx * vhead_len <= vhead_half * py) hit = true;
        }
        // Down arrowhead (↓ or ↕): tip at py=ch-1, base at py=ch-vhead_len
        if ((cp == 0x2193 || cp == 0x2195) && py >= ch - vhead_len) {
            uint rdy = ch - 1 - py;
            if (dx * vhead_len <= vhead_half * rdy) hit = true;
        }
    }

    return hit;
}

static inline half4 unpack_rgb(uint rgb) {
    return half4(
        half((rgb >> 16) & 0xFF) / 255.0h,
        half((rgb >> 8)  & 0xFF) / 255.0h,
        half( rgb        & 0xFF) / 255.0h,
        1.0h
    );
}

static inline bool draw_box_line(uint cp, uint px, uint py,
                                  uint cw, uint ch) {
    if (cp < 0x2500 || cp > 0x257F) return false;
    uchar edges = BOX_EDGES[cp - 0x2500];
    if (edges == 0) return false;

    uint cx = cw / 2;
    uint cy = ch / 2;
    uint light_w = max(1u, cw / 8);
    uint heavy_w = max(2u, cw / 4);

    bool hit = false;

    // Right edge
    if (edges & 0x01) {
        uint w = (edges & 0x10) ? heavy_w : light_w;
        if (px >= cx && py >= cy - w/2 && py < cy + (w+1)/2) hit = true;
    }
    // Left edge
    if (edges & 0x02) {
        uint w = (edges & 0x20) ? heavy_w : light_w;
        if (px <= cx && py >= cy - w/2 && py < cy + (w+1)/2) hit = true;
    }
    // Down edge
    if (edges & 0x04) {
        uint w = (edges & 0x40) ? heavy_w : light_w;
        if (py >= cy && px >= cx - w/2 && px < cx + (w+1)/2) hit = true;
    }
    // Up edge
    if (edges & 0x08) {
        uint w = (edges & 0x80) ? heavy_w : light_w;
        if (py <= cy && px >= cx - w/2 && px < cx + (w+1)/2) hit = true;
    }

    return hit;
}

kernel void render(
    texture2d<half, access::write>  output     [[texture(0)]],
    texture2d<half, access::read>   atlas      [[texture(1)]],
    device const CellData*          cells      [[buffer(0)]],
    device const half4*             palette    [[buffer(1)]],
    constant Uniforms&              uni        [[buffer(2)]],
    uint2 gid [[thread_position_in_grid]]
) {
    uint out_w = output.get_width();
    uint out_h = output.get_height();
    if (gid.x >= out_w || gid.y >= out_h) return;

    half4 bg_default = unpack_rgb(uni.frame_bg);

    // Padding region
    int2 pos = int2(gid) - int2(uni.padding, uni.padding_top);
    if (pos.x < 0 || pos.y < 0 ||
        uint(pos.x) >= uni.cols * uni.cell_width ||
        uint(pos.y) >= uni.rows * uni.cell_height) {
        output.write(bg_default, gid);
        return;
    }

    uint col = uint(pos.x) / uni.cell_width;
    uint row = uint(pos.y) / uni.cell_height;
    if (col >= uni.cols || row >= uni.rows) {
        output.write(bg_default, gid);
        return;
    }

    uint px = uint(pos.x) % uni.cell_width;   // pixel within cell
    uint py = uint(pos.y) % uni.cell_height;

    CellData cell = cells[row * uni.cols + col];

    // Wide continuation — the cell carries the owner's colors and atlas coords
    if (cell.flags & FLAG_WIDE_CONT) {
        half4 fg = palette[cell.fg_index];
        half4 bg = palette[cell.bg_index];
        if (cell.flags & FLAG_INVERSE) { half4 tmp = fg; fg = bg; bg = tmp; }
        if (cell.flags & FLAG_HIDDEN) fg = bg;
        // Offset px to sample the right half of the wide glyph
        px += uni.cell_width;
        uint atlas_px = uint(cell.atlas_x) * uni.atlas_cell_width + px;
        uint atlas_py = uint(cell.atlas_y) * uni.atlas_cell_height + py;
        half alpha = atlas.read(uint2(atlas_px, atlas_py)).r;
        half4 color = mix(bg, fg, alpha);
        output.write(color, gid);
        return;
    }

    // Bold: map palette 0-7 → 8-15 for bright colors
    uchar fg_idx = cell.fg_index;
    if ((cell.flags & FLAG_BOLD) && fg_idx < 8) {
        fg_idx += 8;
    }

    // Resolve fg/bg from palette
    half4 fg = palette[fg_idx];
    half4 bg = palette[cell.bg_index];

    // Hidden: make fg match bg
    if (cell.flags & FLAG_HIDDEN) {
        fg = bg;
    }

    // Inverse
    if (cell.flags & FLAG_INVERSE) {
        half4 tmp = fg;
        fg = bg;
        bg = tmp;
    }

    // Selection highlight (also invert)
    if (cell.flags & FLAG_SELECTED) {
        half4 tmp = fg;
        fg = bg;
        bg = tmp;
    }

    // Start with background
    half4 color = bg;

    // Box drawing (procedural)
    uint cp = uint(cell.codepoint);
    if (cp >= 0x2500 && cp <= 0x257F) {
        if (draw_box_line(cp, px, py, uni.cell_width, uni.cell_height)) {
            color = fg;
        }
    } else if (cp >= 0x2190 && cp <= 0x2195) {
        if (draw_arrow(cp, px, py, uni.cell_width, uni.cell_height)) {
            color = fg;
        }
    } else if (cell.atlas_x != 0 || cell.atlas_y != 0) {
        // Glyph from atlas
        uint glyph_w = (cell.flags & FLAG_WIDE) ? uni.atlas_cell_width * 2 : uni.atlas_cell_width;
        if (px < glyph_w && py < uni.atlas_cell_height) {
            uint atlas_px = uint(cell.atlas_x) * uni.atlas_cell_width + px;
            uint atlas_py = uint(cell.atlas_y) * uni.atlas_cell_height + py;
            half alpha = atlas.read(uint2(atlas_px, atlas_py)).r;
            color = mix(bg, fg, alpha);
        }
    }

    // Underline (1px line near bottom of cell)
    if (cell.flags & FLAG_UNDERLINE) {
        uint underline_y = uni.cell_height - 2;
        if (py == underline_y) {
            color = fg;
        }
    }

    // Strikethrough (1px line at middle of cell)
    if (cell.flags & FLAG_STRIKE) {
        uint strike_y = uni.cell_height / 2;
        if (py == strike_y) {
            color = fg;
        }
    }

    // Cursor (block = invert entire cell) — position from uniforms, not cell flags
    if (row == uni.cursor_row && col == uni.cursor_col && uni.cursor_visible != 0) {
        color = half4(1.0h - color.r, 1.0h - color.g, 1.0h - color.b, 1.0h);
    }

    output.write(color, gid);
}
