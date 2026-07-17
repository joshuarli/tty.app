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

pub trait GlyphAtlas {
    fn get_or_insert<R: Rasterize + ?Sized>(
        &mut self,
        codepoint: u32,
        wide: bool,
        bold: bool,
        rasterizer: &R,
    ) -> AtlasPos;

    fn get_ascii(&self, cp: u8, bold: bool) -> AtlasPos;
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
    // LRU: slot → last access sequence. Eviction is cold, so a compact
    // timestamp array keeps the hot lookup path small.
    usage: Vec<u64>,
    frame: u64,
    evictions: u64,
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
            usage: vec![0; (cols * rows) as usize],
            frame: 0,
            evictions: 0,
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
    pub fn get_or_insert<R: Rasterize + ?Sized>(
        &mut self,
        codepoint: u32,
        wide: bool,
        bold: bool,
        rasterizer: &R,
    ) -> AtlasPos {
        self.frame = self.frame.wrapping_add(1);
        let key = GlyphKey {
            codepoint,
            wide,
            bold,
        };

        if let Some(pos) = self.map.get(&key) {
            let pos = *pos;
            let slot = pos.y as u32 * self.cols + pos.x as u32 / 2;
            self.usage[slot as usize] = self.frame;
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

    pub fn evictions(&self) -> u64 {
        self.evictions
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
        self.usage[slot as usize] = self.frame;
        pos
    }

    fn evict_lru(&mut self) -> u32 {
        self.evictions = self.evictions.wrapping_add(1);
        let mut slot = self.ascii_end;
        let mut oldest = u64::MAX;
        for candidate in self.ascii_end..self.usage.len() as u32 {
            if self.usage[candidate as usize] < oldest {
                oldest = self.usage[candidate as usize];
                slot = candidate;
            }
        }

        if slot < self.usage.len() as u32 {
            let grid_x = (slot % self.cols) * 2;
            let grid_y = slot / self.cols;
            self.map
                .retain(|_, pos| !(pos.x == grid_x as u8 && pos.y == grid_y as u8));
        }
        slot
    }
}

impl GlyphAtlas for Atlas {
    fn get_or_insert<R: Rasterize + ?Sized>(
        &mut self,
        codepoint: u32,
        wide: bool,
        bold: bool,
        rasterizer: &R,
    ) -> AtlasPos {
        Self::get_or_insert(self, codepoint, wide, bold, rasterizer)
    }

    fn get_ascii(&self, cp: u8, bold: bool) -> AtlasPos {
        Self::get_ascii(self, cp, bold)
    }
}
