use std::ffi::c_void;
use std::mem::size_of;

use metal::*;

use tty::config;
use tty::terminal::cell::{Cell, CellFlags};

#[repr(C)]
struct ShaderUniforms {
    cols: u32,
    rows: u32,
    cell_width: u32,
    cell_height: u32,
    atlas_cell_width: u32,
    atlas_cell_height: u32,
    padding: u32,
    padding_top: u32,
    cursor_row: u32,
    cursor_col: u32,
    cursor_visible: u32,
    frame_bg: u32,
}

struct Ctx {
    device: Device,
    queue: CommandQueue,
    pipeline: ComputePipelineState,
}

fn setup() -> Ctx {
    let device = Device::system_default().expect("no Metal device found");
    let queue = device.new_command_queue();
    let src = include_str!("../src/renderer/shader.metal");
    let opts = CompileOptions::new();
    opts.set_fast_math_enabled(true);
    let lib = device
        .new_library_with_source(src, &opts)
        .expect("shader compilation failed");
    let func = lib
        .get_function("render", None)
        .expect("shader entry point 'render' not found");
    let pipeline = device
        .new_compute_pipeline_state_with_function(&func)
        .expect("compute pipeline creation failed");
    Ctx {
        device,
        queue,
        pipeline,
    }
}

fn f32_to_f16(val: f32) -> u16 {
    let bits = val.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    let frac = bits & 0x007F_FFFF;
    if exp <= 0 {
        return 0;
    }
    if exp >= 31 {
        return (sign | 0x7C00) as u16;
    }
    (sign | ((exp as u32) << 10) | (frac >> 13)) as u16
}

fn build_palette_data() -> Vec<u16> {
    let mut data = Vec::with_capacity(256 * 4);
    for &rgb in config::PALETTE.iter() {
        data.push(f32_to_f16(((rgb >> 16) & 0xFF) as f32 / 255.0));
        data.push(f32_to_f16(((rgb >> 8) & 0xFF) as f32 / 255.0));
        data.push(f32_to_f16((rgb & 0xFF) as f32 / 255.0));
        data.push(f32_to_f16(1.0));
    }
    data
}

/// Dispatch the render shader with the given grid state and return raw BGRA8 pixels.
fn render_pixels(
    ctx: &Ctx,
    cells: &[Cell],
    uniforms: &ShaderUniforms,
    out_w: u32,
    out_h: u32,
) -> Vec<u8> {
    render_pixels_with_atlas(ctx, cells, uniforms, out_w, out_h, None)
}

