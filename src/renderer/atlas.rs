use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLRegion, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};
use rustc_hash::{FxBuildHasher, FxHashMap};

use crate::renderer::Rasterize;
use crate::renderer::font::RasterizedGlyph;

const ATLAS_SIZE: u32 = 2048;

/// Key for atlas glyph lookup.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct GlyphKey {
    pub codepoint: u32,
    pub wide: bool,
    pub bold: bool,
}

/// Position of a glyph in the atlas grid.
#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasPos {
    pub x: u8, // grid column
    pub y: u8, // grid row
}

/// Glyph texture atlas with grid-based packing.
pub struct Atlas {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    cell_width: u32,
    cell_height: u32,
    cols: u32,
    rows: u32,
    map: FxHashMap<GlyphKey, AtlasPos>,
    // Next allocation slot. Every slot reserves two cell columns so wide
    // glyphs cannot overlap the following slot or run past the texture edge.
    next_slot: u32,
    // Slots 0..ascii_end are pinned (never evicted)
    ascii_end: u32,
    // Unpinned slots form an intrusive LRU list. Keeping links per slot avoids
    // retaining stale heap entries or scanning the atlas when evicting.
    lru_head: u32,
    lru_tail: u32,
    lru_prev: Vec<u32>,
    lru_next: Vec<u32>,
    slot_keys: Vec<Option<GlyphKey>>,
    // Direct lookup for ASCII (0x00..0x7F) — bypasses HashMap and LRU
    ascii_table: [AtlasPos; 128],
    bold_ascii_table: [AtlasPos; 128],
}

