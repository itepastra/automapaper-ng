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
    f32::consts::LN_2,
    fs::{self, read_to_string},
    num::NonZeroU32,
    ptr::NonNull,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_surface},
    Connection, Proxy, QueueHandle,
};
use wgpu::{
    Adapter, BackendOptions, Backends, BufferDescriptor, BufferUsages, DeviceDescriptor,
    ExperimentalFeatures, Features, Instance, InstanceDescriptor, InstanceFlags, Limits,
    MemoryBudgetThresholds, RequestAdapterOptions, Trace,
};

use crate::{
    config::Config,
    ipc::socket_path,
    uniform::ColorValue,
    wallpaper::{self, create_state_pipeline, Wallpaper},
    AppCommand, Params, UniformState,
};

pub const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub fn run_renderer(rx: Receiver<AppCommand>, config: Config) {
    let conn = Connection::connect_to_env().expect("failed to connect to Wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("failed to init registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor unavailable");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer_shell unavailable");

    let instance = Instance::new(InstanceDescriptor {
        backends: Backends::all(),
        flags: InstanceFlags::default(),
        memory_budget_thresholds: MemoryBudgetThresholds::default(),
        backend_options: BackendOptions::default(),
        display: None,
    });

    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        compatible_surface: None,
        ..Default::default()
    }))
    .expect("Failed to get adapter");

    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        label: Some("device"),
        required_features: Features::empty(),
        required_limits: Limits::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: Trace::Off,
        experimental_features: ExperimentalFeatures::disabled(),
    }))
    .expect("failed to request device");

    let uniform_buf = device.create_buffer(&BufferDescriptor {
        label: Some("param uniforms"),
        size: std::mem::size_of::<Params>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let state_shader =
        read_to_string(config.get_state_path()).expect("failed to read state shader");
    let display_shader =
        read_to_string(config.get_display_path()).expect("failed to read display shader");

    let bind_group_layout = create_state_bind_group_layout(&device);
    let pipeline_layout = create_pipeline_layout(&device, &bind_group_layout);
    let state_sampler = create_state_sampler(&device);

    let state_pipeline = create_state_pipeline(&device, &pipeline_layout, &state_shader);

    let current_uniforms = UniformState {
        c1: config.c1.into(),
        c2: config.c2.into(),
        c3: config.c3.into(),
        c4: config.c4.into(),
        ..Default::default()
    };

    let target_uniforms = current_uniforms.clone();

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),

        conn,
        compositor,
        layer_shell,
        instance,

        device,
        queue,

        bind_group_layout,
        pipeline_layout,
        state_pipeline,
        uniform_buf,
        state_sampler,

        shrink_horizontal: config.state_shrink_h,
        shrink_vertical: config.state_shrink_v,
        decay_time: config.decay_time,
        frame_time: Duration::from_secs_f32(config.frame_time),
        current_uniforms,
        target_uniforms,
        state_shader_source: state_shader,
        display_shader_source: display_shader,
        command_rx: rx,
        start: Instant::now(),
        last_uniform_update: Instant::now(),
        wallpapers: Vec::new(),
        exit: false,
    };

    while !app.exit {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("Wayland dispatch failed");
    }

    let _ = fs::remove_file(socket_path());
}

enum ReadState {
    StateA,
    StateB,
}

pub struct App {
    // Shared
    registry_state: RegistryState,
    output_state: OutputState,

    conn: Connection,
    compositor: CompositorState,
    layer_shell: LayerShell,
    instance: Instance,

    device: wgpu::Device,
    queue: wgpu::Queue,

    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    state_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    state_sampler: wgpu::Sampler,

    shrink_horizontal: u32,
    shrink_vertical: u32,
    decay_time: f32,
    frame_time: Duration,
    current_uniforms: UniformState,
    target_uniforms: UniformState,
    state_shader_source: String,
    display_shader_source: String,
    command_rx: mpsc::Receiver<AppCommand>,
    start: Instant,
    last_uniform_update: Instant,

    wallpapers: Vec<Wallpaper>,
    exit: bool,
}

impl App {
    fn update_uniforms(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_uniform_update).as_secs_f32();
        self.last_uniform_update = now;

        let alpha = if self.decay_time <= 0.0 {
            1.0
        } else {
            1.0 - 0.5_f32.powf(dt / self.decay_time)
        };

