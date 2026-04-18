use rand::RngExt;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use std::{
    fs::{self, read_to_string},
    num::NonZeroU32,
    ptr::NonNull,
    sync::mpsc::{self, Receiver},
    time::Instant,
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_surface},
    Connection, Proxy, QueueHandle,
};

use crate::{
    config::Config, ipc::socket_path, uniform::UniformValue, AppCommand, Params, UniformState,
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

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub fn run_renderer(rx: Receiver<AppCommand>, config: Config) {
    let conn = Connection::connect_to_env().expect("failed to connect to Wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("failed to init registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor unavailable");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell unavailable");

    let wl_surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        wl_surface,
        Layer::Background,
        Some("wgpu-layer"),
        None,
    );

    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(-1);
    layer.set_size(0, 0);
    layer.commit();

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });

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
            .expect("failed to create wgpu surface")
    };

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("failed to get adapter");

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .expect("failed to request device");

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

    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params uniform"),
        size: std::mem::size_of::<Params>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let state_shader =
        read_to_string(config.get_state_path()).expect("failed to read state shader");
    let display_shader =
        read_to_string(config.get_display_path()).expect("failed to read display shader");

    let bind_group_layout = create_state_bind_group_layout(&device);
    let pipeline_layout = create_pipeline_layout(&device, &bind_group_layout);
    let state_sampler = create_state_sampler(&device);

    let disp_width = 1920;
    let disp_height = 1080;
    let state_width = disp_width / config.state_shrink_h;
    let state_height = disp_height / config.state_shrink_v;

    let (state_a, state_a_view) =
        create_state_texture(&device, state_width, state_height, "state a");
    let (state_b, state_b_view) =
        create_state_texture(&device, state_width, state_height, "state b");

    randomize_state_texture(&queue, &state_a, state_width, state_height);
    randomize_state_texture(&queue, &state_b, state_width, state_height);

    let state_pipeline = create_render_pipeline(
        &device,
        STATE_FORMAT,
        &pipeline_layout,
        &state_shader,
        "state pipeline",
    );

    let display_pipeline = create_render_pipeline(
        &device,
        surface_format,
        &pipeline_layout,
        &display_shader,
        "display pipeline",
    );

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),

        layer,
        exit: false,
        configured: false,
        needs_reconfigure: true,
        width: disp_width,
        height: disp_height,
        state_shrink_h: config.state_shrink_h,
        state_shrink_v: config.state_shrink_v,

        surface,
        device,
        queue,
        surface_format,
        present_mode,
        alpha_mode: caps.alpha_modes[0],

        bind_group_layout,
        pipeline_layout,

        state_pipeline,
        display_pipeline,

        uniform_buf,
        state_sampler,

        state_a,
        state_a_view,
        state_b,
        state_b_view,

        read_state: ReadState::StateA,
        start: Instant::now(),

        uniforms: UniformState {
            c1: config.c1.into(),
            c2: config.c2.into(),
            c3: config.c3.into(),
            c4: config.c4.into(),
            ..Default::default()
        },
        state_shader_source: state_shader,
        display_shader_source: display_shader,
        command_rx: rx,
    };

    while !app.exit {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("Wayland dispatch failed");
    }

    let _ = fs::remove_file(socket_path());
    drop(app.surface);
}

enum ReadState {
    StateA,
    StateB,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,

    layer: LayerSurface,
    exit: bool,
    configured: bool,
    needs_reconfigure: bool,
    width: u32,
    height: u32,
    state_shrink_h: u32,
    state_shrink_v: u32,

    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,

    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,

    state_pipeline: wgpu::RenderPipeline,
    display_pipeline: wgpu::RenderPipeline,

    uniform_buf: wgpu::Buffer,
    state_sampler: wgpu::Sampler,

    state_a: wgpu::Texture,
    state_a_view: wgpu::TextureView,
    state_b: wgpu::Texture,
    state_b_view: wgpu::TextureView,

    read_state: ReadState,
    start: Instant,

    uniforms: UniformState,
    state_shader_source: String,
    display_shader_source: String,
    command_rx: mpsc::Receiver<AppCommand>,
}

