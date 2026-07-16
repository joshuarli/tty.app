pub mod atlas;
pub mod core;
pub mod font;
pub mod metal;

/// Font rasterization interface (enables mock rasterizers in tests).
pub trait Rasterize {
    fn rasterize(&self, codepoint: u32, bold: bool) -> Option<font::RasterizedGlyph>;
    fn rasterize_wide(&self, codepoint: u32, bold: bool) -> Option<font::RasterizedGlyph>;
}
