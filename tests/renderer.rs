use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLTexture};

use tty::renderer::Rasterize;
use tty::renderer::atlas::Atlas;
use tty::renderer::font::RasterizedGlyph;

/// A mock font rasterizer that returns fixed-size dummy glyph data.
struct MockRasterizer {
    cell_width: u32,
    cell_height: u32,
}

impl Rasterize for MockRasterizer {
    fn rasterize(&self, _codepoint: u32, _bold: bool) -> Option<RasterizedGlyph> {
        let w = self.cell_width as usize;
        let h = self.cell_height as usize;
        Some(RasterizedGlyph {
            data: vec![128u8; w * h],
            width: self.cell_width,
            height: self.cell_height,
        })
    }

    fn rasterize_wide(&self, _codepoint: u32, _bold: bool) -> Option<RasterizedGlyph> {
        let w = (self.cell_width * 2) as usize;
        let h = self.cell_height as usize;
        Some(RasterizedGlyph {
            data: vec![128u8; w * h],
            width: self.cell_width * 2,
            height: self.cell_height,
        })
    }
}

fn device() -> Retained<ProtocolObject<dyn MTLDevice>> {
    MTLCreateSystemDefaultDevice().expect("Metal device required")
}

#[test]
fn preload_ascii_populates_table() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);
    atlas.preload_ascii(&rasterizer);

    let a = atlas.get_ascii(b'A', false);
    assert!(a.x != 0 || a.y != 0);
    let bold_a = atlas.get_ascii(b'A', true);
    assert_ne!((a.x, a.y), (bold_a.x, bold_a.y));
}

#[test]
fn get_or_insert_returns_cached_position() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);

    let pos1 = atlas.get_or_insert(0x41, false, false, &rasterizer);
    let pos2 = atlas.get_or_insert(0x41, false, false, &rasterizer);
    assert_eq!(pos1.x, pos2.x);
    assert_eq!(pos1.y, pos2.y);
}

#[test]
fn different_codepoints_get_different_slots() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);

    let pos_a = atlas.get_or_insert(0x41, false, false, &rasterizer);
    let pos_b = atlas.get_or_insert(0x42, false, false, &rasterizer);
    assert!(pos_a.x != pos_b.x || pos_a.y != pos_b.y);
}

#[test]
fn atlas_texture_is_accessible() {
    let device = device();
    let atlas = Atlas::new(&device, 8, 16);
    let tex = &atlas.texture;
    assert_eq!(tex.width(), 2048);
    assert_eq!(tex.height(), 2048);
}

#[test]
fn wide_glyph_gets_slot() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);

    let pos1 = atlas.get_or_insert(0x4E00, true, false, &rasterizer);
    let pos2 = atlas.get_or_insert(0x4E00, true, false, &rasterizer);
    // Wide glyph should get a valid insert and cache hit
    assert_eq!(pos1.x, pos2.x);
    assert_eq!(pos1.y, pos2.y);
}

#[test]
fn wide_glyph_at_slot_boundary_fits_atlas() {
    let device = device();
    let cell_w = 512;
    let cell_h = 512;
    let rasterizer = MockRasterizer {
        cell_width: cell_w,
        cell_height: cell_h,
    };
    let mut atlas = Atlas::new(&device, cell_w, cell_h);

    // With one-cell slots, this wide insert lands in the final atlas column
    // and triggers AGX's Region width OOB assertion.
    for cp in 0x41..0x44 {
        atlas.get_or_insert(cp, false, false, &rasterizer);
    }
    let pos = atlas.get_or_insert(0x4E00, true, false, &rasterizer);

    assert!(pos.x as u32 + 2 <= 2048 / cell_w);
}

#[test]
fn narrow_glyphs_share_the_atlas_row() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 512,
        cell_height: 512,
    };
    let mut atlas = Atlas::new(&device, 512, 512);

    let first = atlas.get_or_insert(0x41, false, false, &rasterizer);
    let second = atlas.get_or_insert(0x42, false, false, &rasterizer);
    assert_eq!(first.y, second.y);
    assert_eq!(second.x, first.x + 1);
}

#[test]
fn ascii_table_raw_matches_preload() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);
    atlas.preload_ascii(&rasterizer);

    let table = atlas.ascii_table_raw();
    // 0x20 (space) is at slot 0 = position (0, 0); check 0x21 instead
    let excl = atlas.get_ascii(b'!', false);
    assert_eq!(table[b'!' as usize], [excl.x, excl.y]);
    assert!(excl.x != 0 || excl.y != 0);

    let space = atlas.get_ascii(b' ', false);
    assert_eq!(table[b' ' as usize], [space.x, space.y]);
}

#[test]
fn evict_lru_when_full() {
    let device = device();
    let cell_w = 512;
    let cell_h = 512;
    let rasterizer = MockRasterizer {
        cell_width: cell_w,
        cell_height: cell_h,
    };
    let mut atlas = Atlas::new(&device, cell_w, cell_h);

    // Fill all 8 double-cell slots (2048/512 = 4 cell columns, so 2×4 = 8)
    for cp in 0x41u32..0x51 {
        atlas.get_or_insert(cp, false, false, &rasterizer);
    }

    // Insert 16 more — should not panic (eviction handles all slots)
    for cp in 0x51u32..0x61 {
        atlas.get_or_insert(cp, false, false, &rasterizer);
    }

    // Inserts after overflow should succeed (position returns valid coords)
    let pos = atlas.get_or_insert(0x61, false, false, &rasterizer);
    assert!(pos.x < 4, "x out of range: {}", pos.x);
    assert!(pos.y < 4, "y out of range: {}", pos.y);
}

#[test]
fn eviction_preserves_ascii_when_large_enough() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    // 256 × 128 = 32768 slots — plenty for 95 ASCII + many non-ASCII
    let mut atlas = Atlas::new(&device, 8, 16);
    atlas.preload_ascii(&rasterizer);

    // Insert enough non-ASCII to trigger eviction if nothing were pinned
    for cp in 0x80u32..0x200 {
        atlas.get_or_insert(cp, false, false, &rasterizer);
    }

    let _space = atlas.get_ascii(b' ', false);
    // space is at slot 0 = position (0, 0) — acceptable
    let a = atlas.get_ascii(b'A', false);
    assert!(a.x != 0 || a.y != 0, "A should be at non-zero position");
}