fn render_pixels_with_atlas(
    ctx: &Ctx,
    cells: &[Cell],
    uniforms: &ShaderUniforms,
    out_w: u32,
    out_h: u32,
    atlas_fill: Option<(u32, u32, u32, u32, u8)>,
) -> Vec<u8> {
    // Output texture
    let out_desc = TextureDescriptor::new();
    out_desc.set_texture_type(MTLTextureType::D2);
    out_desc.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
    out_desc.set_width(out_w as u64);
    out_desc.set_height(out_h as u64);
    out_desc.set_usage(MTLTextureUsage::ShaderWrite);
    out_desc.set_storage_mode(MTLStorageMode::Shared);
    let output = ctx.device.new_texture(&out_desc);

    // Atlas texture (blank — all zeros means "no glyph contribution")
    let atlas_desc = TextureDescriptor::new();
    atlas_desc.set_texture_type(MTLTextureType::D2);
    atlas_desc.set_pixel_format(MTLPixelFormat::R8Unorm);
    atlas_desc.set_width(256);
    atlas_desc.set_height(256);
    atlas_desc.set_usage(MTLTextureUsage::ShaderRead);
    atlas_desc.set_storage_mode(MTLStorageMode::Shared);
    let atlas = ctx.device.new_texture(&atlas_desc);
    let zero = vec![0u8; 256 * 256];
    atlas.replace_region(
        MTLRegion::new_2d(0, 0, 256, 256),
        0,
        zero.as_ptr() as *const c_void,
        256,
    );
    if let Some((x, y, width, height, value)) = atlas_fill {
        let data = vec![value; (width * height) as usize];
        atlas.replace_region(
            MTLRegion::new_2d(x as u64, y as u64, width as u64, height as u64),
            0,
            data.as_ptr() as *const c_void,
            width as u64,
        );
    }

    // Cell data buffer (Cell IS the GPU format)
    let cell_buf = ctx.device.new_buffer_with_data(
        cells.as_ptr() as *const c_void,
        std::mem::size_of_val(cells) as u64,
        MTLResourceOptions::StorageModeShared,
    );

    // Palette buffer (256 × half4)
    let pal_data = build_palette_data();
    let pal_buf = ctx.device.new_buffer_with_data(
        pal_data.as_ptr() as *const c_void,
        (pal_data.len() * 2) as u64,
        MTLResourceOptions::StorageModeShared,
    );

    // Uniform buffer
    let uni_buf = ctx.device.new_buffer_with_data(
        uniforms as *const _ as *const c_void,
        size_of::<ShaderUniforms>() as u64,
        MTLResourceOptions::StorageModeShared,
    );

    let cmd_buf = ctx.queue.new_command_buffer();
    let enc = cmd_buf.new_compute_command_encoder();
    enc.set_compute_pipeline_state(&ctx.pipeline);
    enc.set_texture(0, Some(&output));
    enc.set_texture(1, Some(&atlas));
    enc.set_buffer(0, Some(&cell_buf), 0);
    enc.set_buffer(1, Some(&pal_buf), 0);
    enc.set_buffer(2, Some(&uni_buf), 0);
    enc.dispatch_threads(
        MTLSize::new(out_w as u64, out_h as u64, 1),
        MTLSize::new(16, 16, 1),
    );
    enc.end_encoding();
    cmd_buf.commit();
    cmd_buf.wait_until_completed();

    // Read back pixels
    let bpr = out_w as u64 * 4;
    let mut pixels = vec![0u8; (out_h as usize) * (bpr as usize)];
    output.get_bytes(
        pixels.as_mut_ptr() as *mut c_void,
        bpr,
        MTLRegion::new_2d(0, 0, out_w as u64, out_h as u64),
        0,
    );
    pixels
}

/// Extract a BGRA pixel at pixel (x, y). Returns (b, g, r, a).
fn pixel_bgra(pixels: &[u8], x: u32, y: u32, out_w: u32) -> (u8, u8, u8, u8) {
    let i = (y as usize * out_w as usize + x as usize) * 4;
    (pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3])
}

/// Expected BGRA pixel value from a palette color (0x00RRGGBB).
fn pal_to_bgra(rgb: u32) -> (u8, u8, u8, u8) {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    (b, g, r, 255)
}

/// One-cell grid with the given cell content.
fn single_cell_test(
    cell: Cell,
    cell_w: u32,
    cell_h: u32,
    _palette_bg: u32,
    cursor_row: u32,
    cursor_col: u32,
    cursor_visible: u32,
) -> (Vec<u8>, u32, u32) {
    let ctx = setup();
    let cols = 1;
    let rows = 1;
    let out_w = cols * cell_w;
    let out_h = rows * cell_h;
    let uniforms = ShaderUniforms {
        cols,
        rows,
        cell_width: cell_w,
        cell_height: cell_h,
        atlas_cell_width: cell_w,
        atlas_cell_height: cell_h,
        padding: 0,
        padding_top: 0,
        cursor_row,
        cursor_col,
        cursor_visible,
        frame_bg: 0,
    };
    let pixels = render_pixels(&ctx, &[cell], &uniforms, out_w, out_h);
    (pixels, out_w, out_h)
}

