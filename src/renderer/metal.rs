use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bitvec::prelude::*;
use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::CGSize;
use objc2_metal::*;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::config;
use crate::renderer::core::MetalCore;
use crate::renderer_trait::Renderer;
use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::grid::Grid;
use crate::terminal::scrollback::Scrollback;

/// Uniforms passed to the compute shader. Must match Metal Uniforms struct.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub cols: u32,
    pub rows: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub atlas_cell_width: u32,
    pub atlas_cell_height: u32,
    pub padding: u32,
    pub padding_top: u32,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub cursor_visible: u32,
    pub frame_bg: u32,
    pub damage_origin_x: u32,
    pub damage_origin_y: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ScrollCopyUniforms {
    source_y: u32,
    destination_y: u32,
    width: u32,
    height: u32,
}

const CELL_SIZE: usize = mem::size_of::<Cell>();
const _: () = assert!(CELL_SIZE == 8, "Cell must be 8 bytes for GPU layout");
const TILED_SKIP_FLAG: u16 = 0x8000;

/// Number of cell buffers for pipelining (CPU uploads to one while GPU reads the other).
const NUM_BUFFERS: usize = 2;

pub struct MetalRenderer {
    core: MetalCore,
    layer: Retained<CAMetalLayer>,

    // Double-buffered cell data — CPU writes to one while GPU reads the other
    cell_buffers: [Retained<ProtocolObject<dyn MTLBuffer>>; NUM_BUFFERS],
    current_buffer: usize,
    buffer_ready: [Arc<AtomicBool>; NUM_BUFFERS],
    // Per-buffer dirty row tracking: each dirty row must be copied to BOTH buffers
    pending: [BitVec; NUM_BUFFERS],

    // Retained active-cell resources. Each slot is paired with a cell
    // buffer so the CPU can prepare the next frame while the GPU reads the
    // previous slot.
    retained_surface: Retained<ProtocolObject<dyn MTLTexture>>,
    active_cell_buffers: [Retained<ProtocolObject<dyn MTLBuffer>>; NUM_BUFFERS],
    retained_uniform_buffers: [Retained<ProtocolObject<dyn MTLBuffer>>; NUM_BUFFERS],
    copy_uniform_buffers: [Retained<ProtocolObject<dyn MTLBuffer>>; NUM_BUFFERS],
    retained_surface_initialized: bool,

    // Atlas texture
    pub atlas_texture: Retained<ProtocolObject<dyn MTLTexture>>,

    // Grid dimensions
    pub cols: u32,
    pub rows: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub scale_factor: f64,
    pub notch_px: u32,

    // Track whether we need to render (deferred frame or previous drawable miss)
    pub(crate) needs_render: bool,

    // Previous cursor state — re-render when cursor moves without dirtying cells
    prev_cursor_row: u32,
    prev_cursor_col: u32,
    prev_cursor_visible: bool,
}