impl App {
    fn reconfigure(&mut self) {
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

        self.surface.configure(&self.device, &config);

        let state_width = self.width / self.state_shrink_h;
        let state_height = self.height / self.state_shrink_v;

        let (state_a, state_a_view) =
            create_state_texture(&self.device, state_width, state_height, "state a");
        let (state_b, state_b_view) =
            create_state_texture(&self.device, state_width, state_height, "state b");

        randomize_state_texture(&self.queue, &state_a, state_width, state_height);
        randomize_state_texture(&self.queue, &state_b, state_width, state_height);

        self.state_a = state_a;
        self.state_a_view = state_a_view;
        self.state_b = state_b;
        self.state_b_view = state_b_view;
        self.read_state = ReadState::StateA;
    }

    fn handle_pending_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                AppCommand::Set { name, value } => {
                    if let Err(msg) = self.apply_uniform_update(&name, value) {
                        eprintln!("uniform update failed: {msg}");
                    }
                }
                AppCommand::Shader { fragment_glsl } => {
                    self.display_shader_source = fragment_glsl;
                    self.display_pipeline = create_render_pipeline(
                        &self.device,
                        self.surface_format,
                        &self.pipeline_layout,
                        &self.display_shader_source,
                        "display shader",
                    );
                }
            }
        }
    }

    fn apply_uniform_update(&mut self, name: &str, value: UniformValue) -> Result<(), String> {
        match (name, value) {
            ("c1", UniformValue::ColorRgb([r, g, b])) => {
                self.uniforms.c1 = [r, g, b, 1.0];
                Ok(())
            }
            ("c2", UniformValue::ColorRgb([r, g, b])) => {
                self.uniforms.c2 = [r, g, b, 1.0];
                Ok(())
            }
            ("c3", UniformValue::ColorRgb([r, g, b])) => {
                self.uniforms.c3 = [r, g, b, 1.0];
                Ok(())
            }
            ("c4", UniformValue::ColorRgb([r, g, b])) => {
                self.uniforms.c4 = [r, g, b, 1.0];
                Ok(())
            }
            ("time_scale", UniformValue::Float(v)) => {
                self.uniforms.time_scale = v;
                Ok(())
            }
            _ => Err(format!("unsupported assignment: {name}")),
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured {
            return;
        }

        self.handle_pending_commands();

        let params = Params {
            resolution: [self.width as f32, self.height as f32],
            time: self.start.elapsed().as_secs_f32(),
            time_scale: self.uniforms.time_scale,
            c1: self.uniforms.c1,
            c2: self.uniforms.c2,
            c3: self.uniforms.c3,
            c4: self.uniforms.c4,
        };

        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&params));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.needs_reconfigure = true;
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.reconfigure();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.reconfigure();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("surface validation error");
                return;
            }
        };

        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let (read_view, write_view) = match self.read_state {
            ReadState::StateA => (&self.state_a_view, &self.state_b_view),
            ReadState::StateB => (&self.state_b_view, &self.state_a_view),
        };

        let read_bind_group = create_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.uniform_buf,
            read_view,
            &self.state_sampler,
            "read_bind_group",
        );

        let write_bind_group = create_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.uniform_buf,
            write_view,
            &self.state_sampler,
            "write_bind_group",
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
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

            pass.set_pipeline(&self.state_pipeline);
            pass.set_bind_group(0, &read_bind_group, &[]);
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
            pass.set_bind_group(0, &write_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit([encoder.finish()]);
        frame.present();

        self.read_state = match self.read_state {
            ReadState::StateA => ReadState::StateB,
            ReadState::StateB => ReadState::StateA,
        };

        if self.needs_reconfigure {
            self.reconfigure();
            self.needs_reconfigure = false;
        }

        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
    }
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

fn create_state_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("state sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

fn create_state_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("state and display bind group layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
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

fn create_pipeline_layout(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    })
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

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if surface == self.layer.wl_surface() {
            self.draw(qh);
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.width = NonZeroU32::new(configure.new_size.0).map_or(256, NonZeroU32::get);
        self.height = NonZeroU32::new(configure.new_size.1).map_or(256, NonZeroU32::get);

        self.reconfigure();
        self.configured = true;
        self.needs_reconfigure = false;

        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());

        self.draw(qh);
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
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

delegate_compositor!(App);
delegate_output!(App);
delegate_layer!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}
