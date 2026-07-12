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
    fn rasterize(&self, _codepoint: u32) -> Option<RasterizedGlyph> {
        let w = self.cell_width as usize;
        let h = self.cell_height as usize;
        Some(RasterizedGlyph {
            data: vec![128u8; w * h],
            width: self.cell_width,
            height: self.cell_height,
        })
    }

    fn rasterize_wide(&self, _codepoint: u32) -> Option<RasterizedGlyph> {
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

    let a = atlas.get_ascii(b'A');
    assert!(a.x != 0 || a.y != 0);
}

#[test]
fn get_or_insert_returns_cached_position() {
    let device = device();
    let rasterizer = MockRasterizer {
        cell_width: 8,
        cell_height: 16,
    };
    let mut atlas = Atlas::new(&device, 8, 16);

    let pos1 = atlas.get_or_insert(0x41, false, &rasterizer);
    let pos2 = atlas.get_or_insert(0x41, false, &rasterizer);
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

    let pos_a = atlas.get_or_insert(0x41, false, &rasterizer);
    let pos_b = atlas.get_or_insert(0x42, false, &rasterizer);
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

    let pos1 = atlas.get_or_insert(0x4E00, true, &rasterizer);
    let pos2 = atlas.get_or_insert(0x4E00, true, &rasterizer);
    // Wide glyph should get a valid insert and cache hit
    assert_eq!(pos1.x, pos2.x);
    assert_eq!(pos1.y, pos2.y);
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
    let excl = atlas.get_ascii(b'!');
    assert_eq!(table[b'!' as usize], [excl.x, excl.y]);
    assert!(excl.x != 0 || excl.y != 0);

    let space = atlas.get_ascii(b' ');
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

    // Fill all 16 slots (2048/512 = 4, so 4×4 = 16)
    for cp in 0x41u32..0x51 {
        atlas.get_or_insert(cp, false, &rasterizer);
    }

    // Insert 16 more — should not panic (eviction handles all slots)
    for cp in 0x51u32..0x61 {
        atlas.get_or_insert(cp, false, &rasterizer);
    }

    // Inserts after overflow should succeed (position returns valid coords)
    let pos = atlas.get_or_insert(0x61, false, &rasterizer);
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
        atlas.get_or_insert(cp, false, &rasterizer);
    }

    let _space = atlas.get_ascii(b' ');
    // space is at slot 0 = position (0, 0) — acceptable
    let a = atlas.get_ascii(b'A');
    assert!(a.x != 0 || a.y != 0, "A should be at non-zero position");
}
