use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub cursor_visible: u32,
    pub frame_bg: u32,
}

/// GPU-side cell data. Must match Metal CellData struct exactly (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellData {
    pub codepoint: u16,
    pub flags: u16,
    pub fg_index: u8,
    pub bg_index: u8,
    pub atlas_x: u8,
    pub atlas_y: u8,
    pub fg_rgb: u32,
    pub bg_rgb: u32,
}

const CELL_DATA_SIZE: usize = mem::size_of::<CellData>();
const _: () = assert!(CELL_DATA_SIZE == 16, "CellData must be 16 bytes");

// Cell and CellData must have identical layout for bulk memcpy.
const _: () = assert!(
    mem::size_of::<Cell>() == CELL_DATA_SIZE,
    "Cell and CellData size mismatch"
);

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

    // Palette buffer (256 × float4)
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

    // Track whether we need to render
    needs_render: bool,
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
    ) -> Self {
        let device = Device::system_default().expect("no Metal device found");
        let command_queue = device.new_command_queue();

        // Set up CAMetalLayer
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        layer.set_presents_with_transaction(true);
        layer.set_display_sync_enabled(false);
        layer.set_opaque(true);
        layer.set_framebuffer_only(false); // compute shader writes to texture

        // Attach layer to NSView (layer-backed, then replace the layer)
        view.setWantsLayer(true);
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

        // Double-buffered cell data
        let buffer_size = (cols as usize * rows as usize * CELL_DATA_SIZE) as u64;
        let cell_buffers = [
            device.new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
            device.new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
        ];
        let buffer_ready = [
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        ];

        // Palette buffer (256 × [f32; 4])
        let palette_data = Self::build_palette_buffer();
        let palette_buffer = device.new_buffer_with_data(
            palette_data.as_ptr() as *const c_void,
            (256 * 4 * mem::size_of::<f32>()) as u64,
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
            palette_buffer,
            uniform_buffer,
            atlas_texture,
            cols,
            rows,
            cell_width,
            cell_height,
            scale_factor,
            needs_render: true,
        }
    }

    fn build_palette_buffer() -> Vec<f32> {
        let mut data = Vec::with_capacity(256 * 4);
        for &rgb in config::PALETTE.iter() {
            let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
            let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
            let b = (rgb & 0xFF) as f32 / 255.0;
            data.push(r);
            data.push(g);
            data.push(b);
            data.push(1.0);
        }
        data
    }

    /// Render a frame. Only dispatches GPU work if content changed.
    /// Cell data is bulk-copied from the grid (Cell and CellData share identical repr(C) layout).
    /// Bold brightness and hidden attribute are handled in the shader.
    pub fn render_frame(&mut self, grid: &mut Grid, cursor_visible: bool) -> bool {
        let any_dirty = grid.dirty.any();
        grid.clear_dirty();

        if !any_dirty && !self.needs_render {
            return true;
        }
        self.needs_render = false;

        // Wait for the target buffer to be free (GPU finished reading it).
        // In practice this rarely spins — the GPU is at most 1 frame behind.
        while !self.buffer_ready[self.current_buffer].load(Ordering::Acquire) {
            std::hint::spin_loop();
        }

        // Bulk-copy the entire grid into the current cell buffer.
        // Cell is #[repr(C)] with identical layout to CellData — this is a plain memcpy.
        let grid_cells = grid.cols as usize * grid.rows as usize;
        let copy_cells = grid_cells.min(self.cols as usize * self.rows as usize);
        let byte_count = copy_cells * CELL_DATA_SIZE;
        unsafe {
            let src = grid.cells.as_ptr() as *const u8;
            let dst = self.cell_buffers[self.current_buffer].contents() as *mut u8;
            ptr::copy_nonoverlapping(src, dst, byte_count);
        }

        objc2::rc::autoreleasepool(|_| {
            let drawable = match self.layer.next_drawable() {
                Some(d) => d,
                None => return false,
            };

            let texture = drawable.texture();

            // Update uniforms
            let uniforms = Uniforms {
                cols: self.cols,
                rows: self.rows,
                cell_width: self.cell_width,
                cell_height: self.cell_height,
                atlas_cell_width: self.cell_width,
                atlas_cell_height: self.cell_height,
                padding: (config::PADDING as f64 * self.scale_factor) as u32,
                cursor_row: grid.cursor_row as u32,
                cursor_col: grid.cursor_col as u32,
                cursor_visible: if cursor_visible { 1 } else { 0 },
                frame_bg: config::DEFAULT_BG,
            };
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
        let usable_w = width - padding_px * 2;
        let usable_h = height - padding_px * 2;
        self.cols = usable_w / self.cell_width;
        self.rows = usable_h / self.cell_height;

        // Wait for any in-flight GPU work before reallocating
        for i in 0..NUM_BUFFERS {
            while !self.buffer_ready[i].load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        }

        // Reallocate both cell buffers
        let buffer_size = (self.cols as usize * self.rows as usize * CELL_DATA_SIZE) as u64;
        self.cell_buffers = [
            self.device
                .new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
            self.device
                .new_buffer(buffer_size, MTLResourceOptions::StorageModeShared),
        ];
        self.needs_render = true;
    }

    pub fn device(&self) -> &Device {
        &self.device
    }
}
