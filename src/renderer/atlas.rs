use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use metal::*;

use crate::renderer::font::{FontRasterizer, RasterizedGlyph};

const ATLAS_SIZE: u32 = 2048;

/// Key for atlas glyph lookup.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct GlyphKey {
    pub codepoint: u16,
    pub wide: bool,
}

/// Position of a glyph in the atlas grid.
#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasPos {
    pub x: u8, // grid column
    pub y: u8, // grid row
}

/// Glyph texture atlas with grid-based packing.
pub struct Atlas {
    pub texture: Texture,
    cell_width: u32,
    cell_height: u32,
    cols: u32,
    rows: u32,
    map: HashMap<GlyphKey, AtlasPos>,
    // Next allocation slot
    next_slot: u32,
    // LRU: slot → last used frame
    usage: Vec<u64>,
    frame: u64,
    // Min-heap for O(log n) LRU eviction (lazy deletion)
    lru_heap: BinaryHeap<Reverse<(u64, u32)>>,
    // Slots 0..ascii_end are pinned (never evicted)
    ascii_end: u32,
    // Direct lookup for ASCII (0x00..0x7F) — bypasses HashMap and LRU
    ascii_table: [AtlasPos; 128],
}

impl Atlas {
    pub fn new(device: &Device, cell_width: u32, cell_height: u32) -> Self {
        let desc = TextureDescriptor::new();
        desc.set_pixel_format(MTLPixelFormat::R8Unorm);
        desc.set_width(ATLAS_SIZE as u64);
        desc.set_height(ATLAS_SIZE as u64);
        desc.set_storage_mode(MTLStorageMode::Shared);
        desc.set_usage(MTLTextureUsage::ShaderRead);
        let texture = device.new_texture(&desc);

        // Use double cell width for the atlas grid to accommodate wide glyphs
        let cols = ATLAS_SIZE / cell_width;
        let rows = ATLAS_SIZE / cell_height;

        Atlas {
            texture,
            cell_width,
            cell_height,
            cols,
            rows,
            map: HashMap::with_capacity(512),
            next_slot: 0,
            usage: vec![0; (cols * rows) as usize],
            frame: 0,
            lru_heap: BinaryHeap::with_capacity(512),
            ascii_end: 0,
            ascii_table: [AtlasPos::default(); 128],
        }
    }

    /// Pre-rasterize ASCII range and pin those slots.
    pub fn preload_ascii(&mut self, rasterizer: &FontRasterizer) {
        for cp in 0x20u16..=0x7E {
            let key = GlyphKey {
                codepoint: cp,
                wide: false,
            };
            if let Some(glyph) = rasterizer.rasterize(cp) {
                let pos = self.insert(key, &glyph);
                self.ascii_table[cp as usize] = pos;
            }
        }
        self.ascii_end = self.next_slot;
    }

    /// Direct ASCII lookup — no HashMap, no LRU. Caller must ensure cp < 128.
    #[inline]
    pub fn get_ascii(&self, cp: u8) -> AtlasPos {
        self.ascii_table[cp as usize]
    }

    /// Get the ASCII atlas table as [x, y] pairs for Grid initialization.
    pub fn ascii_table_raw(&self) -> [[u8; 2]; 128] {
        let mut out = [[0u8; 2]; 128];
        for (i, pos) in self.ascii_table.iter().enumerate() {
            out[i] = [pos.x, pos.y];
        }
        out
    }

    /// Look up or rasterize a glyph, returning its atlas position.
    pub fn get_or_insert(
        &mut self,
        codepoint: u16,
        wide: bool,
        rasterizer: &FontRasterizer,
    ) -> AtlasPos {
        let key = GlyphKey { codepoint, wide };

        if let Some(pos) = self.map.get(&key) {
            let slot = pos.y as u32 * self.cols + pos.x as u32;
            self.usage[slot as usize] = self.frame;
            self.lru_heap.push(Reverse((self.frame, slot)));
            return *pos;
        }

        // Rasterize
        let glyph = if wide {
            rasterizer.rasterize_wide(codepoint)
        } else {
            rasterizer.rasterize(codepoint)
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

        let grid_x = slot % self.cols;
        let grid_y = slot / self.cols;

        // Upload glyph data to atlas texture
        let region = MTLRegion::new_2d(
            (grid_x * self.cell_width) as u64,
            (grid_y * self.cell_height) as u64,
            glyph
                .width
                .min(self.cell_width * if key.wide { 2 } else { 1 }) as u64,
            glyph.height.min(self.cell_height) as u64,
        );

        self.texture.replace_region(
            region,
            0,
            glyph.data.as_ptr() as *const _,
            glyph.width as u64, // bytes per row (R8 = 1 byte per pixel)
        );

        let pos = AtlasPos {
            x: grid_x as u8,
            y: grid_y as u8,
        };
        self.map.insert(key, pos);
        self.usage[slot as usize] = self.frame;
        self.lru_heap.push(Reverse((self.frame, slot)));
        pos
    }

    fn evict_lru(&mut self) -> u32 {
        // Pop stale entries until we find a valid, non-pinned LRU slot
        while let Some(Reverse((frame, slot))) = self.lru_heap.pop() {
            if slot < self.ascii_end {
                continue; // pinned
            }
            if self.usage[slot as usize] != frame {
                continue; // stale — slot was accessed more recently
            }
            // Found the real LRU slot
            let grid_x = slot % self.cols;
            let grid_y = slot / self.cols;
            self.map
                .retain(|_, pos| !(pos.x == grid_x as u8 && pos.y == grid_y as u8));
            return slot;
        }
        // Fallback: shouldn't happen unless every slot is pinned
        self.ascii_end
    }
}
