use rand::RngExt;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use wayland_client::QueueHandle;
use wgpu::{
    BindGroupLayout, Buffer, CommandEncoderDescriptor, CompositeAlphaMode, Device, PresentMode,
    Queue, RenderPassDescriptor, RenderPipeline, Sampler, Surface, SurfaceConfiguration,
    TextureFormat, TextureUsages,
};

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Clone, Copy)]
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
    layer: LayerSurface,
    surface: wgpu::Surface<'static>,
    width: u32,
    height: u32,

    state_a: wgpu::Texture,
    state_a_view: wgpu::TextureView,
    state_a_bind: wgpu::BindGroup,
    state_b: wgpu::Texture,
    state_b_view: wgpu::TextureView,
    state_b_bind: wgpu::BindGroup,

    read_state: ReadState,
}

impl Wallpaper {
    pub fn create(
        width: u32,
        shrink_h: u32,
        height: u32,
        shrink_v: u32,
        device: &Device,
        queue: &Queue,
        bind_group_layout: &BindGroupLayout,
        uniform_buf: &Buffer,
        state_sampler: &Sampler,
        layer: LayerSurface,
        surface: Surface<'static>,
    ) -> Self {
        let (state_a, state_a_view, state_a_bind, state_b, state_b_view, state_b_bind) =
            state_stuff(
                width,
                shrink_h,
                height,
                shrink_v,
                device,
                queue,
                bind_group_layout,
                uniform_buf,
                state_sampler,
            );

        Wallpaper {
            layer: layer,
            width: width,
            height: height,
            surface,
            state_a,
            state_a_view,
            state_a_bind,
            state_b,
            state_b_view,
            state_b_bind,
            read_state: ReadState::StateA,
        }
    }

    pub fn reconfigure(
        &mut self,
        surface_format: TextureFormat,
        present_mode: PresentMode,
        alpha_mode: CompositeAlphaMode,
        device: &Device,
        bind_group_layout: &BindGroupLayout,
        uniform_buf: &Buffer,
        state_sampler: &Sampler,
        shrink_h: u32,
        shrink_v: u32,
        queue: &Queue,
    ) {
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: self.width,
            height: self.height,
            present_mode: present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: alpha_mode,
            view_formats: vec![],
        };

        self.surface.configure(device, &config);

        let (state_a, state_a_view, state_a_bind, state_b, state_b_view, state_b_bind) =
            state_stuff(
                self.width,
                shrink_h,
                self.height,
                shrink_v,
                device,
                queue,
                bind_group_layout,
                uniform_buf,
                state_sampler,
            );

        self.state_a = state_a;
        self.state_a_view = state_a_view;
        self.state_a_bind = state_a_bind;
        self.state_b = state_b;
        self.state_b_view = state_b_view;
        self.state_b_bind = state_b_bind;
    }

    pub fn draw(
        &mut self,
        device: &Device,
        state_pipeline: &RenderPipeline,
        display_pipeline: &RenderPipeline,
        qh: &Queue,
    ) -> bool {
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

        let write_view = match self.read_state {
            ReadState::StateA => &self.state_b_view,
            ReadState::StateB => &self.state_a_view,
        };

        let (read_group, write_group) = match self.read_state {
            ReadState::StateA => (&self.state_a_bind, &self.state_b_bind),
            ReadState::StateB => (&self.state_b_bind, &self.state_b_bind),
        };

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("main encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
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
            pass.set_bind_group(0, read_group, &[]);
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

            pass.set_pipeline(&display_pipeline);
            pass.set_bind_group(0, write_group, &[]);
            pass.draw(0..3, 0..1);
        }

        qh.submit([encoder.finish()]);
        frame.present();

        self.read_state = self.read_state.swap();
        return needs_reconfigure;
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
fn state_stuff(
    width: u32,
    shrink_h: u32,
    height: u32,
    shrink_v: u32,
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
    let state_width = width / shrink_h;
    let state_height = height / shrink_v;

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
