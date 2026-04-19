use rand::RngExt;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::CompositorState,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
        WaylandSurface,
    },
};
use std::ptr::NonNull;
use wayland_client::{
    protocol::{wl_output, wl_surface},
    Connection, Proxy, QueueHandle,
};
use wgpu::{BindGroupLayout, Buffer, Device, Queue, RenderPipeline, Sampler};

use crate::{
    renderer::{App, STATE_FORMAT},
    Params,
};

const VERT_GLSL: &str = r#"
#version 450

layout(location = 0) out vec2 v_uv;

vec2 positions[3] = vec2[](
    vec2(-1.0, -3.0),
    vec2(-1.0,  1.0),
    vec2( 3.0,  1.0)
);

void main() {
    vec2 p = positions[gl_VertexIndex];
    v_uv = 0.5 * (p + vec2(1.0));
    gl_Position = vec4(p, 0.0, 1.0);
}
"#;

#[derive(Clone, Copy, Debug)]
enum ReadState {
    StateA,
    StateB,
}

impl ReadState {
    fn swap(self) -> ReadState {
        match self {
            ReadState::StateA => ReadState::StateB,
            ReadState::StateB => ReadState::StateA,
        }
    }
}

pub struct Wallpaper {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    surface: wgpu::Surface<'static>,

    surface_format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,

    width: u32,
    shrink_horizontal: u32,
    height: u32,
    shrink_vertical: u32,

    display_pipeline: wgpu::RenderPipeline,

    state_a: wgpu::Texture,
    state_b: wgpu::Texture,
    state_a_view: wgpu::TextureView,
    state_b_view: wgpu::TextureView,

    state_a_bind_group: wgpu::BindGroup,
    state_b_bind_group: wgpu::BindGroup,

    read_state: ReadState,
}

impl Wallpaper {
    pub fn new(
        conn: &Connection,
        qh: &QueueHandle<App>,
        instance: &wgpu::Instance,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        output: wl_output::WlOutput,
        bind_group_layout: &wgpu::BindGroupLayout,
        pipeline_layout: &wgpu::PipelineLayout,
        uniform_buf: &wgpu::Buffer,
        state_sampler: &wgpu::Sampler,
        _state_pipeline: &wgpu::RenderPipeline,
        display_shader_source: &str,
        shrink_horizontal: u32,
        shrink_vertical: u32,
    ) -> Self {
        // width and height will be changed by (re)configure() anyways, so just choose something here
        let width = 1920;
        let height = 1080;

        // create layer surface
        let wl_surface = compositor.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Background,
            Some("wgpu-layer"),
            Some(&output),
        );

        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.commit();

