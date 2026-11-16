//! The GPU path of the ambient occlusion bake ([`crate::ao`]): plain
//! rasterisation, no ray-tracing hardware. For each direction over the
//! sphere the occluders are drawn into an orthographic depth map looking up
//! that direction from below, peeled into [`LAYERS`] layers (the lowest
//! surfaces first), and a compute pass asks each texel whether any layer lies
//! within `distance` above it. Each texel sums the cosine weight of the
//! directions that escape, which is what the CPU tracer estimates with rays.
//!
//! A texel with more than [`LAYERS`] surfaces under it along a direction
//! (deep stacks of floors) misses what's above them; for typical exports
//! (a ground, a floor slab, a storey or two) the layers hold all of it.

use bevy::math::Vec3;
use wgpu::util::DeviceExt;

/// Depth layers peeled per direction.
pub const LAYERS: usize = 8;
/// Depth map size per direction.
const MAP: u32 = 2048;
/// Texels per compute chunk (each chunk is its own buffers).
const CHUNK: usize = 1 << 21;
/// Uniform stride (dynamic offsets are 256-aligned).
const STRIDE: u64 = 256;

const SHADER: &str = r#"
struct View {
    centre: vec4<f32>,  // xyz, w = radius
    right: vec4<f32>,
    up: vec4<f32>,
    dir: vec4<f32>,     // xyz, w = 1 on the first peel
    params: vec4<f32>,  // distance, bias, depth texel (world), texel count
};
@group(0) @binding(0) var<uniform> v: View;

fn project(p: vec3<f32>) -> vec3<f32> {
    let q = p - v.centre.xyz;
    let r = v.centre.w;
    return vec3<f32>(dot(q, v.right.xyz) / r, dot(q, v.up.xyz) / r, (dot(q, v.dir.xyz) + r) / (2.0 * r));
}

// ---- peel ----
@group(0) @binding(1) var prev: texture_2d<f32>;

@vertex
fn vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    let s = project(p);
    return vec4<f32>(s.x, s.y, s.z, 1.0);
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    if v.dir.w < 0.5 {
        let before = textureLoad(prev, vec2<i32>(at.xy), 0).r;
        if at.z <= before + 1e-7 {
            discard;
        }
    }
    return vec4<f32>(at.z, 0.0, 0.0, 0.0);
}

// ---- accumulate ----
struct Texel { pos: vec4<f32>, nrm: vec4<f32> };
@group(1) @binding(0) var<storage, read> texels: array<Texel>;
@group(1) @binding(1) var<storage, read_write> acc: array<vec2<f32>>;
@group(1) @binding(2) var<uniform> count: vec4<u32>;
@group(2) @binding(0) var l0: texture_2d<f32>;
@group(2) @binding(1) var l1: texture_2d<f32>;
@group(2) @binding(2) var l2: texture_2d<f32>;
@group(2) @binding(3) var l3: texture_2d<f32>;
@group(2) @binding(4) var l4: texture_2d<f32>;
@group(2) @binding(5) var l5: texture_2d<f32>;
@group(2) @binding(6) var l6: texture_2d<f32>;
@group(2) @binding(7) var l7: texture_2d<f32>;

fn layer(k: u32, c: vec2<i32>) -> f32 {
    switch k {
        case 0u: { return textureLoad(l0, c, 0).r; }
        case 1u: { return textureLoad(l1, c, 0).r; }
        case 2u: { return textureLoad(l2, c, 0).r; }
        case 3u: { return textureLoad(l3, c, 0).r; }
        case 4u: { return textureLoad(l4, c, 0).r; }
        case 5u: { return textureLoad(l5, c, 0).r; }
        case 6u: { return textureLoad(l6, c, 0).r; }
        default: { return textureLoad(l7, c, 0).r; }
    }
}