/// Helper to get all pixels that should be in the cell interior area
/// (index range for rows and columns within the cell).
fn in_cell_pixels(
    pixels: &[u8],
    out_w: u32,
    top: u32,
    left: u32,
    w: u32,
    h: u32,
) -> Vec<(u8, u8, u8, u8)> {
    let mut ps = Vec::new();
    for y in top..top + h {
        for x in left..left + w {
            ps.push(pixel_bgra(pixels, x, y, out_w));
        }
    }
    ps
}

fn all_same(pixels: &[(u8, u8, u8, u8)], expected: (u8, u8, u8, u8)) -> bool {
    pixels.iter().all(|&p| p == expected)
}

#[test]
fn empty_cell_renders_background() {
    let cell = Cell {
        codepoint: 0,
        flags: CellFlags::empty(),
        fg_index: 7,
        bg_index: 0,
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let ps = in_cell_pixels(&pixels, out_w, 0, 0, 8, 16);
    assert!(
        all_same(&ps, pal_to_bgra(config::PALETTE[0])),
        "empty cell should be palette[0] (bg)"
    );
}

#[test]
fn cell_layout_matches_shader() {
    assert_eq!(size_of::<Cell>(), 8);
    assert_eq!(std::mem::offset_of!(Cell, codepoint), 0);
    assert_eq!(std::mem::offset_of!(Cell, flags), 2);
    assert_eq!(std::mem::offset_of!(Cell, fg_index), 4);
    assert_eq!(std::mem::offset_of!(Cell, bg_index), 5);
    assert_eq!(std::mem::offset_of!(Cell, atlas_x), 6);
    assert_eq!(std::mem::offset_of!(Cell, atlas_y), 7);
}

#[test]
fn atlas_glyph_contributes_foreground_pixels() {
    let ctx = setup();
    let cell = Cell {
        codepoint: b'A' as u16,
        flags: CellFlags::empty(),
        fg_index: 7,
        bg_index: 0,
        atlas_x: 1,
        atlas_y: 0,
    };
    let uniforms = ShaderUniforms {
        cols: 1,
        rows: 1,
        cell_width: 8,
        cell_height: 16,
        atlas_cell_width: 8,
        atlas_cell_height: 16,
        padding: 0,
        padding_top: 0,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: 0,
        frame_bg: 0,
    };
    let pixels =
        render_pixels_with_atlas(&ctx, &[cell], &uniforms, 8, 16, Some((8, 0, 8, 16, 255)));

    assert_eq!(
        pixel_bgra(&pixels, 0, 0, 8),
        pal_to_bgra(config::PALETTE[7])
    );
}

#[test]
fn box_drawing_horizontal_line() {
    // ─ (U+2500): light horizontal line at vertical center
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::empty(),
        fg_index: 7, // white
        bg_index: 0, // black
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _out_h) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_fg = pal_to_bgra(config::PALETTE[7]); // white
    let expected_bg = pal_to_bgra(config::PALETTE[0]); // black

    let cy = 8u32; // cell_height / 2
    // Center row should be fg (the horizontal line)
    for x in 0..8 {
        assert_eq!(
            pixel_bgra(&pixels, x, cy, out_w),
            expected_fg,
            "box line at row {cy} col {x}"
        );
    }
    // Top row should be bg
    for x in 0..8 {
        assert_eq!(
            pixel_bgra(&pixels, x, 0, out_w),
            expected_bg,
            "box top at col {x}"
        );
    }
}

#[test]
fn bold_remaps_to_bright() {
    // fg=1 (dark red) + BOLD → remap to 9 (bright red)
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::BOLD,
        fg_index: 1,
        bg_index: 0,
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_fg = pal_to_bgra(config::PALETTE[9]); // bright red, not dark red

    let cy = 8u32;
    for x in 0..8 {
        assert_eq!(
            pixel_bgra(&pixels, x, cy, out_w),
            expected_fg,
            "bold remap at col {x}"
        );
    }
}

