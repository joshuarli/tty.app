use crate::config;

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
#[repr(C)]
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
            fg_index: 7, // default fg = palette 7
            bg_index: 0, // default bg = palette 0
            atlas_x: 0,
            atlas_y: 0,
            fg_rgb: config::DEFAULT_FG,
            bg_rgb: config::DEFAULT_BG,
        }
    }
}

impl Cell {
    /// Create a blank cell with the given SGR background attributes.
    #[inline]
    pub fn blank(attr: &Cell) -> Self {
        Self {
            codepoint: b' ' as u16,
            flags: CellFlags::empty(),
            fg_index: attr.fg_index,
            bg_index: attr.bg_index,
            atlas_x: 0,
            atlas_y: 0,
            fg_rgb: attr.fg_rgb,
            bg_rgb: attr.bg_rgb,
        }
    }
}
