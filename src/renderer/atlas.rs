use std::collections::HashMap;

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
    pub x: u8,  // grid column
    pub y: u8,  // grid row
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
    // Slots 0..ascii_end are pinned (never evicted)
    ascii_end: u32,
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
            ascii_end: 0,
        }
    }

    /// Pre-rasterize ASCII range and pin those slots.
    pub fn preload_ascii(&mut self, rasterizer: &FontRasterizer) {
        for cp in 0x20u16..=0x7E {
            let key = GlyphKey { codepoint: cp, wide: false };
            if let Some(glyph) = rasterizer.rasterize(cp) {
                self.insert(key, &glyph);
            }
        }
        self.ascii_end = self.next_slot;
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
            return *pos;
        }

        // Rasterize
        let glyph = if wide {
            rasterizer.rasterize_wide(codepoint)
        } else {
            rasterizer.rasterize(codepoint)
        };

        match glyph {
            Some(g) => {
                self.insert(key, &g)
            }
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
            glyph.width.min(self.cell_width * if key.wide { 2 } else { 1 }) as u64,
            glyph.height.min(self.cell_height) as u64,
        );

        self.texture.replace_region(
            region,
            0,
            glyph.data.as_ptr() as *const _,
            glyph.width as u64,  // bytes per row (R8 = 1 byte per pixel)
        );

        let pos = AtlasPos {
            x: grid_x as u8,
            y: grid_y as u8,
        };
        self.map.insert(key, pos);
        self.usage[slot as usize] = self.frame;
        pos
    }

    fn evict_lru(&mut self) -> u32 {
        // Find the least recently used slot (skip pinned ASCII)
        let mut min_frame = u64::MAX;
        let mut min_slot = self.ascii_end;

        for slot in self.ascii_end..(self.cols * self.rows) {
            if self.usage[slot as usize] < min_frame {
                min_frame = self.usage[slot as usize];
                min_slot = slot;
            }
        }

        // Remove the old entry from the map
        let grid_x = min_slot % self.cols;
        let grid_y = min_slot / self.cols;
        self.map.retain(|_, pos| {
            !(pos.x == grid_x as u8 && pos.y == grid_y as u8)
        });

        min_slot
    }

    /// Call once per frame to advance the LRU counter.
    pub fn tick(&mut self) {
        self.frame += 1;
    }

    /// Clear the atlas (e.g., on DPI change). Keeps the texture, clears all mappings.
    pub fn clear(&mut self) {
        self.map.clear();
        self.next_slot = 0;
        self.ascii_end = 0;
        self.usage.fill(0);
    }
}
