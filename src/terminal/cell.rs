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

/// Terminal cell — also the GPU CellData format.
/// Atlas coords are resolved at write time so GPU upload is a raw memcpy.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub codepoint: u16,
    pub flags: CellFlags,
    pub fg_index: u8,
    pub bg_index: u8,
    pub atlas_x: u8,
    pub atlas_y: u8,
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
        }
    }
}

// Cell is the GPU format — must be exactly 8 bytes.
const _: () = assert!(std::mem::size_of::<Cell>() == 8, "Cell must be 8 bytes");

impl Cell {
    /// Create a blank (space) cell with given SGR background and space atlas position.
    #[inline]
    pub fn blank(attr: &Cell, space_atlas: [u8; 2]) -> Self {
        Self {
            codepoint: b' ' as u16,
            flags: CellFlags::empty(),
            fg_index: attr.fg_index,
            bg_index: attr.bg_index,
            atlas_x: space_atlas[0],
            atlas_y: space_atlas[1],
        }
    }
}
