use core_graphics::base::kCGImageAlphaNoneSkipLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::font::CGGlyph;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{self as ct_font, CTFont};
use core_text::font_descriptor::kCTFontOrientationDefault;

/// Rasterized glyph data (grayscale alpha).
pub struct RasterizedGlyph {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Font metrics for the monospace grid.
#[derive(Clone, Debug)]
pub struct FontMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    pub descent: f64,
}

pub struct FontRasterizer {
    ct_font: CTFont,
    pub metrics: FontMetrics,
}

impl FontRasterizer {
    pub fn new(family: &str, size: f64, scale: f64) -> Self {
        let font_size = size * scale;
        let ct_font = ct_font::new_from_name(family, font_size).expect("failed to load font");

        let ascent = ct_font.ascent();
        let descent = ct_font.descent();
        let leading = ct_font.leading();

        // Get advance width from a reference glyph ('M')
        let characters: [u16; 1] = [b'M' as u16];
        let mut glyphs: [CGGlyph; 1] = [0];
        unsafe {
            ct_font.get_glyphs_for_characters(
                characters.as_ptr(),
                glyphs.as_mut_ptr(),
                1,
            );
        }

        let mut advances = [CGSize::new(0.0, 0.0)];
        unsafe {
            ct_font.get_advances_for_glyphs(
                kCTFontOrientationDefault,
                glyphs.as_ptr(),
                advances.as_mut_ptr(),
                1,
            );
        }

        let cell_width = advances[0].width.ceil() as u32;
        let cell_height = (ascent + descent + leading).ceil() as u32;

        let metrics = FontMetrics {
            cell_width,
            cell_height,
            descent,
        };

        Self { ct_font, metrics }
    }

    /// Rasterize a single codepoint into an R8 alpha bitmap.
    /// Returns None if the glyph is missing.
    pub fn rasterize(&self, codepoint: u16) -> Option<RasterizedGlyph> {
        let characters = [codepoint];
        let mut glyphs: [CGGlyph; 1] = [0];
        let result = unsafe {
            self.ct_font.get_glyphs_for_characters(
                characters.as_ptr(),
                glyphs.as_mut_ptr(),
                1,
            )
        };
        if !result || glyphs[0] == 0 {
            return None;
        }

        let glyph = glyphs[0];
        let m = &self.metrics;

        let w = m.cell_width as usize;
        let h = m.cell_height as usize;
        if w == 0 || h == 0 {
            return None;
        }

        // Create RGBA context (CoreText requires color context)
        let color_space = CGColorSpace::create_device_rgb();
        let mut ctx = CGContext::create_bitmap_context(
            None,
            w,
            h,
            8,
            w * 4,
            &color_space,
            kCGImageAlphaNoneSkipLast,
        );

        // Clear to black
        ctx.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
        ctx.fill_rect(CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(w as f64, h as f64),
        ));

        // Draw glyph in white
        ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        ctx.set_allows_font_smoothing(true);
        ctx.set_should_smooth_fonts(true);
        ctx.set_allows_antialiasing(true);
        ctx.set_should_antialias(true);

        let baseline_y = m.descent;
        let positions = [CGPoint::new(0.0, baseline_y)];
        let glyphs_cg = [glyph];

        self.ct_font.draw_glyphs(
            &glyphs_cg,
            &positions,
            ctx.clone(),
        );

        // With font smoothing enabled, CoreText renders slightly different
        // values per RGB channel (subpixel AA). We collapse to grayscale
        // by taking the max of R/G/B — this preserves the smoothing's
        // stem-broadening effect without actual subpixel color fringing.
        let rgba_data = ctx.data();
        let alpha_data: Vec<u8> = (0..w * h)
            .map(|i| {
                let r = rgba_data[i * 4];
                let g = rgba_data[i * 4 + 1];
                let b = rgba_data[i * 4 + 2];
                ((r as u16 + g as u16 + b as u16) / 3) as u8
            })
            .collect();

        Some(RasterizedGlyph {
            data: alpha_data,
            width: w as u32,
            height: h as u32,
        })
    }

    /// Rasterize a wide (double-width) codepoint.
    pub fn rasterize_wide(&self, codepoint: u16) -> Option<RasterizedGlyph> {
        let characters = [codepoint];
        let mut glyphs: [CGGlyph; 1] = [0];
        let result = unsafe {
            self.ct_font.get_glyphs_for_characters(
                characters.as_ptr(),
                glyphs.as_mut_ptr(),
                1,
            )
        };
        if !result || glyphs[0] == 0 {
            return None;
        }

        let glyph = glyphs[0];
        let m = &self.metrics;

        let w = (m.cell_width * 2) as usize;
        let h = m.cell_height as usize;

        let color_space = CGColorSpace::create_device_rgb();
        let mut ctx = CGContext::create_bitmap_context(
            None,
            w,
            h,
            8,
            w * 4,
            &color_space,
            kCGImageAlphaNoneSkipLast,
        );

        ctx.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
        ctx.fill_rect(CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(w as f64, h as f64),
        ));

        ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        ctx.set_allows_font_smoothing(true);
        ctx.set_should_smooth_fonts(true);
        ctx.set_allows_antialiasing(true);
        ctx.set_should_antialias(true);

        let baseline_y = m.descent;
        let positions = [CGPoint::new(0.0, baseline_y)];
        let glyphs_cg = [glyph];

        self.ct_font.draw_glyphs(&glyphs_cg, &positions, ctx.clone());

        let rgba_data = ctx.data();
        let mut alpha_data = vec![0u8; w * h];
        for i in 0..w * h {
            let r = rgba_data[i * 4];
            let g = rgba_data[i * 4 + 1];
            let b = rgba_data[i * 4 + 2];
            alpha_data[i] = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        }

        Some(RasterizedGlyph {
            data: alpha_data,
            width: w as u32,
            height: h as u32,
        })
    }
}