        let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(conn.backend().display_ptr() as *mut _).unwrap(),
        ));
        let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(layer.wl_surface().id().as_ptr() as *mut _).unwrap(),
        ));

        let surface = unsafe {
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display_handle),
                    raw_window_handle,
                })
                .expect("failed to create wallpaper surface")
        };

        // Query caps after creation. Surface will be configured later in configure().
        // Any adapter compatible with the instance/device stack should work here.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("failed to get surface-compatible adapter");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };

        let alpha_mode = caps.alpha_modes[0];

        let display_pipeline = create_render_pipeline(
            device,
            surface_format,
            pipeline_layout,
            display_shader_source,
            "display pipeline",
        );

        let (state_a, state_a_view, state_a_bind_group, state_b, state_b_view, state_b_bind_group) =
            state_stuff(
                width,
                shrink_horizontal,
                height,
                shrink_vertical,
                device,
                queue,
                bind_group_layout,
                uniform_buf,
                state_sampler,
            );

        Self {
            output,
            layer,
            surface,
            surface_format,
            present_mode,
            alpha_mode,
            display_pipeline,
            width,
            shrink_horizontal,
            height,
            shrink_vertical,
            state_a,
            state_a_view,
            state_b,
            state_b_view,
            state_a_bind_group,
            state_b_bind_group,
            read_state: ReadState::StateA,
        }
    }

    pub fn output(&self) -> &wl_output::WlOutput {
        &self.output
    }
    pub fn layer(&self) -> &LayerSurface {
        &self.layer
    }
    pub fn wl_surface(&self) -> &wl_surface::WlSurface {
        self.layer.wl_surface()
    }
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }
    pub fn set_display_pipeline(&mut self, pipeline: wgpu::RenderPipeline) {
        self.display_pipeline = pipeline;
    }
    pub fn set_size(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    pub fn request_frame(&self, qh: &QueueHandle<App>) {
        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
        self.layer.wl_surface().commit();
    }

    pub fn reconfigure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        uniform_buf: &wgpu::Buffer,
        state_sampler: &wgpu::Sampler,
    ) {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            width: self.width.max(1),
            height: self.height.max(1),
            present_mode: self.present_mode,
            alpha_mode: self.alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        let (state_a, state_a_view, state_a_bind_group, state_b, state_b_view, state_b_bind_group) =
            state_stuff(
                self.width,
                self.shrink_horizontal,
                self.height,
                self.shrink_vertical,
                device,
                queue,
                bind_group_layout,
                uniform_buf,
                state_sampler,
            );
        self.surface.configure(device, &config);

        self.state_a = state_a;
        self.state_a_view = state_a_view;
        self.state_b = state_b;
        self.state_b_view = state_b_view;
        self.state_a_bind_group = state_a_bind_group;
        self.state_b_bind_group = state_b_bind_group;
        self.read_state = ReadState::StateA;
    }

    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bind_group_layout: &wgpu::BindGroupLayout,
        uniform_buf: &wgpu::Buffer,
        _state_sampler: &wgpu::Sampler,
        state_pipeline: &wgpu::RenderPipeline,
        params: &Params,
    ) -> bool {
        let mut params = *params;
        params.resolution = [self.width as f32, self.height as f32];
        queue.write_buffer(uniform_buf, 0, bytemuck::bytes_of(&params));

        let mut needs_reconfigure = false;
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(surface_texture) => surface_texture,
            wgpu::CurrentSurfaceTexture::Suboptimal(surface_texture) => {
                needs_reconfigure = true;
                surface_texture
            }
            wgpu::CurrentSurfaceTexture::Timeout => return false,
            wgpu::CurrentSurfaceTexture::Occluded => return false,
            wgpu::CurrentSurfaceTexture::Outdated => return true,
            wgpu::CurrentSurfaceTexture::Lost => return true,
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("surface validation error");
                return false;
            }
        };

        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let (write_view, read_bind_group, write_bind_group) = match self.read_state {
            ReadState::StateA => (
                &self.state_b_view,
                &self.state_a_bind_group,
                &self.state_b_bind_group,
            ),
            ReadState::StateB => (
                &self.state_a_view,
                &self.state_b_bind_group,
                &self.state_a_bind_group,
            ),
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("main encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("state pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: write_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            pass.set_pipeline(state_pipeline);
            pass.set_bind_group(0, read_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("display pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.display_pipeline);
            pass.set_bind_group(0, write_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        queue.submit([encoder.finish()]);
        frame.present();

        self.read_state = self.read_state.swap();
        needs_reconfigure
    }
}

fn create_render_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    pipeline_layout: &wgpu::PipelineLayout,
    fragment_glsl: &str,
    label: &str,
) -> wgpu::RenderPipeline {
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vertex shader"),
        source: wgpu::ShaderSource::Glsl {
            shader: VERT_GLSL.into(),
            stage: wgpu::naga::ShaderStage::Vertex,
            defines: Default::default(),
        },
    });

    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Glsl {
            shader: fragment_glsl.into(),
            stage: wgpu::naga::ShaderStage::Fragment,
            defines: Default::default(),
        },
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs,
            entry_point: Some("main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs,
            entry_point: Some("main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_state_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STATE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
fn state_stuff(
    width: u32,
    shrink_horizontal: u32,
    height: u32,
    shrink_vertical: u32,
    device: &Device,
    queue: &Queue,
    bind_group_layout: &BindGroupLayout,
    uniform_buf: &Buffer,
    state_sampler: &Sampler,
) -> (
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::BindGroup,
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::BindGroup,
) {
    let state_width = width / shrink_horizontal;
    let state_height = height / shrink_vertical;

    let (state_a, state_a_view) =
        create_state_texture(device, state_width, state_height, "state a");
    let (state_b, state_b_view) =
        create_state_texture(device, state_width, state_height, "state b");

    randomize_state_texture(queue, &state_a, state_width, state_height);
    randomize_state_texture(queue, &state_b, state_width, state_height);

    let state_a_bind = create_bind_group(
        device,
        bind_group_layout,
        uniform_buf,
        &state_a_view,
        state_sampler,
        "state a bind group",
    );

    let state_b_bind = create_bind_group(
        device,
        bind_group_layout,
        uniform_buf,
        &state_b_view,
        state_sampler,
        "state b bind group",
    );

    (
        state_a,
        state_a_view,
        state_a_bind,
        state_b,
        state_b_view,
        state_b_bind,
    )
}

fn randomize_state_texture(queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32) {
    const PIXEL_WIDTH: u32 = 4;
    let mut rng = rand::rng();
    let mut data = vec![0u8; (width * height * PIXEL_WIDTH) as usize];

    for px in data.chunks_exact_mut(PIXEL_WIDTH as usize) {
        px[0] = rng.random();
        px[1] = rng.random();
        px[2] = rng.random();
        px[3] = 255;
    }

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * PIXEL_WIDTH),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform_buf: &wgpu::Buffer,
    state_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(state_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

pub(crate) fn create_state_pipeline(
    device: &Device,
    pipeline_layout: &wgpu::PipelineLayout,
    state_shader: &str,
) -> RenderPipeline {
    create_render_pipeline(
        device,
        STATE_FORMAT,
        pipeline_layout,
        state_shader,
        "state pipeline",
    )
}
