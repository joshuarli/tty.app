use std::ffi::c_void;
use std::mem;
use std::sync::{Arc, Mutex};

use cocoa::appkit::NSView;
use cocoa::base::id as cocoa_id;
use core_graphics_types::geometry::CGSize;
use metal::*;
use objc::rc::autoreleasepool;
use objc::runtime::YES;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

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

const TRIPLE_BUFFER_COUNT: usize = 3;

pub struct MetalRenderer {
    device: Device,
    command_queue: CommandQueue,
    pipeline: ComputePipelineState,
    layer: MetalLayer,

    // Triple-buffered cell data
    cell_buffers: [Buffer; TRIPLE_BUFFER_COUNT],
    frame_index: usize,

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
}

impl MetalRenderer {
    pub fn new(window: &winit::window::Window, cols: u32, rows: u32, cell_width: u32, cell_height: u32) -> Self {
        let device = Device::system_default().expect("no Metal device found");
        let command_queue = device.new_command_queue();

        // Set up CAMetalLayer (inline raw-window-metal logic)
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        layer.set_presents_with_transaction(false);
        layer.set_display_sync_enabled(true);
        layer.set_opaque(true);
        layer.set_framebuffer_only(false); // compute shader writes to texture

        // Attach layer to NSView
        let scale_factor = window.scale_factor();
        unsafe {
            if let RawWindowHandle::AppKit(handle) = window.window_handle().unwrap().as_raw() {
                let view = handle.ns_view.as_ptr() as cocoa_id;
                view.setWantsLayer(YES);
                let _: () = msg_send![view, setLayer: layer.as_ref()];
            }
        }
        layer.set_contents_scale(scale_factor);

        let window_size = window.inner_size();
        layer.set_drawable_size(CGSize::new(window_size.width as f64, window_size.height as f64));

        // Compile shader from source at runtime
        let shader_source = include_str!("shader.metal");
        let compile_options = CompileOptions::new();
        compile_options.set_fast_math_enabled(true);
        let library = device
            .new_library_with_source(shader_source, &compile_options)
            .expect("failed to compile Metal shader");
        let function = library.get_function("render", None).expect("shader function 'render' not found");
        let pipeline = device
            .new_compute_pipeline_state_with_function(&function)
            .expect("failed to create compute pipeline");

        // Allocate triple-buffered cell buffers
        let max_cells = (cols * rows) as usize;
        let buffer_size = max_cells * CELL_DATA_SIZE;
        let cell_buffers = std::array::from_fn(|_| {
            device.new_buffer(buffer_size as u64, MTLResourceOptions::StorageModeShared)
        });

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
            frame_index: 0,
            palette_buffer,
            uniform_buffer,
            atlas_texture,
            cols,
            rows,
            cell_width,
            cell_height,
            scale_factor,
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

    /// Upload dirty rows from the grid to the current frame's cell buffer.
    /// Returns true if anything was dirty.
    pub fn upload_dirty_rows(&mut self, grid: &Grid) -> bool {
        let dirty = &grid.dirty;
        let mut any_dirty = false;

        let buf = &self.cell_buffers[self.frame_index % TRIPLE_BUFFER_COUNT];
        let buf_ptr = buf.contents() as *mut CellData;

        for row in 0..self.rows.min(grid.rows as u32) {
            if !dirty[row as usize] {
                continue;
            }
            any_dirty = true;
            let row_offset = (row * self.cols) as usize;
            let src_offset = (row as u16 * grid.cols) as usize;
            let copy_cols = self.cols.min(grid.cols as u32) as usize;

            unsafe {
                let dst = buf_ptr.add(row_offset);
                for col in 0..copy_cols {
                    let cell = &grid.cells[src_offset + col];
                    *dst.add(col) = cell.to_cell_data();
                }
            }
        }

        any_dirty
    }

    /// Render a frame. Returns false if no drawable was available.
    pub fn render_frame(&mut self, grid: &mut Grid, cursor_visible: bool) -> bool {
        let has_dirty = self.upload_dirty_rows(grid);
        grid.clear_dirty();

        if !has_dirty {
            return true;
        }

        autoreleasepool(|| {
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
            encoder.set_texture(0, Some(&texture));
            encoder.set_texture(1, Some(&self.atlas_texture));
            encoder.set_buffer(
                0,
                Some(&self.cell_buffers[self.frame_index % TRIPLE_BUFFER_COUNT]),
                0,
            );
            encoder.set_buffer(1, Some(&self.palette_buffer), 0);
            encoder.set_buffer(2, Some(&self.uniform_buffer), 0);

            let w = texture.width();
            let h = texture.height();
            let threadgroup_size = MTLSize::new(16, 16, 1);
            let grid_size = MTLSize::new(w, h, 1);

            encoder.dispatch_threads(grid_size, threadgroup_size);
            encoder.end_encoding();

            command_buffer.present_drawable(&drawable);
            command_buffer.commit();

            self.frame_index += 1;
            true
        })
    }

    /// Resize the Metal layer and reallocate buffers.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.scale_factor = scale;
        let physical_w = (width as f64 * scale) as u64;
        let physical_h = (height as f64 * scale) as u64;
        self.layer.set_drawable_size(CGSize::new(physical_w as f64, physical_h as f64));
        self.layer.set_contents_scale(scale);

        // Recalculate grid dimensions
        let padding_px = (config::PADDING as f64 * scale) as u32;
        let usable_w = physical_w as u32 - padding_px * 2;
        let usable_h = physical_h as u32 - padding_px * 2;
        self.cols = usable_w / self.cell_width;
        self.rows = usable_h / self.cell_height;

        // Reallocate cell buffers
        let max_cells = (self.cols * self.rows) as usize;
        let buffer_size = max_cells * CELL_DATA_SIZE;
        for i in 0..TRIPLE_BUFFER_COUNT {
            self.cell_buffers[i] =
                self.device.new_buffer(buffer_size as u64, MTLResourceOptions::StorageModeShared);
        }
    }

    pub fn device(&self) -> &Device {
        &self.device
    }
}
