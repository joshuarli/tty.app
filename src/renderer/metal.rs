use std::ffi::c_void;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bitvec::prelude::*;
use block::ConcreteBlock;
use core_graphics_types::geometry::CGSize;
use metal::foreign_types::ForeignType;
use metal::*;
use objc2_app_kit::NSView;

use crate::config;
use crate::terminal::cell::Cell;
use crate::terminal::grid::Grid;

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
}

const CELL_SIZE: usize = mem::size_of::<Cell>();
const _: () = assert!(CELL_SIZE == 8, "Cell must be 8 bytes for GPU layout");

/// Number of cell buffers for pipelining (CPU uploads to one while GPU reads the other).
const NUM_BUFFERS: usize = 2;

pub struct MetalRenderer {
    device: Device,
    command_queue: CommandQueue,
    pipeline: ComputePipelineState,
    layer: MetalLayer,

    // Double-buffered cell data — CPU writes to one while GPU reads the other
    cell_buffers: [Buffer; NUM_BUFFERS],
    current_buffer: usize,
    buffer_ready: [Arc<AtomicBool>; NUM_BUFFERS],
    // Per-buffer dirty row tracking: each dirty row must be copied to BOTH buffers
    pending: [BitVec; NUM_BUFFERS],

    // Palette buffer (256 × half4)
    palette_buffer: Buffer,

    // Uniform buffer
    uniform_buffer: Buffer,

    // Atlas texture
    pub atlas_texture: Texture,

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
        let device = Device::system_default().expect("no Metal device found");
        let command_queue = device.new_command_queue();

        // Set up CAMetalLayer
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        layer.set_presents_with_transaction(true);
        layer.set_display_sync_enabled(true);
        layer.set_opaque(true);
        layer.set_framebuffer_only(false); // compute shader writes to texture

        // Attach layer to NSView (layer-backed, then replace the layer)
        view.setWantsLayer(true);
        // SAFETY: layer.as_ptr() returns a valid CAMetalLayer pointer. setLayer:
        // accepts any CALayer subclass, which CAMetalLayer is. The view retains
        // the layer, and both outlive this call.
        unsafe {
            let layer_obj: *mut objc2::runtime::AnyObject = layer.as_ptr().cast();
            let _: () = objc2::msg_send![view, setLayer: layer_obj];
        }
        layer.set_contents_scale(scale_factor);

        layer.set_drawable_size(CGSize::new(width as f64, height as f64));

        // Compile shader from source at runtime
        let shader_source = include_str!("shader.metal");
        let compile_options = CompileOptions::new();
        compile_options.set_fast_math_enabled(true);
        let library = device
            .new_library_with_source(shader_source, &compile_options)
            .expect("failed to compile Metal shader");
        let function = library
            .get_function("render", None)
            .expect("shader function 'render' not found");
        let pipeline = device
            .new_compute_pipeline_state_with_function(&function)
            .expect("failed to create compute pipeline");

        // Double-buffered cell data (Cell is the GPU format — no conversion needed)
        let buffer_size = (cols as usize * rows as usize * CELL_SIZE) as u64;
        let cell_buffers = [
            device.new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
            device.new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
        ];
        let buffer_ready = [
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        ];

        // Palette buffer (256 × half4 = 256 × 4 × 2 bytes)
        let palette_data = Self::build_palette_buffer();
        let palette_buffer = device.new_buffer_with_data(
            palette_data.as_ptr() as *const c_void,
            (256 * 4 * mem::size_of::<u16>()) as u64,
            MTLResourceOptions::StorageModeShared,
        );

        // Uniform buffer
        let uniform_buffer = device.new_buffer(
            mem::size_of::<Uniforms>() as u64,
            MTLResourceOptions::StorageModeShared,
        );

        // Atlas texture (2048x2048 R8Unorm)
        let atlas_desc = TextureDescriptor::new();
        atlas_desc.set_pixel_format(MTLPixelFormat::R8Unorm);
        atlas_desc.set_width(2048);
        atlas_desc.set_height(2048);
        atlas_desc.set_storage_mode(MTLStorageMode::Shared);
        atlas_desc.set_usage(MTLTextureUsage::ShaderRead);
        let atlas_texture = device.new_texture(&atlas_desc);