@compute @workgroup_size(64)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x + id.y * 65535u * 64u;
    if i >= count.x {
        return;
    }
    let t = texels[i];
    let n = t.nrm.xyz;
    let w = dot(n, v.dir.xyz);
    if w <= 0.0 {
        return;
    }
    let r2 = 2.0 * v.centre.w;
    let dist = v.params.x;
    let pix = v.params.z;
    // Lift off the surface: the CPU bias, plus the depth texel's slope.
    let slope = sqrt(max(1.0 - w * w, 0.0)) / max(w, 0.1);
    let lift = v.params.y + pix * (0.75 * slope + 0.5);
    let s = project(t.pos.xyz);
    let size = f32(textureDimensions(l0).x);
    let c = vec2<i32>(i32((s.x * 0.5 + 0.5) * size), i32((0.5 - s.y * 0.5) * size));
    let lo = s.z + lift / r2;
    let hi = s.z + dist / r2;
    var open = 1.0;
    for (var k = 0u; k < 8u; k++) {
        let z = layer(k, c);
        if z >= 1.0 || z > hi {
            break;
        }
        if z > lo {
            open = 0.0;
            break;
        }
    }
    acc[i] = acc[i] + vec2<f32>(w * open, w);
}
"#;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// Evenly spread directions over the whole sphere.
pub fn sphere(n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = i as f32 * golden;
            Vec3::new(r * a.cos(), y, r * a.sin())
        })
        .collect()
}