impl Atlas {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, cell_width: u32, cell_height: u32) -> Self {
        assert!(cell_width > 0 && cell_width <= ATLAS_SIZE / 2);
        assert!(cell_height > 0 && cell_height <= ATLAS_SIZE);

        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::R8Unorm,
                ATLAS_SIZE as usize,
                ATLAS_SIZE as usize,
                false,
            )
        };
        desc.setStorageMode(MTLStorageMode::Shared);
        desc.setUsage(MTLTextureUsage::ShaderRead);
        let texture = device
            .newTextureWithDescriptor(&desc)
            .expect("failed to create atlas texture");

        // Every slot is two cells wide because wide glyphs are uploaded as a
        // double-cell bitmap. Positions remain expressed in cell columns for
        // the shader, so allocated x coordinates are even.
        let cols = ATLAS_SIZE / (cell_width * 2);
        let rows = ATLAS_SIZE / cell_height;

        Atlas {
            texture,
            cell_width,
            cell_height,
            cols,
            rows,
            map: FxHashMap::with_capacity_and_hasher(512, FxBuildHasher),
            next_slot: 0,
            ascii_end: 0,
            lru_head: u32::MAX,
            lru_tail: u32::MAX,
            lru_prev: vec![u32::MAX; (cols * rows) as usize],
            lru_next: vec![u32::MAX; (cols * rows) as usize],
            slot_keys: vec![None; (cols * rows) as usize],
            ascii_table: [AtlasPos::default(); 128],
            bold_ascii_table: [AtlasPos::default(); 128],
        }
    }

    /// Pre-rasterize ASCII range and pin those slots.
    pub fn preload_ascii<R: Rasterize>(&mut self, rasterizer: &R) {
        for bold in [false, true] {
            for cp in 0x20u32..=0x7E {
                let key = GlyphKey {
                    codepoint: cp,
                    wide: false,
                    bold,
                };
                if let Some(glyph) = rasterizer.rasterize(cp, bold) {
                    let pos = self.insert(key, &glyph);
                    if bold {
                        self.bold_ascii_table[cp as usize] = pos;
                    } else {
                        self.ascii_table[cp as usize] = pos;
                    }
                }
            }
        }
        self.ascii_end = self.next_slot;
        self.lru_head = u32::MAX;
        self.lru_tail = u32::MAX;
        self.lru_prev.fill(u32::MAX);
        self.lru_next.fill(u32::MAX);
    }

    /// Direct ASCII lookup — no HashMap, no LRU. Caller must ensure cp < 128.
    #[inline]
    pub fn get_ascii(&self, cp: u8, bold: bool) -> AtlasPos {
        if bold {
            self.bold_ascii_table[cp as usize]
        } else {
            self.ascii_table[cp as usize]
        }
    }

    /// Get the ASCII atlas table as [x, y] pairs for Grid initialization.
    pub fn ascii_table_raw(&self) -> [[u8; 2]; 128] {
        let mut out = [[0u8; 2]; 128];
        for (i, pos) in self.ascii_table.iter().enumerate() {
            out[i] = [pos.x, pos.y];
        }
        out
    }

    pub fn bold_ascii_table_raw(&self) -> [[u8; 2]; 128] {
        let mut out = [[0u8; 2]; 128];
        for (i, pos) in self.bold_ascii_table.iter().enumerate() {
            out[i] = [pos.x, pos.y];
        }
        out
    }

    /// Look up or rasterize a glyph, returning its atlas position.
    pub fn get_or_insert<R: Rasterize>(
        &mut self,
        codepoint: u32,
        wide: bool,
        bold: bool,
        rasterizer: &R,
    ) -> AtlasPos {
        let key = GlyphKey {
            codepoint,
            wide,
            bold,
        };

        if let Some(pos) = self.map.get(&key) {
            let pos = *pos;
            let slot = pos.y as u32 * self.cols + pos.x as u32 / 2;
            self.touch_lru(slot);
            return pos;
        }

        // Rasterize
        let glyph = if wide {
            rasterizer.rasterize_wide(codepoint, bold)
        } else {
            rasterizer.rasterize(codepoint, bold)
        };

        match glyph {
            Some(g) => self.insert(key, &g),
            None => AtlasPos::default(), // missing glyph → slot (0,0) which is space
        }
    }

    fn insert(&mut self, key: GlyphKey, glyph: &RasterizedGlyph) -> AtlasPos {
        let slot = if self.next_slot < self.cols * self.rows {
            let s = self.next_slot;
            self.next_slot += 1;
            s
        } else {
            self.evict_lru()
        };

        let grid_x = (slot % self.cols) * 2;
        let grid_y = slot / self.cols;

        // Upload glyph data to atlas texture
        let region = MTLRegion {
            origin: objc2_metal::MTLOrigin {
                x: (grid_x * self.cell_width) as usize,
                y: (grid_y * self.cell_height) as usize,
                z: 0,
            },
            size: objc2_metal::MTLSize {
                width: glyph
                    .width
                    .min(self.cell_width * if key.wide { 2 } else { 1 })
                    as usize,
                height: glyph.height.min(self.cell_height) as usize,
                depth: 1,
            },
        };

        unsafe {
            self.texture
                .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region,
                    0,
                    NonNull::new(glyph.data.as_ptr() as *mut c_void).expect("empty glyph data"),
                    glyph.width as usize, // bytes per row (R8 = 1 byte per pixel)
                );
        }

        let pos = AtlasPos {
            x: grid_x as u8,
            y: grid_y as u8,
        };
        self.map.insert(key, pos);
        self.slot_keys[slot as usize] = Some(key);
        self.push_lru_front(slot);
        pos
    }

    fn evict_lru(&mut self) -> u32 {
        let slot = self.lru_tail;
        assert!(slot != u32::MAX, "atlas has no evictable slots");
        self.unlink_lru(slot);
        let key = self.slot_keys[slot as usize]
            .take()
            .expect("evictable atlas slot has no key");
        self.map.remove(&key);
        slot
    }

    fn touch_lru(&mut self, slot: u32) {
        if slot < self.ascii_end || self.lru_head == slot {
            return;
        }
        self.unlink_lru(slot);
        self.push_lru_front(slot);
    }

    fn push_lru_front(&mut self, slot: u32) {
        self.lru_prev[slot as usize] = u32::MAX;
        self.lru_next[slot as usize] = self.lru_head;
        if self.lru_head != u32::MAX {
            self.lru_prev[self.lru_head as usize] = slot;
        } else {
            self.lru_tail = slot;
        }
        self.lru_head = slot;
    }

    fn unlink_lru(&mut self, slot: u32) {
        let prev = self.lru_prev[slot as usize];
        let next = self.lru_next[slot as usize];
        if prev == u32::MAX {
            self.lru_head = next;
        } else {
            self.lru_next[prev as usize] = next;
        }
        if next == u32::MAX {
            self.lru_tail = prev;
        } else {
            self.lru_prev[next as usize] = prev;
        }
        self.lru_prev[slot as usize] = u32::MAX;
        self.lru_next[slot as usize] = u32::MAX;
    }
}
