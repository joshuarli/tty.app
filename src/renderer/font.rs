use crate::config;
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
    // Explicit fallback fonts checked in order when the primary font lacks a glyph.
    // Using an explicit list avoids CTFontCreateForString whose cascade behaviour
    // differs between CLI (cargo run) and GUI app bundle contexts for user-installed fonts.
    fallback_fonts: Vec<CTFont>,
    pub metrics: FontMetrics,
}

// System fonts ordered by Unicode coverage priority for terminal use.
const FALLBACK_FAMILIES: &[&str] = &[
    "Apple Symbols",
    "Menlo",
    "Apple Braille",
    "Apple Color Emoji",
    "Hiragino Sans",
    "Arial Unicode MS",
];

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
        // SAFETY: characters and glyphs are stack arrays with exactly 1 element.
        // The count argument (1) matches both array lengths.
        unsafe {
            ct_font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1);
        }

        let mut advances = [CGSize::new(0.0, 0.0)];
        // SAFETY: glyphs and advances are stack arrays with exactly 1 element.
        // The count argument (1) matches both array lengths.
        unsafe {
            ct_font.get_advances_for_glyphs(
                kCTFontOrientationDefault,
                glyphs.as_ptr(),
                advances.as_mut_ptr(),
                1,
            );
        }

        let cell_width = advances[0].width.round() as u32;
        let cell_height = (ascent + descent + leading).ceil() as u32;

        let metrics = FontMetrics {
            cell_width,
            cell_height,
            descent,
        };

        // Pre-load fallback fonts at the same point size.
        let fallback_fonts = FALLBACK_FAMILIES
            .iter()
            .filter_map(|name| ct_font::new_from_name(name, font_size).ok())
            .collect();

        Self {
            ct_font,
            fallback_fonts,
            metrics,
        }
    }

    /// Return the best font for rendering `codepoint`, or None to use the primary font.
    ///
    /// For ASCII, the primary font is always used (fast path).
    /// For non-ASCII, we skip the primary font and check the explicit fallback list.
    /// This is necessary because in a GUI app bundle context,
    /// CTFontGetGlyphsForCharacters on a user-installed font (Hack) can report
    /// non-zero glyph IDs for characters the font doesn't actually support —
    /// CoreText's cascade fires at the API level and returns the .notdef glyph
    /// (rendered as "_" in Hack) rather than returning false.
    /// System fonts loaded explicitly by name don't exhibit this behaviour.
    fn font_for_codepoint(&self, codepoint: u32) -> Option<&CTFont> {
        // ASCII fast path: primary font always has these.
        if codepoint < 0x80 {
            return None; // use self.ct_font
        }

        // Non-ASCII: query the explicit fallback list only.
        for font in &self.fallback_fonts {
            if Self::font_has_glyph(font, codepoint) {
                return Some(font);
            }
        }

        // No fallback has it — let the primary font render whatever it has.
        None
    }

    /// Check if a font has a glyph for a codepoint, handling surrogate pairs for non-BMP.
    fn font_has_glyph(font: &CTFont, codepoint: u32) -> bool {
        if codepoint <= 0xFFFF {
            let characters = [codepoint as u16];
            let mut glyphs: [CGGlyph; 1] = [0];
            let found = unsafe {
                font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1)
            };
            found && glyphs[0] != 0
        } else {
            let hi = ((codepoint - 0x10000) >> 10) as u16 + 0xD800;
            let lo = ((codepoint - 0x10000) & 0x3FF) as u16 + 0xDC00;
            let characters = [hi, lo];
            let mut glyphs: [CGGlyph; 2] = [0, 0];
            let found = unsafe {
                font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 2)
            };
            found && glyphs[0] != 0
        }
    }

    /// Rasterize a single codepoint into an R8 alpha bitmap.
    /// Returns None if the glyph is missing from all fonts.
    pub fn rasterize(&self, codepoint: u32) -> Option<RasterizedGlyph> {
        let font = self.font_for_codepoint(codepoint).unwrap_or(&self.ct_font);
        self.rasterize_with_font(font, codepoint, self.metrics.cell_width)
    }

    /// Rasterize a wide (double-width) codepoint.
    pub fn rasterize_wide(&self, codepoint: u32) -> Option<RasterizedGlyph> {
        let font = self.font_for_codepoint(codepoint).unwrap_or(&self.ct_font);
        self.rasterize_with_font(font, codepoint, self.metrics.cell_width * 2)
    }

    /// Resolve a codepoint to a glyph ID, handling UTF-16 surrogate pairs for non-BMP.
    fn glyph_for_codepoint(font: &CTFont, codepoint: u32) -> Option<CGGlyph> {
        if codepoint <= 0xFFFF {
            let characters = [codepoint as u16];
            let mut glyphs: [CGGlyph; 1] = [0];
            let result = unsafe {
                font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1)
            };
            if result && glyphs[0] != 0 {
                Some(glyphs[0])
            } else {
                None
            }
        } else {
            let hi = ((codepoint - 0x10000) >> 10) as u16 + 0xD800;
            let lo = ((codepoint - 0x10000) & 0x3FF) as u16 + 0xDC00;
            let characters = [hi, lo];
            let mut glyphs: [CGGlyph; 2] = [0, 0];
            let result = unsafe {
                font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 2)
            };
            if result && glyphs[0] != 0 {
                Some(glyphs[0])
            } else {
                None
            }
        }
    }

    fn rasterize_with_font(
        &self,
        font: &CTFont,
        codepoint: u32,
        render_width: u32,
    ) -> Option<RasterizedGlyph> {
        let glyph = Self::glyph_for_codepoint(font, codepoint)?;
        let m = &self.metrics;

        let w = render_width as usize;
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

        font.draw_glyphs(&glyphs_cg, &positions, ctx.clone());

        // With font smoothing enabled, CoreText renders slightly different
        // values per RGB channel (subpixel AA). We blend between the min
        // channel (thinnest) and average (medium) to control stem weight.
        // FONT_SMOOTH_WEIGHT 0.0 = thinnest, 1.0 = full average.
        let rgba_data = ctx.data();
        let w_f = config::FONT_SMOOTH_WEIGHT;
        let alpha_data: Vec<u8> = (0..w * h)
            .map(|i| {
                let r = rgba_data[i * 4] as f32;
                let g = rgba_data[i * 4 + 1] as f32;
                let b = rgba_data[i * 4 + 2] as f32;
                let thin = r.min(g).min(b);
                let avg = (r + g + b) / 3.0;
                (thin + w_f * (avg - thin)) as u8
            })
            .collect();

        Some(RasterizedGlyph {
            data: alpha_data,
            width: render_width,
            height: m.cell_height,
        })
    }
}
