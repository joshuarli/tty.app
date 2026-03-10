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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub codepoint: u16,
    pub flags: CellFlags,
    pub fg_index: u16,
    pub bg_index: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            codepoint: b' ' as u16,
            flags: CellFlags::empty(),
            fg_index: 7, // default fg = palette 7
            bg_index: 0, // default bg = palette 0
        }
    }
}

// Cell and CellData (GPU) must both be 8 bytes for efficient render upload.
const _: () = assert!(std::mem::size_of::<Cell>() == 8, "Cell must be 8 bytes");

impl Cell {
    /// Create a blank cell with the given SGR background attributes.
    #[inline]
    pub fn blank(attr: &Cell) -> Self {
        Self {
            codepoint: b' ' as u16,
            flags: CellFlags::empty(),
            fg_index: attr.fg_index,
            bg_index: attr.bg_index,
        }
    }
}