impl MetalRenderer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view: &NSView,
        scale_factor: f64,
        width: u32,
        height: u32,
        cols: u32,
        rows: u32,
        cell_width: u32,
        cell_height: u32,
        notch_px: u32,
    ) -> Self {
        let core = MetalCore::new_with_tiled();
        let device = core.device();

        // Set up CAMetalLayer
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setPresentsWithTransaction(true);
        layer.setDisplaySyncEnabled(true);
        layer.setOpaque(true);
        layer.setFramebufferOnly(false); // compute shader writes to texture

        // Attach layer to NSView (layer-backed, then replace the layer)
        view.setWantsLayer(true);
        // SAFETY: layer.as_ptr() returns a valid CAMetalLayer pointer. setLayer:
        // accepts any CALayer subclass, which CAMetalLayer is. The view retains
        // the layer, and both outlive this call.
        unsafe {
            let layer_obj: *mut objc2::runtime::AnyObject =
                (Retained::as_ptr(&layer) as *mut CAMetalLayer).cast();
            let _: () = objc2::msg_send![view, setLayer: layer_obj];
        }
        layer.setContentsScale(scale_factor);

        layer.setDrawableSize(CGSize::new(width as f64, height as f64));

        // Double-buffered cell data (Cell matches the compute buffer ABI)
        let buffer_size = (cols as usize * rows as usize * CELL_SIZE) as u64;
        let cell_buffers = [
            device
                .newBufferWithLength_options(
                    buffer_size as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create cell buffer"),
            device
                .newBufferWithLength_options(
                    buffer_size as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create cell buffer"),
        ];
        let buffer_ready = [
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        ];

        let active_buffer_size = cols as usize * rows as usize * mem::size_of::<u32>();
        let active_cell_buffers = [
            device
                .newBufferWithLength_options(
                    active_buffer_size,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create active cell buffer"),
            device
                .newBufferWithLength_options(
                    active_buffer_size,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create active cell buffer"),
        ];
        let retained_uniform_buffers = [
            device
                .newBufferWithLength_options(
                    mem::size_of::<Uniforms>(),
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create retained uniform buffer"),
            device
                .newBufferWithLength_options(
                    mem::size_of::<Uniforms>(),
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create retained uniform buffer"),
        ];
        let copy_uniform_buffers = [
            device
                .newBufferWithLength_options(
                    mem::size_of::<ScrollCopyUniforms>(),
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create copy uniform buffer"),
            device
                .newBufferWithLength_options(
                    mem::size_of::<ScrollCopyUniforms>(),
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create copy uniform buffer"),
        ];

        // Atlas texture (2048x2048 R8Unorm)
        let atlas_desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::R8Unorm,
                2048,
                2048,
                false,
            )
        };
        atlas_desc.setStorageMode(MTLStorageMode::Shared);
        atlas_desc.setUsage(MTLTextureUsage::ShaderRead);
        let atlas_texture = device
            .newTextureWithDescriptor(&atlas_desc)
            .expect("failed to create atlas texture");

        let surface_desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width as usize,
                height as usize,
                false,
            )
        };
        surface_desc.setStorageMode(MTLStorageMode::Private);
        surface_desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite);
        let retained_surface = device
            .newTextureWithDescriptor(&surface_desc)
            .expect("failed to create retained surface texture");

        MetalRenderer {
            core,
            layer,
            cell_buffers,
            current_buffer: 0,
            buffer_ready,
            pending: [bitvec![1; rows as usize], bitvec![1; rows as usize]],
            retained_surface,
            active_cell_buffers,
            retained_uniform_buffers,
            copy_uniform_buffers,
            retained_surface_initialized: false,
            atlas_texture,
            cols,
            rows,
            cell_width,
            cell_height,
            scale_factor,
            notch_px,
            needs_render: true,
            prev_cursor_row: 0,
            prev_cursor_col: 0,
            prev_cursor_visible: true,
        }
    }

    /// Render a frame. Only dispatches GPU work if content changed.
    /// Returns true if GPU work was dispatched, false if the frame was idle.
    /// Cell data is memcpy'd directly — Cell IS the GPU format.
    pub fn render_frame(
        &mut self,
        grid: &mut Grid,
        scrollback: &Scrollback,
        viewport_offset: usize,
        cursor_visible: bool,
    ) -> bool {
        // Semantic scroll retention is not enabled yet; consume hints rather than
        // letting them survive into a later frame with no matching surface update.
        let _ = grid.take_scroll_hint();

        // Merge grid dirty rows into both per-buffer pending bitsets
        let mut had_new_dirty = false;
        for (i, bit) in grid.dirty.iter().enumerate() {
            if *bit {
                had_new_dirty = true;
                self.pending[0].set(i, true);
                self.pending[1].set(i, true);
            }
        }
        grid.clear_dirty();

        // Cursor is a uniform overlay — detect position/visibility changes
        let cursor_row = grid.cursor_row as u32;
        let cursor_col = grid.cursor_col as u32;
        let previous_cursor = (
            self.prev_cursor_row,
            self.prev_cursor_col,
            self.prev_cursor_visible,
        );
        let cursor_changed = cursor_row != self.prev_cursor_row
            || cursor_col != self.prev_cursor_col
            || cursor_visible != self.prev_cursor_visible;
        if cursor_changed {
            self.prev_cursor_row = cursor_row;
            self.prev_cursor_col = cursor_col;
            self.prev_cursor_visible = cursor_visible;
        }

        // Render when: new dirty rows, cursor changed, or deferred render.
        let need_frame = had_new_dirty || cursor_changed || self.needs_render;
        if !need_frame {
            return false;
        }

        self.render_retained_frame(
            grid,
            scrollback,
            viewport_offset,
            cursor_visible,
            previous_cursor,
        )
    }

    fn render_retained_frame(
        &mut self,
        grid: &Grid,
        scrollback: &Scrollback,
        viewport_offset: usize,
        cursor_visible: bool,
        previous_cursor: (u32, u32, bool),
    ) -> bool {
        let cur = self.current_buffer;
        if !self.buffer_ready[cur].load(Ordering::Acquire) {
            self.needs_render = true;
            return false;
        }

        autoreleasepool(|_| {
            let drawable = match self.layer.nextDrawable() {
                Some(d) => d,
                None => {
                    self.needs_render = true;
                    return false;
                }
            };

            let cols = self.cols as usize;
            let rows = self.rows as usize;
            let texture = drawable.texture();
            let retained_surface = &self.retained_surface;
            let active_cell_buffers = &self.active_cell_buffers;
            let retained_uniform_buffers = &self.retained_uniform_buffers;
            let copy_uniform_buffers = &self.copy_uniform_buffers;
            let dst_base = self.cell_buffers[cur].contents().as_ptr() as *mut Cell;
            let resident = unsafe { std::slice::from_raw_parts_mut(dst_base, rows * cols) };
            for cell in resident.iter_mut() {
                cell.flags = CellFlags::from_bits_retain(cell.flags.bits() | TILED_SKIP_FLAG);
            }

            for (row, pending) in self.pending[cur].iter().enumerate() {
                if !*pending {
                    continue;
                }

                let source: Option<&[Cell]> = if viewport_offset > 0 && row < viewport_offset {
                    let sb_idx = viewport_offset - 1 - row;
                    scrollback.row(sb_idx)
                } else if row >= viewport_offset {
                    Some(grid.row_slice((row - viewport_offset) as u16))
                } else {
                    None
                };
                let Some(source) = source else {
                    continue;
                };

                let source_len = source.len().min(cols);
                let destination = &mut resident[row * cols..(row + 1) * cols];
                let mut left = source_len;
                let mut right = 0usize;
                for col in 0..source_len {
                    let old = &destination[col];
                    let flags = old.flags.bits() & !TILED_SKIP_FLAG;
                    let changed = source[col].codepoint != old.codepoint
                        || source[col].flags.bits() != flags
                        || source[col].fg_index != old.fg_index
                        || source[col].bg_index != old.bg_index
                        || source[col].atlas_x != old.atlas_x
                        || source[col].atlas_y != old.atlas_y;
                    if changed {
                        left = left.min(col);
                        right = right.max(col + 1);
                        destination[col].flags =
                            CellFlags::from_bits_retain(flags & !TILED_SKIP_FLAG);
                    }
                }

                if source_len < cols {
                    for cell in &mut destination[source_len..] {
                        cell.flags =
                            CellFlags::from_bits_retain(cell.flags.bits() & !TILED_SKIP_FLAG);
                    }
                    left = left.min(source_len);
                    right = cols;
                }

                if left < right {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            source[left..right].as_ptr(),
                            destination.as_mut_ptr().add(left),
                            right - left,
                        );
                    }
                }
            }

            if !self.retained_surface_initialized {
                for cell in resident.iter_mut() {
                    cell.flags = CellFlags::from_bits_retain(cell.flags.bits() & !TILED_SKIP_FLAG);
                }
            }

            for (row, col, _) in [
                previous_cursor,
                (
                    grid.cursor_row as u32,
                    grid.cursor_col as u32,
                    cursor_visible,
                ),
            ] {
                if (row as usize) < rows && (col as usize) < cols {
                    let cell = &mut resident[row as usize * cols + col as usize];
                    cell.flags = CellFlags::from_bits_retain(cell.flags.bits() & !TILED_SKIP_FLAG);
                }
            }

            let active_base = active_cell_buffers[cur].contents().as_ptr() as *mut u32;
            let mut active_count = 0usize;
            for (index, cell) in resident.iter().enumerate() {
                if cell.flags.bits() & TILED_SKIP_FLAG == 0 {
                    unsafe {
                        active_base.add(active_count).write(index as u32);
                    }
                    active_count += 1;
                }
            }

            let padding = (config::PADDING as f64 * self.scale_factor) as u32;
            let uniform = Uniforms {
                cols: self.cols,
                rows: self.rows,
                cell_width: self.cell_width,
                cell_height: self.cell_height,
                atlas_cell_width: self.cell_width,
                atlas_cell_height: self.cell_height,
                padding,
                padding_top: self.notch_px.max(padding),
                cursor_row: grid.cursor_row as u32,
                cursor_col: grid.cursor_col as u32,
                cursor_visible: u32::from(cursor_visible),
                frame_bg: config::DEFAULT_BG,
                damage_origin_x: 0,
                damage_origin_y: 0,
            };
            unsafe {
                (retained_uniform_buffers[cur].contents().as_ptr() as *mut Uniforms).write(uniform);
                (copy_uniform_buffers[cur].contents().as_ptr() as *mut ScrollCopyUniforms).write(
                    ScrollCopyUniforms {
                        source_y: 0,
                        destination_y: 0,
                        width: texture.width() as u32,
                        height: texture.height() as u32,
                    },
                );
            }

            let command_buffer = self
                .core
                .command_queue()
                .commandBuffer()
                .expect("failed to create retained command buffer");

            if !self.retained_surface_initialized {
                let encoder = command_buffer
                    .computeCommandEncoder()
                    .expect("failed to create retained initialization encoder");
                encoder.setComputePipelineState(self.core.tiled_pipeline());
                unsafe {
                    encoder.setTexture_atIndex(Some(retained_surface), 0);
                    encoder.setTexture_atIndex(Some(&self.atlas_texture), 1);
                    encoder.setBuffer_offset_atIndex(Some(&self.cell_buffers[cur]), 0, 0);
                    encoder.setBuffer_offset_atIndex(Some(self.core.palette_buffer()), 0, 1);
                    encoder.setBuffer_offset_atIndex(Some(&retained_uniform_buffers[cur]), 0, 2);
                }
                encoder.dispatchThreadgroups_threadsPerThreadgroup(
                    MTLSize {
                        width: self.cols as usize,
                        height: self.rows as usize,
                        depth: 1,
                    },
                    MTLSize {
                        width: self.cell_width as usize,
                        height: self.cell_height as usize,
                        depth: 1,
                    },
                );
                encoder.endEncoding();
            } else if active_count > 0 {
                let encoder = command_buffer
                    .computeCommandEncoder()
                    .expect("failed to create retained active-cell encoder");
                encoder.setComputePipelineState(self.core.tiled_list_pipeline());
                unsafe {
                    encoder.setTexture_atIndex(Some(retained_surface), 0);
                    encoder.setTexture_atIndex(Some(&self.atlas_texture), 1);
                    encoder.setBuffer_offset_atIndex(Some(&self.cell_buffers[cur]), 0, 0);
                    encoder.setBuffer_offset_atIndex(Some(self.core.palette_buffer()), 0, 1);
                    encoder.setBuffer_offset_atIndex(Some(&retained_uniform_buffers[cur]), 0, 2);
                    encoder.setBuffer_offset_atIndex(Some(&active_cell_buffers[cur]), 0, 3);
                }
                encoder.dispatchThreadgroups_threadsPerThreadgroup(
                    MTLSize {
                        width: active_count,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: self.cell_width as usize,
                        height: self.cell_height as usize,
                        depth: 1,
                    },
                );
                encoder.endEncoding();
            }

            let copy_encoder = command_buffer
                .computeCommandEncoder()
                .expect("failed to create retained surface copy encoder");
            copy_encoder.setComputePipelineState(self.core.scroll_pipeline());
            unsafe {
                copy_encoder.setTexture_atIndex(Some(retained_surface), 0);
                copy_encoder.setTexture_atIndex(Some(&texture), 1);
                copy_encoder.setBuffer_offset_atIndex(Some(&copy_uniform_buffers[cur]), 0, 0);
            }
            copy_encoder.dispatchThreads_threadsPerThreadgroup(
                MTLSize {
                    width: texture.width(),
                    height: texture.height(),
                    depth: 1,
                },
                MTLSize {
                    width: 16,
                    height: 16,
                    depth: 1,
                },
            );
            copy_encoder.endEncoding();

            self.buffer_ready[cur].store(false, Ordering::Release);
            let ready_flag = self.buffer_ready[cur].clone();
            let handler = RcBlock::new(move |_cb| {
                ready_flag.store(true, Ordering::Release);
            });
            unsafe {
                command_buffer.addCompletedHandler(RcBlock::as_ptr(&handler));
            }

            command_buffer.commit();
            command_buffer.waitUntilScheduled();
            drawable.present();

            self.pending[cur].fill(false);
            self.retained_surface_initialized = true;
            self.current_buffer = (self.current_buffer + 1) % NUM_BUFFERS;
            self.needs_render = false;
            true
        })
    }

    /// Resize the Metal layer and reallocate buffers.
    /// NOTE: width/height are already in physical pixels (from winit).
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.scale_factor = scale;
        self.layer
            .setDrawableSize(CGSize::new(width as f64, height as f64));
        self.layer.setContentsScale(scale);

        // Recalculate grid dimensions (all in physical pixels already)
        let padding_px = (config::PADDING as f64 * scale) as u32;
        let padding_top_px = self.notch_px.max(padding_px);
        let usable_w = width - padding_px * 2;
        let usable_h = height - padding_top_px - padding_px;
        self.cols = usable_w / self.cell_width;
        self.rows = usable_h / self.cell_height;

        // Wait for any in-flight GPU work before reallocating
        for i in 0..NUM_BUFFERS {
            while !self.buffer_ready[i].load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        }

        // Reallocate both cell buffers
        let buffer_size = (self.cols as usize * self.rows as usize * CELL_SIZE) as u64;
        self.cell_buffers = [
            self.core
                .device()
                .newBufferWithLength_options(
                    buffer_size as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create cell buffer"),
            self.core
                .device()
                .newBufferWithLength_options(
                    buffer_size as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create cell buffer"),
        ];
        let active_buffer_size = self.cols as usize * self.rows as usize * mem::size_of::<u32>();
        self.active_cell_buffers = [
            self.core
                .device()
                .newBufferWithLength_options(
                    active_buffer_size,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create active cell buffer"),
            self.core
                .device()
                .newBufferWithLength_options(
                    active_buffer_size,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create active cell buffer"),
        ];
        let surface_desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width as usize,
                height as usize,
                false,
            )
        };
        surface_desc.setStorageMode(MTLStorageMode::Private);
        surface_desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite);
        self.retained_surface = self
            .core
            .device()
            .newTextureWithDescriptor(&surface_desc)
            .expect("failed to create retained surface texture");
        self.retained_surface_initialized = false;
        // Mark all rows pending in both buffers after resize
        self.pending = [
            bitvec![1; self.rows as usize],
            bitvec![1; self.rows as usize],
        ];
        self.needs_render = true;
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        self.core.device()
    }
}

impl Renderer for MetalRenderer {
    fn render_frame(
        &mut self,
        grid: &mut Grid,
        scrollback: &Scrollback,
        viewport_offset: usize,
        cursor_visible: bool,
    ) -> bool {
        self.render_frame(grid, scrollback, viewport_offset, cursor_visible)
    }

    fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.resize(width, height, scale);
    }

    fn cols(&self) -> u32 {
        self.cols
    }

    fn rows(&self) -> u32 {
        self.rows
    }

    fn cell_width(&self) -> u32 {
        self.cell_width
    }

    fn cell_height(&self) -> u32 {
        self.cell_height
    }

    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    fn notch_px(&self) -> u32 {
        self.notch_px
    }

    fn needs_render(&self) -> bool {
        self.needs_render
    }
}
