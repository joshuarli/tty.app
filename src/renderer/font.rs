use crate::config;
use core_foundation::base::{CFIndex, TCFType};
use core_foundation::string::CFString;
use core_graphics::base::kCGImageAlphaNoneSkipLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::font::CGGlyph;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{self as ct_font, CTFont, CTFontRef};
use core_text::font_descriptor::kCTFontOrientationDefault;

// CTFontCreateForString: given a base font and a string, returns the best
// font from the system cascade for rendering that string. This is CoreText's
// standard font substitution mechanism.
#[repr(C)]
struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

unsafe extern "C" {
    fn CTFontCreateForString(
        current_font: CTFontRef,
        string: *const std::ffi::c_void, // CFStringRef
        range: CFRange,
    ) -> CTFontRef;
}

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
            ct_font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1);
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

        let cell_width = advances[0].width.round() as u32;
        let cell_height = (ascent + descent + leading).ceil() as u32;

        let metrics = FontMetrics {
            cell_width,
            cell_height,
            descent,
        };

        Self { ct_font, metrics }
    }

    /// Return the best CoreText font for rendering `codepoint`, falling back
    /// to system font substitution when the primary font lacks the glyph.
    fn font_for_codepoint(&self, codepoint: u16) -> Option<CTFont> {
        // Fast path: primary font has the glyph
        let characters = [codepoint];
        let mut glyphs: [CGGlyph; 1] = [0];
        let found = unsafe {
            self.ct_font
                .get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1)
        };
        if found && glyphs[0] != 0 {
            return None; // signal: use self.ct_font
        }

        // Fallback: ask CoreText which system font covers this codepoint
        let ch = char::from_u32(codepoint as u32)?;
        let cf_str = CFString::new(&ch.to_string());
        let fallback_ref = unsafe {
            CTFontCreateForString(
                self.ct_font.as_concrete_TypeRef(),
                cf_str.as_concrete_TypeRef() as *const _,
                CFRange {
                    location: 0,
                    length: 1,
                },
            )
        };
        if fallback_ref.is_null() {
            return None;
        }
        Some(unsafe { CTFont::wrap_under_get_rule(fallback_ref) })
    }

    /// Rasterize a single codepoint into an R8 alpha bitmap.
    /// Returns None if the glyph is missing from all fonts.
    pub fn rasterize(&self, codepoint: u16) -> Option<RasterizedGlyph> {
        let fallback = self.font_for_codepoint(codepoint);
        let font = fallback.as_ref().unwrap_or(&self.ct_font);
        self.rasterize_with_font(font, codepoint, self.metrics.cell_width, false)
    }

    /// Rasterize a wide (double-width) codepoint.
    pub fn rasterize_wide(&self, codepoint: u16) -> Option<RasterizedGlyph> {
        let fallback = self.font_for_codepoint(codepoint);
        let font = fallback.as_ref().unwrap_or(&self.ct_font);
        self.rasterize_with_font(font, codepoint, self.metrics.cell_width * 2, true)
    }

    fn rasterize_with_font(
        &self,
        font: &CTFont,
        codepoint: u16,
        render_width: u32,
        _wide: bool,
    ) -> Option<RasterizedGlyph> {
        let characters = [codepoint];
        let mut glyphs: [CGGlyph; 1] = [0];
        let result =
            unsafe { font.get_glyphs_for_characters(characters.as_ptr(), glyphs.as_mut_ptr(), 1) };
        if !result || glyphs[0] == 0 {
            return None;
        }

        let glyph = glyphs[0];
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