/// The open fraction of each texel `(position, normal)` against the
/// occluder triangles, or `None` without a usable GPU (then the caller
/// traces on the CPU). `samples` is rays per hemisphere, as on the CPU.
pub fn occlusion(
    tris: &[[Vec3; 3]],
    texels: &[(Vec3, Vec3)],
    samples: usize,
    distance: f32,
    bias: f32,
) -> Option<Vec<f32>> {
    if texels.is_empty() {
        return Some(Vec::new());
    }
    if std::env::var("MODELER_AO").is_ok_and(|v| v == "cpu") {
        return None;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ao"),
        ..Default::default()
    }))
    .ok()?;
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    {
        let errors = errors.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |e| {
            if let Ok(mut v) = errors.lock() {
                v.push(e.to_string());
            }
        }));
    }

    // Bounds of everything, so every direction's map covers the scene.
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in tris.iter().flatten().chain(texels.iter().map(|t| &t.0)) {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let centre = (lo + hi) * 0.5;
    let radius = ((hi - lo).length() * 0.5).max(1e-3) + distance;
    let pix = 2.0 * radius / MAP as f32;

    let verts: Vec<f32> = tris.iter().flatten().flat_map(|p| p.to_array()).collect();
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ao verts"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    // One uniform entry per (direction, layer).
    let dirs = sphere(samples * 2);
    let mut uni = vec![0u8; dirs.len() * LAYERS * STRIDE as usize];
    for (di, d) in dirs.iter().enumerate() {
        let (right, up) = d.any_orthonormal_pair();
        for k in 0..LAYERS {
            let f: [f32; 20] = [
                centre.x,
                centre.y,
                centre.z,
                radius,
                right.x,
                right.y,
                right.z,
                0.0,
                up.x,
                up.y,
                up.z,
                0.0,
                d.x,
                d.y,
                d.z,
                if k == 0 { 1.0 } else { 0.0 },
                distance,
                bias,
                pix,
                0.0,
            ];
            let at = (di * LAYERS + k) * STRIDE as usize;
            uni[at..at + 80].copy_from_slice(bytemuck::cast_slice(&f));
        }
    }
    let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ao views"),
        contents: &uni,
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ao"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let tex_entry = |binding, vis| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let view_entry = |vis| wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(80),
        },
        count: None,
    };
    let peel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ao peel"),
        entries: &[
            view_entry(wgpu::ShaderStages::VERTEX_FRAGMENT),
            tex_entry(1, wgpu::ShaderStages::FRAGMENT),
        ],
    });
    let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ao view"),
        entries: &[view_entry(wgpu::ShaderStages::COMPUTE)],
    });
    let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let texel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ao texels"),
        entries: &[
            storage(0, true),
            storage(1, false),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let layers_entries: Vec<_> = (0..LAYERS as u32)
        .map(|b| tex_entry(b, wgpu::ShaderStages::COMPUTE))
        .collect();
    let layers_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ao layers"),
        entries: &layers_entries,
    });

    let peel = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ao peel"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&peel_layout)],
                immediate_size: 0,
            }),
        ),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3],
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::TextureFormat::R32Float.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let acc_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("ao accumulate"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[
                    Some(&view_layout),
                    Some(&texel_layout),
                    Some(&layers_layout),
                ],
                immediate_size: 0,
            }),
        ),
        module: &module,
        entry_point: Some("accumulate"),
        compilation_options: Default::default(),
        cache: None,
    });

    let tex = |label, fmt, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: MAP,
                height: MAP,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        })
    };
    let depth = tex(
        "ao depth",
        wgpu::TextureFormat::Depth32Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let depth_view = depth.create_view(&Default::default());
    let layers: Vec<wgpu::TextureView> = (0..LAYERS)
        .map(|_| {
            tex(
                "ao layer",
                wgpu::TextureFormat::R32Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
            .create_view(&Default::default())
        })
        .collect();
    let peel_groups: Vec<wgpu::BindGroup> = (0..LAYERS)
        .map(|k| {
            // The first layer reads the last one, ignored (dir.w = 1).
            let prev = &layers[(k + LAYERS - 1) % LAYERS];
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &peel_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &ubuf,
                            offset: 0,
                            size: wgpu::BufferSize::new(80),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(prev),
                    },
                ],
            })
        })
        .collect();
    let view_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &view_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &ubuf,
                offset: 0,
                size: wgpu::BufferSize::new(80),
            }),
        }],
    });
    let layers_entries: Vec<_> = layers
        .iter()
        .enumerate()
        .map(|(b, v)| wgpu::BindGroupEntry {
            binding: b as u32,
            resource: wgpu::BindingResource::TextureView(v),
        })
        .collect();
    let layers_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layers_layout,
        entries: &layers_entries,
    });

    // Texels in chunks.
    struct Chunk {
        len: usize,
        acc: wgpu::Buffer,
        group: wgpu::BindGroup,
    }
    let chunks: Vec<Chunk> = texels
        .chunks(CHUNK)
        .map(|c| {
            let data: Vec<f32> = c
                .iter()
                .flat_map(|(p, n)| [p.x, p.y, p.z, 0.0, n.x, n.y, n.z, 0.0])
                .collect();
            let tbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ao texels"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let acc = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ao acc"),
                size: (c.len() * 8) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let cbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ao count"),
                contents: bytemuck::cast_slice(&[c.len() as u32, 0, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &texel_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: tbuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: acc.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: cbuf.as_entire_binding(),
                    },
                ],
            });
            Chunk {
                len: c.len(),
                acc,
                group,
            }
        })
        .collect();

    let vcount = (tris.len() * 3) as u32;
    for di in 0..dirs.len() {
        let mut enc = device.create_command_encoder(&Default::default());
        for (k, layer) in layers.iter().enumerate() {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: layer,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&peel);
            pass.set_bind_group(
                0,
                &peel_groups[k],
                &[((di * LAYERS + k) as u64 * STRIDE) as u32],
            );
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vcount, 0..1);
        }
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&acc_pipe);
            pass.set_bind_group(0, &view_group, &[((di * LAYERS) as u64 * STRIDE) as u32]);
            pass.set_bind_group(2, &layers_group, &[]);
            for c in &chunks {
                pass.set_bind_group(1, &c.group, &[]);
                let groups = c.len.div_ceil(64) as u32;
                pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
            }
        }
        queue.submit([enc.finish()]);
        // Keep the queue short so the OS never sees a hung GPU.
        if di % 16 == 15 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
    }

    let mut out = Vec::with_capacity(texels.len());
    for c in &chunks {
        let size = (c.len * 8) as u64;
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(&c.acc, 0, &read, 0, size);
        queue.submit([enc.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        let view = read.slice(..).get_mapped_range().ok()?;
        let acc: &[f32] = bytemuck::cast_slice(&view);
        out.extend(
            acc.as_chunks::<2>()
                .0
                .iter()
                .map(|a| if a[1] > 0.0 { a[0] / a[1] } else { 1.0 }),
        );
    }
    let errs = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errs.is_empty() {
        eprintln!(
            "ao: GPU bake failed, tracing on the CPU: {}",
            errs.join("; ")
        );
        return None;
    }
    Some(out)
}