        MetalRenderer {
            device,
            command_queue,
            pipeline,
            layer,
            cell_buffers,
            current_buffer: 0,
            buffer_ready,
            pending: [bitvec![1; rows as usize], bitvec![1; rows as usize]],
            palette_buffer,
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

    /// Convert f32 to IEEE 754 half-precision (f16) stored as u16.
    fn f32_to_f16(val: f32) -> u16 {
        let bits = val.to_bits();
        let sign = (bits >> 16) & 0x8000;
        let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
        let frac = bits & 0x007F_FFFF;
        if exp <= 0 {
            0 // flush subnormals to zero — palette values are [0,1]
        } else if exp >= 31 {
            (sign | 0x7C00) as u16 // infinity
        } else {
            (sign | ((exp as u32) << 10) | (frac >> 13)) as u16
        }
    }

    fn build_palette_buffer() -> Vec<u16> {
        let mut data = Vec::with_capacity(256 * 4);
        for &rgb in config::PALETTE.iter() {
            let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
            let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
            let b = (rgb & 0xFF) as f32 / 255.0;
            data.push(Self::f32_to_f16(r));
            data.push(Self::f32_to_f16(g));
            data.push(Self::f32_to_f16(b));
            data.push(Self::f32_to_f16(1.0));
        }
        data
    }

    /// Render a frame. Only dispatches GPU work if content changed.
    /// Returns true if GPU work was dispatched, false if the frame was idle.
    /// Cell data is memcpy'd directly — Cell IS the GPU format.
    pub fn render_frame(&mut self, grid: &mut Grid, cursor_visible: bool) -> bool {
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

        // Render when: new dirty rows, cursor changed, deferred render, or pending
        // rows in the current buffer (from a previous frame's dirty merge that was
        // uploaded to the other buffer but not this one — both buffers must stay in sync).
        let need_frame =
            had_new_dirty || cursor_changed || self.needs_render || self.pending[cur].any();
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
        let dst_base = self.cell_buffers[cur].contents() as *mut u8;
        for (row, pending) in self.pending[cur].iter().enumerate() {
            if *pending {
                let src = grid.row_slice(row as u16);
                // SAFETY: dst_base points to a Metal shared buffer sized for
                // self.cols * self.rows * CELL_SIZE bytes. row < self.rows and
                // cols <= self.cols, so the offset is in bounds.
                // src points to a contiguous slice of cols Cell values.
                unsafe {
                    let dst = dst_base.add(row * dst_stride);
                    std::ptr::copy_nonoverlapping(
                        src.as_ptr() as *const u8,
                        dst,
                        row_bytes,
                    );
                }
            }
        }
        self.pending[cur].fill(false);

        objc2::rc::autoreleasepool(|_| {
            let drawable = match self.layer.next_drawable() {
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
            };
            // SAFETY: uniform_buffer was allocated with size_of::<Uniforms>() bytes
            // of shared storage. contents() returns a valid pointer to that region.
            // No GPU command buffer is reading this buffer yet (commit happens below).
            unsafe {
                let ptr = self.uniform_buffer.contents() as *mut Uniforms;
                *ptr = uniforms;
            }

            let command_buffer = self.command_queue.new_command_buffer();
            let encoder = command_buffer.new_compute_command_encoder();

            encoder.set_compute_pipeline_state(&self.pipeline);
            encoder.set_texture(0, Some(texture));
            encoder.set_texture(1, Some(&self.atlas_texture));
            encoder.set_buffer(0, Some(&self.cell_buffers[self.current_buffer]), 0);
            encoder.set_buffer(1, Some(&self.palette_buffer), 0);
            encoder.set_buffer(2, Some(&self.uniform_buffer), 0);

            let w = texture.width();
            let h = texture.height();
            let threadgroup_size = MTLSize::new(16, 16, 1);
            let grid_size = MTLSize::new(w, h, 1);

            encoder.dispatch_threads(grid_size, threadgroup_size);
            encoder.end_encoding();

            // Mark buffer as in-flight before commit
            self.buffer_ready[self.current_buffer].store(false, Ordering::Release);

            // Signal buffer availability when GPU finishes
            let ready_flag = self.buffer_ready[self.current_buffer].clone();
            let handler = ConcreteBlock::new(move |_cb: &CommandBufferRef| {
                ready_flag.store(true, Ordering::Release);
            });
            let handler = handler.copy();
            command_buffer.add_completed_handler(&handler);

            command_buffer.commit();
            command_buffer.wait_until_scheduled();
            drawable.present();

            // Swap to the other buffer for next frame
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
            .set_drawable_size(CGSize::new(width as f64, height as f64));
        self.layer.set_contents_scale(scale);

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
            self.device
                .new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
            self.device
                .new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
        ];
        // Mark all rows pending in both buffers after resize
        self.pending = [
            bitvec![1; self.rows as usize],
            bitvec![1; self.rows as usize],
        ];
        self.needs_render = true;
    }

    pub fn device(&self) -> &Device {
        &self.device
    }
}