#[test]
fn hidden_matches_background() {
    // HIDDEN: fg = bg, so the box line should be invisible (bg color)
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::HIDDEN,
        fg_index: 7, // white
        bg_index: 1, // red
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected = pal_to_bgra(config::PALETTE[1]); // bg = red

    let ps = in_cell_pixels(&pixels, out_w, 0, 0, 8, 16);
    assert!(
        all_same(&ps, expected),
        "hidden cell should be all bg color"
    );
}

#[test]
fn inverse_swaps_fg_and_bg() {
    // INVERSE: cell background = fg(white), box line = bg(black)
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::INVERSE,
        fg_index: 7, // white
        bg_index: 0, // black
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_bg = pal_to_bgra(config::PALETTE[7]); // swapped: bg is white
    let expected_fg = pal_to_bgra(config::PALETTE[0]); // swapped: fg (box line) is black

    // Cell background should be white (was fg)
    assert_eq!(
        pixel_bgra(&pixels, 0, 0, out_w),
        expected_bg,
        "inverse top-left"
    );
    // Box line should be black (was bg)
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        expected_fg,
        "inverse box line"
    );
}

#[test]
fn underline_renders_at_bottom() {
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::UNDERLINE,
        fg_index: 7,
        bg_index: 0,
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_fg = pal_to_bgra(config::PALETTE[7]);

    // Underline at cell_height - 2 = 14
    for x in 0..8 {
        assert_eq!(
            pixel_bgra(&pixels, x, 14, out_w),
            expected_fg,
            "underline at col {x}"
        );
    }
    // Line above underline should NOT be fg (it's a box drawing pixel, not underline)
    // Actually, cell_height - 3 = 13 — this is the box drawing area since 2500 has center line at cy.
    // With a horizontal line box AND underline: the center row (8) and underline row (14) should be fg.
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        expected_fg,
        "box line preserved under underline"
    );
}

#[test]
fn strikethrough_renders_at_mid() {
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::STRIKE,
        fg_index: 7,
        bg_index: 0,
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_fg = pal_to_bgra(config::PALETTE[7]);

    // Strikethrough at cell_height / 2 = 8
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        expected_fg,
        "strikethrough at mid"
    );
}

#[test]
fn cursor_inverts_pixel() {
    // With cursor at (0,0): background → inverted bg (1 - bg = white)
    let cell = Cell {
        codepoint: 0,
        flags: CellFlags::empty(),
        fg_index: 7,
        bg_index: 0, // black
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 1);
    // bg = black → inverted = (1 - 0, 1 - 0, 1 - 0) = white
    let inverted: (u8, u8, u8, u8) = (255, 255, 255, 255);

    let ps = in_cell_pixels(&pixels, out_w, 0, 0, 8, 16);
    assert!(
        all_same(&ps, inverted),
        "cursor inverts entire cell to white"
    );
}

#[test]
fn cursor_inverts_fg_and_box_drawing() {
    // Box drawing with cursor: box line is fg (white) → inverted → black
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::empty(),
        fg_index: 7, // white
        bg_index: 0, // black
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 1);
    // At cursor row/col: color is inverted via 1.0 - color
    // Box line: white (1,1,1,1) → inverted → black (0,0,0,1)
    // Background: black (0,0,0,1) → inverted → white (1,1,1,1)
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        (0, 0, 0, 255),
        "cursor inverts box line white→black"
    );
    assert_eq!(
        pixel_bgra(&pixels, 0, 0, out_w),
        (255, 255, 255, 255),
        "cursor inverts bg black→white"
    );
}

#[test]
fn selected_inverts_like_inverse() {
    // SELECTED swaps fg/bg (same logic as INVERSE)
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::SELECTED,
        fg_index: 7, // white
        bg_index: 0, // black
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    // Background should be fg (white), box line should be bg (black)
    assert_eq!(
        pixel_bgra(&pixels, 0, 0, out_w),
        pal_to_bgra(config::PALETTE[7]),
        "selected bg white"
    );
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        pal_to_bgra(config::PALETTE[0]),
        "selected box line black"
    );
}