        self.current_uniforms = self.current_uniforms.mix(&self.target_uniforms, alpha);
    }

    fn make_params(&self) -> Params {
        Params {
            resolution: [1., 1.],
            time: self.start.elapsed().as_secs_f32(),
            time_scale: self.current_uniforms.time_scale,
            c1: self.current_uniforms.c1,
            c2: self.current_uniforms.c2,
            c3: self.current_uniforms.c3,
            c4: self.current_uniforms.c4,
        }
    }

    fn apply_color_update(&mut self, name: &str, value: ColorValue) -> Result<(), String> {
        match name {
            "c1" => self.target_uniforms.c1 = value.into(),
            "c2" => self.target_uniforms.c2 = value.into(),
            "c3" => self.target_uniforms.c3 = value.into(),
            "c4" => self.target_uniforms.c4 = value.into(),

            _ => return Err(format!("unsupported assignment: {name}")),
        }
        Ok(())
    }

    fn handle_pending_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                AppCommand::Set { name, value } => {
                    if let Err(msg) = self.apply_color_update(&name, value) {
                        eprintln!("uniform update failed: {msg}")
                    }
                }
                AppCommand::Stop => self.exit = true,
                AppCommand::DisplayShader { fragment_glsl } => todo!(),
                AppCommand::StateShader { fragment_glsl } => todo!(),
            }
        }
    }

    fn wallpaper_index_by_surface(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.wallpapers
            .iter()
            .position(|w| w.wl_surface() == surface)
    }

    fn wallpaper_index_by_layer(&self, layer: &LayerSurface) -> Option<usize> {
        self.wallpapers.iter().position(|w| w.layer() == layer)
    }

    fn create_wallpaper_for_output(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.wallpapers.iter().any(|w| w.output() == &output) {
            return;
        }

        let wallpaper = Wallpaper::new(
            &self.conn,
            qh,
            &self.instance,
            &self.device,
            &self.queue,
            &self.compositor,
            &self.layer_shell,
            output,
            &self.bind_group_layout,
            &self.pipeline_layout,
            &self.uniform_buf,
            &self.state_sampler,
            &self.state_pipeline,
            &self.display_shader_source,
            self.shrink_horizontal,
            self.shrink_vertical,
        );

        self.wallpapers.push(wallpaper);
    }
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
        self.handle_pending_commands();
        self.update_uniforms();

        let params = self.make_params();

        if let Some(i) = self.wallpaper_index_by_surface(surface) {
            let wallpaper = &mut self.wallpapers[i];

            let needs_reconfigure = wallpaper.draw(
                &self.device,
                &self.queue,
                &self.bind_group_layout,
                &self.uniform_buf,
                &self.state_sampler,
                &self.state_pipeline,
                &params,
                self.frame_time,
            );

            if needs_reconfigure {
                wallpaper.reconfigure(
                    &self.device,
                    &self.queue,
                    &self.bind_group_layout,
                    &self.uniform_buf,
                    &self.state_sampler,
                );
            }

            wallpaper.request_frame(qh);
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
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.handle_pending_commands();
        let params = &self.make_params();

        if let Some(i) = self.wallpaper_index_by_layer(layer) {
            let wallpaper = &mut self.wallpapers[i];

            wallpaper.set_size(
                NonZeroU32::new(configure.new_size.0).map_or(256, NonZeroU32::get),
                NonZeroU32::new(configure.new_size.1).map_or(256, NonZeroU32::get),
            );

            wallpaper.reconfigure(
                &self.device,
                &self.queue,
                &self.bind_group_layout,
                &self.uniform_buf,
                &self.state_sampler,
            );

            wallpaper.request_frame(qh);

            let needs_reconfigure = wallpaper.draw(
                &self.device,
                &self.queue,
                &self.bind_group_layout,
                &self.uniform_buf,
                &self.state_sampler,
                &self.state_pipeline,
                &params,
                self.frame_time,
            );

            if needs_reconfigure {
                wallpaper.reconfigure(
                    &self.device,
                    &self.queue,
                    &self.bind_group_layout,
                    &self.uniform_buf,
                    &self.state_sampler,
                );
            }
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.create_wallpaper_for_output(qh, output);
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
        output: wl_output::WlOutput,
    ) {
        self.wallpapers.retain(|w| w.output() != &output);
    }
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
