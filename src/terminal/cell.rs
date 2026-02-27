use crate::config;
use crate::renderer::metal::CellData;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct CellFlags: u16 {
        const WIDE       = 0x0001;
        const WIDE_CONT  = 0x0002;
        const UNDERLINE  = 0x0004;
        const STRIKE     = 0x0008;
        const INVERSE    = 0x0010;
        const CURSOR     = 0x0020;
        const SELECTED   = 0x0040;
        const BOLD       = 0x0080;  // triggers bright color mapping
        const ITALIC     = 0x0100;
        const DIM        = 0x0200;
        const HIDDEN     = 0x0400;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub codepoint: u16,
    pub flags: CellFlags,
    pub fg_index: u8,
    pub bg_index: u8,
    pub atlas_x: u8,
    pub atlas_y: u8,
    pub fg_rgb: u32,
    pub bg_rgb: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            codepoint: b' ' as u16,
            flags: CellFlags::empty(),
            fg_index: 7,  // default fg = palette 7
            bg_index: 0,  // default bg = palette 0
            atlas_x: 0,
            atlas_y: 0,
            fg_rgb: config::DEFAULT_FG,
            bg_rgb: config::DEFAULT_BG,
        }
    }
}

impl Cell {
    /// Convert to GPU-side CellData for upload.
    pub fn to_cell_data(&self) -> CellData {
        let mut flags = self.flags;
        let mut fg_index = self.fg_index;

        // Bold = bright colors: map palette 0-7 → 8-15
        if flags.contains(CellFlags::BOLD) && fg_index < 8 {
            fg_index += 8;
        }

        // Hidden: make fg = bg
        if flags.contains(CellFlags::HIDDEN) {
            return CellData {
                codepoint: self.codepoint,
                flags: flags.bits(),
                fg_index: self.bg_index,
                bg_index: self.bg_index,
                atlas_x: self.atlas_x,
                atlas_y: self.atlas_y,
                fg_rgb: self.bg_rgb,
                bg_rgb: self.bg_rgb,
            };
        }

        CellData {
            codepoint: self.codepoint,
            flags: flags.bits(),
            fg_index,
            bg_index: self.bg_index,
            atlas_x: self.atlas_x,
            atlas_y: self.atlas_y,
            fg_rgb: self.fg_rgb,
            bg_rgb: self.bg_rgb,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