#[test]
fn padding_is_background() {
    let ctx = setup();
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::empty(),
        fg_index: 7,
        bg_index: 1, // red cell bg (won't be visible in padding)
        atlas_x: 0,
        atlas_y: 0,
    };
    let cols = 1;
    let rows = 1;
    let cell_w = 8;
    let cell_h = 16;
    let padding = 8;
    let out_w = cols * cell_w + padding * 2;
    let out_h = rows * cell_h + padding + padding;
    let uniforms = ShaderUniforms {
        cols,
        rows,
        cell_width: cell_w,
        cell_height: cell_h,
        atlas_cell_width: cell_w,
        atlas_cell_height: cell_h,
        padding,
        padding_top: padding,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: 0,
        frame_bg: 0, // black
    };
    let pixels = render_pixels(&ctx, &[cell], &uniforms, out_w, out_h);
    let expected_bg: (u8, u8, u8, u8) = (0, 0, 0, 255); // frame_bg = black

    // Corner pixel
    assert_eq!(
        pixel_bgra(&pixels, 0, 0, out_w),
        expected_bg,
        "padding top-left"
    );
    // Edge of padding
    assert_eq!(
        pixel_bgra(&pixels, 7, 7, out_w),
        expected_bg,
        "padding just before cell area"
    );
    // Inside cell (past padding) — should be cell bg (red)
    assert_eq!(
        pixel_bgra(&pixels, 8, 8, out_w),
        pal_to_bgra(config::PALETTE[1]),
        "cell interior after padding"
    );
}

#[test]
fn multiple_cells_have_correct_bg() {
    let ctx = setup();
    // 2×1 grid: cell0 bg=red, cell1 bg=green
    let cells = [
        Cell {
            codepoint: 0x2500,
            flags: CellFlags::empty(),
            fg_index: 7,
            bg_index: 1, // red
            atlas_x: 0,
            atlas_y: 0,
        },
        Cell {
            codepoint: 0x2500,
            flags: CellFlags::empty(),
            fg_index: 7,
            bg_index: 2, // green
            atlas_x: 0,
            atlas_y: 0,
        },
    ];
    let cols = 2;
    let rows = 1;
    let cell_w = 8;
    let cell_h = 16;
    let out_w = cols * cell_w;
    let out_h = rows * cell_h;
    let uniforms = ShaderUniforms {
        cols,
        rows,
        cell_width: cell_w,
        cell_height: cell_h,
        atlas_cell_width: cell_w,
        atlas_cell_height: cell_h,
        padding: 0,
        padding_top: 0,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: 0,
        frame_bg: 0,
    };
    let pixels = render_pixels(&ctx, &cells, &uniforms, out_w, out_h);

    assert_eq!(
        pixel_bgra(&pixels, 0, 0, out_w),
        pal_to_bgra(config::PALETTE[1]),
        "cell0 bg red"
    );
    assert_eq!(
        pixel_bgra(&pixels, 8, 0, out_w),
        pal_to_bgra(config::PALETTE[2]),
        "cell1 bg green"
    );
}

#[test]
fn bold_no_remap_for_fg_above_7() {
    // BOLD with fg=8 (bright black, already in bright range) should stay at 8
    let cell = Cell {
        codepoint: 0x2500,
        flags: CellFlags::BOLD,
        fg_index: 8,
        bg_index: 0,
        atlas_x: 0,
        atlas_y: 0,
    };
    let (pixels, out_w, _) = single_cell_test(cell, 8, 16, 0, 0, 0, 0);
    let expected_fg = pal_to_bgra(config::PALETTE[8]); // bright black
    assert_eq!(
        pixel_bgra(&pixels, 0, 8, out_w),
        expected_fg,
        "bold should not remap fg >= 8"
    );
}
