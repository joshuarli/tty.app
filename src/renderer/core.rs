use std::ffi::c_void;
use std::mem;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::*;

use crate::config;

/// Reusable Metal resources shared by onscreen and headless renderers.
pub struct MetalCore {
    pub(crate) device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub(crate) command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    #[allow(dead_code)]
    pub(crate) pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    #[allow(dead_code)]
    pub(crate) tiled_pipeline: Option<Retained<ProtocolObject<dyn MTLComputePipelineState>>>,
    #[allow(dead_code)]
    pub(crate) tiled_list_pipeline: Option<Retained<ProtocolObject<dyn MTLComputePipelineState>>>,
    #[allow(dead_code)]
    pub(crate) scroll_pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pub(crate) palette_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
}

impl MetalCore {
    /// Create the device, command queue, render pipeline, and palette buffer.
    /// No window, layer, drawable, or display connection is required.
    pub fn new() -> Self {
        Self::new_internal(false)
    }

    /// Create Metal resources including the headless tiled pipeline.
    #[allow(dead_code)]
    pub fn new_with_tiled() -> Self {
        Self::new_internal(true)
    }

    fn new_internal(with_tiled: bool) -> Self {
        let device = MTLCreateSystemDefaultDevice().expect("no Metal device found");
        let command_queue = device
            .newCommandQueue()
            .expect("failed to create Metal command queue");

        let shader_source = include_str!("shader.metal");
        let compile_options = MTLCompileOptions::new();
        compile_options.setMathMode(MTLMathMode::Fast);
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(shader_source),
                Some(&compile_options),
            )
            .expect("failed to compile Metal shader");
        let function = library
            .newFunctionWithName(&NSString::from_str("render"))
            .expect("shader function 'render' not found");
        let pipeline = device
            .newComputePipelineStateWithFunction_error(&function)
            .expect("failed to create compute pipeline");
        let scroll_function = library
            .newFunctionWithName(&NSString::from_str("scroll_copy"))
            .expect("scroll shader function 'scroll_copy' not found");
        let scroll_pipeline = device
            .newComputePipelineStateWithFunction_error(&scroll_function)
            .expect("failed to create scroll compute pipeline");
        let tiled_pipeline = if with_tiled {
            let tiled_function = library
                .newFunctionWithName(&NSString::from_str("render_tiled"))
                .expect("shader function 'render_tiled' not found");
            Some(
                device
                    .newComputePipelineStateWithFunction_error(&tiled_function)
                    .expect("failed to create tiled compute pipeline"),
            )
        } else {
            None
        };
        let tiled_list_pipeline = if with_tiled {
            let tiled_list_function = library
                .newFunctionWithName(&NSString::from_str("render_tiled_list"))
                .expect("shader function 'render_tiled_list' not found");
            Some(
                device
                    .newComputePipelineStateWithFunction_error(&tiled_list_function)
                    .expect("failed to create tiled-list compute pipeline"),
            )
        } else {
            None
        };

        let palette_data = Self::build_palette_buffer();
        let palette_buffer = unsafe {
            device
                .newBufferWithBytes_length_options(
                    NonNull::new(palette_data.as_ptr() as *mut c_void).unwrap(),
                    (256 * 4 * mem::size_of::<u16>()) as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .expect("failed to create palette buffer")
        };

        Self {
            device,
            command_queue,
            pipeline,
            tiled_pipeline,
            tiled_list_pipeline,
            scroll_pipeline,
            palette_buffer,
        }
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    pub fn command_queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.command_queue
    }

    #[allow(dead_code)]
    pub fn pipeline(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        &self.pipeline
    }

    #[allow(dead_code)]
    pub fn tiled_pipeline(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        self.tiled_pipeline
            .as_ref()
            .expect("tiled pipeline was not requested")
    }

    #[allow(dead_code)]
    pub fn tiled_list_pipeline(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        self.tiled_list_pipeline
            .as_ref()
            .expect("tiled-list pipeline was not requested")
    }

    #[allow(dead_code)]
    pub fn scroll_pipeline(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        &self.scroll_pipeline
    }

    pub fn palette_buffer(&self) -> &ProtocolObject<dyn MTLBuffer> {
        &self.palette_buffer
    }

    fn f32_to_f16(val: f32) -> u16 {
        let bits = val.to_bits();
        let sign = (bits >> 16) & 0x8000;
        let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
        let frac = bits & 0x007F_FFFF;
        if exp <= 0 {
            0
        } else if exp >= 31 {
            (sign | 0x7C00) as u16
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
}

impl Default for MetalCore {
    fn default() -> Self {
        Self::new()
    }
}
