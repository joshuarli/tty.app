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
use crate::terminal::cell::Cell;
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

const CELL_SIZE: usize = mem::size_of::<Cell>();
const _: () = assert!(CELL_SIZE == 8, "Cell must be 8 bytes for GPU layout");

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

    // Uniform buffer
    uniform_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,

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
        let core = MetalCore::new();
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

        // Uniform buffer
        let uniform_buffer = device
            .newBufferWithLength_options(
                mem::size_of::<Uniforms>(),
                MTLResourceOptions::StorageModeShared,
            )
            .expect("failed to create uniform buffer");

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

        MetalRenderer {
            core,
            layer,
            cell_buffers,
            current_buffer: 0,
            buffer_ready,
            pending: [bitvec![1; rows as usize], bitvec![1; rows as usize]],
            uniform_buffer,
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
        // The current production path does not retain a framebuffer, so consume
        // semantic scroll hints rather than letting them survive into a later frame.
        let _ = grid.take_scroll_hint();

        // Merge grid dirty rows into both per-buffer pending bitsets
        let cur = self.current_buffer;
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

        // If the GPU is still reading this buffer, skip the frame and retry next
        // iteration. This keeps the event loop non-blocking so PTY data continues
        // to be drained while the GPU catches up.
        if !self.buffer_ready[cur].load(Ordering::Acquire) {
            self.needs_render = true;
            return false;
        }

        // Upload dirty rows — Cell IS the GPU format, so this is a raw memcpy per row.
        let cols = self.cols.min(grid.cols as u32) as usize;
        let row_bytes = cols * CELL_SIZE;
        let dst_stride = self.cols as usize * CELL_SIZE;
        let dst_base = self.cell_buffers[cur].contents().as_ptr() as *mut u8;
        for (row, pending) in self.pending[cur].iter().enumerate() {
            if *pending {
                // Source row from scrollback or grid based on viewport offset
                let src: &[Cell] = if viewport_offset > 0 && row < viewport_offset {
                    // This visible row comes from scrollback history
                    let sb_idx = viewport_offset - 1 - row;
                    match scrollback.row(sb_idx) {
                        Some(r) => r,
                        None => continue,
                    }
                } else {
                    grid.row_slice((row - viewport_offset) as u16)
                };
                let copy_bytes = (src.len().min(cols)) * CELL_SIZE;
                // SAFETY: dst_base points to a Metal shared buffer sized for
                // self.cols * self.rows * CELL_SIZE bytes. row < self.rows and
                // cols <= self.cols, so the offset is in bounds.
                // src points to a contiguous slice of Cell values.
                unsafe {
                    let dst = dst_base.add(row * dst_stride);
                    std::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, dst, copy_bytes);
                    // Clear remainder if scrollback row is shorter than current cols
                    if copy_bytes < row_bytes {
                        std::ptr::write_bytes(dst.add(copy_bytes), 0, row_bytes - copy_bytes);
                    }
                }
            }
        }
        self.pending[cur].fill(false);

        autoreleasepool(|_| {
            let drawable = match self.layer.nextDrawable() {
                Some(d) => d,
                None => {
                    // No drawable available — retry next frame
                    self.needs_render = true;
                    return false;
                }
            };

            let texture = drawable.texture();

            // Update uniforms
            let padding = (config::PADDING as f64 * self.scale_factor) as u32;
            let uniforms = Uniforms {
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
                cursor_visible: if cursor_visible { 1 } else { 0 },
                frame_bg: config::DEFAULT_BG,
                damage_origin_x: 0,
                damage_origin_y: 0,
            };
            // SAFETY: uniform_buffer was allocated with size_of::<Uniforms>() bytes
            // of shared storage. contents() returns a valid pointer to that region.
            // No GPU command buffer is reading this buffer yet (commit happens below).
            unsafe {
                let ptr = self.uniform_buffer.contents().as_ptr() as *mut Uniforms;
                *ptr = uniforms;
            }

            let command_buffer = self
                .core
                .command_queue()
                .commandBuffer()
                .expect("failed to create command buffer");
            let encoder = command_buffer
                .computeCommandEncoder()
                .expect("failed to create compute command encoder");

            encoder.setComputePipelineState(self.core.pipeline());
            unsafe {
                encoder.setTexture_atIndex(Some(&texture), 0);
                encoder.setTexture_atIndex(Some(&self.atlas_texture), 1);
                encoder.setBuffer_offset_atIndex(
                    Some(&self.cell_buffers[self.current_buffer]),
                    0,
                    0,
                );
                encoder.setBuffer_offset_atIndex(Some(self.core.palette_buffer()), 0, 1);
                encoder.setBuffer_offset_atIndex(Some(&self.uniform_buffer), 0, 2);
            }

            let w = texture.width();
            let h = texture.height();
            let threadgroup_size = MTLSize {
                width: 16,
                height: 16,
                depth: 1,
            };
            let grid_size = MTLSize {
                width: w,
                height: h,
                depth: 1,
            };

            encoder.dispatchThreads_threadsPerThreadgroup(grid_size, threadgroup_size);
            encoder.endEncoding();

            // Mark buffer as in-flight before commit
            self.buffer_ready[self.current_buffer].store(false, Ordering::Release);

            // Signal buffer availability when GPU finishes
            let ready_flag = self.buffer_ready[self.current_buffer].clone();
            let handler = RcBlock::new(move |_cb| {
                ready_flag.store(true, Ordering::Release);
            });
            unsafe {
                command_buffer.addCompletedHandler(RcBlock::as_ptr(&handler));
            }

            command_buffer.commit();
            command_buffer.waitUntilScheduled();
            drawable.present();

            // Swap to the other buffer for next frame.
            // No proactive sync render needed — the other buffer's pending rows
            // accumulate and get uploaded the next time it's used for a real render
            // (new dirty rows or cursor change). This lazy convergence avoids
            // back-to-back renders that exhaust Metal's 3-drawable pool.
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
