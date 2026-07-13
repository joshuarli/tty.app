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
    pub(crate) pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    #[allow(dead_code)]
    pub(crate) instanced_pipeline: Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
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

    /// Create Metal resources including the headless instanced-cell pipeline.
    #[allow(dead_code)]
    pub fn new_with_instanced() -> Self {
        Self::new_internal(true)
    }

    fn new_internal(with_instanced: bool) -> Self {
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
        let instanced_pipeline = if with_instanced {
            let vertex_function = library
                .newFunctionWithName(&NSString::from_str("render_instanced_vertex"))
                .expect("shader function 'render_instanced_vertex' not found");
            let fragment_function = library
                .newFunctionWithName(&NSString::from_str("render_instanced_fragment"))
                .expect("shader function 'render_instanced_fragment' not found");
            let render_descriptor = MTLRenderPipelineDescriptor::new();
            render_descriptor.setVertexFunction(Some(&vertex_function));
            render_descriptor.setFragmentFunction(Some(&fragment_function));
            let color_attachment = unsafe {
                render_descriptor
                    .colorAttachments()
                    .objectAtIndexedSubscript(0)
            };
            color_attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            Some(
                device
                    .newRenderPipelineStateWithDescriptor_error(&render_descriptor)
                    .expect("failed to create instanced render pipeline"),
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
            instanced_pipeline,
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

    pub fn pipeline(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        &self.pipeline
    }

    #[allow(dead_code)]
    pub fn instanced_pipeline(&self) -> &ProtocolObject<dyn MTLRenderPipelineState> {
        self.instanced_pipeline
            .as_ref()
            .expect("instanced pipeline was not requested")
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
