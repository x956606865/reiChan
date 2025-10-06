use std::{
    cmp::{max, min},
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use bytemuck::{bytes_of, cast_slice, Pod, Zeroable};
use wgpu::util::DeviceExt;

const GAMMA_SHADER: &str = r#"
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
};

@group(0) @binding(0)
var<storage, read> input_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read_write> gamma_pixels: array<f32>;

@group(0) @binding(2)
var<storage, read> info: PipelineInfo;

@compute @workgroup_size(256)
fn gamma_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    let total = info.width * info.height;
    if (idx >= total) {
        return;
    }

    let value = clamp(input_pixels[idx], 0.0, 255.0);
    if (abs(info.gamma - 1.0) < 1e-6) {
        gamma_pixels[idx] = value;
        return;
    }

    let normalized = value / 255.0;
    let corrected = pow(normalized, info.inv_gamma) * 255.0;
    gamma_pixels[idx] = clamp(corrected, 0.0, 255.0);
}
"#;

const GAUSSIAN_SHADER: &str = r#"
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
};

@group(0) @binding(0)
var<storage, read> source_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read_write> target_pixels: array<f32>;

@group(0) @binding(2)
var<storage, read> kernel_weights: array<f32>;

@group(0) @binding(3)
var<storage, read> info: PipelineInfo;

fn sample_index(x: i32, y: i32) -> u32 {
    let sx = clamp(x, 0, i32(info.width) - 1);
    let sy = clamp(y, 0, i32(info.height) - 1);
    return u32(sy) * info.width + u32(sx);
}

@compute @workgroup_size(16, 16, 1)
fn gaussian_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= info.width || y >= info.height) {
        return;
    }

    let radius = info.gaussian_kernel / 2u;
    var acc = 0.0;
    var k = 0u;
    loop {
        if (k >= info.gaussian_kernel) {
            break;
        }
        let offset = i32(k) - i32(radius);
        let sample_x = i32(x) + offset;
        let index = sample_index(sample_x, i32(y));
        acc = acc + source_pixels[index] * kernel_weights[k];
        k = k + 1u;
    }
    let idx = y * info.width + x;
    target_pixels[idx] = acc;
}

@compute @workgroup_size(16, 16, 1)
fn gaussian_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= info.width || y >= info.height) {
        return;
    }

    let radius = info.gaussian_kernel / 2u;
    var acc = 0.0;
    var k = 0u;
    loop {
        if (k >= info.gaussian_kernel) {
            break;
        }
        let offset = i32(k) - i32(radius);
        let sample_y = i32(y) + offset;
        let index = sample_index(i32(x), sample_y);
        acc = acc + source_pixels[index] * kernel_weights[k];
        k = k + 1u;
    }
    let idx = y * info.width + x;
    target_pixels[idx] = acc;
}
"#;

const SOBEL_SHADER: &str = r#"
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
};

fn sobel_weight_x(row: u32, col: u32) -> f32 {
    if (row == 0u) {
        if (col == 0u) {
            return -1.0;
        } else if (col == 1u) {
            return 0.0;
        } else {
            return 1.0;
        }
    } else if (row == 1u) {
        if (col == 0u) {
            return -2.0;
        } else if (col == 1u) {
            return 0.0;
        } else {
            return 2.0;
        }
    } else {
        if (col == 0u) {
            return -1.0;
        } else if (col == 1u) {
            return 0.0;
        } else {
            return 1.0;
        }
    }
}

fn sobel_weight_y(row: u32, col: u32) -> f32 {
    if (row == 0u) {
        if (col == 0u) {
            return 1.0;
        } else if (col == 1u) {
            return 2.0;
        } else {
            return 1.0;
        }
    } else if (row == 1u) {
        return 0.0;
    } else {
        if (col == 0u) {
            return -1.0;
        } else if (col == 1u) {
            return -2.0;
        } else {
            return -1.0;
        }
    }
}

@group(0) @binding(0)
var<storage, read> blurred_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read_write> gradient_pixels: array<f32>;

@group(0) @binding(2)
var<storage, read> info: PipelineInfo;

@compute @workgroup_size(16, 16, 1)
fn sobel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= info.width || y >= info.height) {
        return;
    }

    var gx = 0.0;
    var gy = 0.0;
    var ky = 0u;
    loop {
        if (ky >= 3u) {
            break;
        }
        var kx = 0u;
        loop {
            if (kx >= 3u) {
                break;
            }
            let offset_x = clamp(i32(x) + i32(kx) - 1, 0, i32(info.width) - 1);
            let offset_y = clamp(i32(y) + i32(ky) - 1, 0, i32(info.height) - 1);
            let index = u32(offset_y) * info.width + u32(offset_x);
            let value = blurred_pixels[index];
            gx = gx + value * sobel_weight_x(ky, kx);
            gy = gy + value * sobel_weight_y(ky, kx);
            kx = kx + 1u;
        }
        ky = ky + 1u;
    }

    let magnitude = sqrt(gx * gx + gy * gy);
    let idx = y * info.width + x;
    gradient_pixels[idx] = magnitude;
}
"#;

const COLUMN_STATS_SHADER: &str = r#"
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
};

@group(0) @binding(0)
var<storage, read> gamma_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read> gradient_pixels: array<f32>;

@group(0) @binding(2)
var<storage, read> info: PipelineInfo;

struct ColumnStats {
    mean_intensity: f32,
    grad_mean: f32,
    grad_variance: f32,
    padding: f32,
};

@group(0) @binding(3)
var<storage, read_write> column_outputs: array<ColumnStats>;

@compute @workgroup_size(256)
fn column_stats(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    if (x >= info.width) {
        return;
    }

    let height = info.height;
    if (height == 0u) {
        column_outputs[x].mean_intensity = 0.0;
        column_outputs[x].grad_mean = 0.0;
        column_outputs[x].grad_variance = 0.0;
        column_outputs[x].padding = 0.0;
        return;
    }

    var gamma_sum = 0.0;
    var grad_sum = 0.0;
    var grad_sum_sq = 0.0;
    var row = 0u;
    loop {
        if (row >= height) {
            break;
        }
        let idx = row * info.width + x;
        let gamma_val = gamma_pixels[idx];
        let grad_val = gradient_pixels[idx];
        gamma_sum = gamma_sum + gamma_val;
        grad_sum = grad_sum + grad_val;
        grad_sum_sq = grad_sum_sq + grad_val * grad_val;
        row = row + 1u;
    }

    let inv_height = info.inv_height;
    let mean_gamma = clamp(gamma_sum * inv_height, 0.0, 255.0);
    let grad_mean_val = grad_sum * inv_height;
    let variance = max(grad_sum_sq * inv_height - grad_mean_val * grad_mean_val, 0.0);

    column_outputs[x].mean_intensity = mean_gamma;
    column_outputs[x].grad_mean = grad_mean_val;
    column_outputs[x].grad_variance = variance;
    column_outputs[x].padding = 0.0;
}
"#;

const ENTROPY_SHADER: &str = r#"
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
};

const MAX_BINS: u32 = 64u;

@group(0) @binding(0)
var<storage, read> blurred_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read> info: PipelineInfo;

@group(0) @binding(2)
var<storage, read_write> entropy_values: array<f32>;

@compute @workgroup_size(256)
fn entropy_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    if (x >= info.width) {
        return;
    }

    let bins = min(info.entropy_bins, MAX_BINS);
    if (bins < 1u) {
        entropy_values[x] = 0.0;
        return;
    }

    var histogram: array<f32, MAX_BINS>;
    var i = 0u;
    loop {
        if (i >= MAX_BINS) {
            break;
        }
        histogram[i] = 0.0;
        i = i + 1u;
    }

    let window = max(info.entropy_window, 1u);
    let pad = window / 2u;
    let width = info.width;
    let height = info.height;

    let start = select(0u, x - pad, x >= pad);
    let end = min(width, x + pad + 1u);
    let span = end - start;
    let total_pixels = f32(span) * info.height_f;
    if (total_pixels <= 0.0) {
        entropy_values[x] = 0.0;
        return;
    }

    var col = start;
    loop {
        if (col >= end) {
            break;
        }
        var row = 0u;
        loop {
            if (row >= height) {
                break;
            }
            let idx = row * width + col;
            let value = clamp(blurred_pixels[idx], 0.0, 255.0);
            var bin = u32(floor(value / 256.0 * f32(bins)));
            if (bin >= bins) {
                bin = bins - 1u;
            }
            histogram[bin] = histogram[bin] + 1.0;
            row = row + 1u;
        }
        col = col + 1u;
    }

    var entropy = 0.0;
    var b = 0u;
    loop {
        if (b >= bins) {
            break;
        }
        let count = histogram[b];
        if (count > 0.0) {
            let p = count / total_pixels;
            entropy = entropy - p * log2(max(p, 1e-12));
        }
        b = b + 1u;
    }

    entropy_values[x] = entropy;
}
"#;

const DEMO_WIDTH: u32 = 256;
const DEMO_HEIGHT: u32 = 192;
const DEFAULT_GAUSSIAN_KERNEL: u32 = 5;
const DEFAULT_ENTROPY_WINDOW: u32 = 15;
const DEFAULT_ENTROPY_BINS: u32 = 32;
const DEFAULT_GAMMA: f32 = 1.12;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct PipelineInfo {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    reserved: u32,
    gamma: f32,
    inv_gamma: f32,
    inv_height: f32,
    height_f: f32,
}

struct CpuReference {
    gamma: Vec<f32>,
    gaussian: Vec<f32>,
    gradient: Vec<f32>,
    mean_intensity: Vec<f32>,
    grad_mean: Vec<f32>,
    grad_variance: Vec<f32>,
    entropy: Vec<f32>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ColumnStat {
    mean_intensity: f32,
    grad_mean: f32,
    grad_variance: f32,
    padding: f32,
}

struct GpuOutputs {
    gamma: Vec<f32>,
    gaussian: Vec<f32>,
    gradient: Vec<f32>,
    mean_intensity: Vec<f32>,
    grad_mean: Vec<f32>,
    grad_variance: Vec<f32>,
    entropy: Vec<f32>,
}

#[derive(Clone, Copy)]
struct EdgeTexturePipelineConfig {
    width: u32,
    height: u32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
    gamma: f32,
}

impl EdgeTexturePipelineConfig {
    fn new(
        width: u32,
        height: u32,
        gaussian_kernel: u32,
        entropy_window: u32,
        entropy_bins: u32,
        gamma: f32,
    ) -> Self {
        Self {
            width,
            height,
            gaussian_kernel,
            entropy_window,
            entropy_bins,
            gamma,
        }
    }

    fn sanitized_gaussian_kernel(&self) -> u32 {
        ensure_odd(self.gaussian_kernel)
    }

    fn sanitized_entropy_window(&self) -> u32 {
        ensure_odd(self.entropy_window)
    }

    fn sanitized_entropy_bins(&self) -> u32 {
        self.entropy_bins.max(1)
    }

    fn pixel_count(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    fn to_pipeline_info(&self) -> PipelineInfo {
        PipelineInfo {
            width: self.width,
            height: self.height,
            gaussian_kernel: self.sanitized_gaussian_kernel(),
            entropy_window: self.sanitized_entropy_window(),
            entropy_bins: self.sanitized_entropy_bins(),
            reserved: 0,
            gamma: self.gamma,
            inv_gamma: if self.gamma <= 1e-6 {
                1.0
            } else {
                1.0 / self.gamma
            },
            inv_height: if self.height == 0 {
                0.0
            } else {
                1.0 / self.height as f32
            },
            height_f: self.height as f32,
        }
    }
}

struct EdgeTextureGpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl EdgeTextureGpuContext {
    async fn new() -> Result<Self, Box<dyn Error>> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok_or("no compatible GPU adapter found")?;

        let required_limits = wgpu::Limits::downlevel_defaults();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("edge-texture-gpu-poc-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                },
                None,
            )
            .await?;

        Ok(Self { device, queue })
    }

    fn execute(
        &self,
        input: &[f32],
        config: EdgeTexturePipelineConfig,
    ) -> Result<GpuOutputs, Box<dyn Error>> {
        if config.is_empty() {
            return Ok(GpuOutputs {
                gamma: Vec::new(),
                gaussian: Vec::new(),
                gradient: Vec::new(),
                mean_intensity: Vec::new(),
                grad_mean: Vec::new(),
                grad_variance: Vec::new(),
                entropy: Vec::new(),
            });
        }

        let pixel_count = config.pixel_count();
        if input.len() != pixel_count {
            return Err(format!(
                "input length {} does not match width*height {}",
                input.len(),
                pixel_count
            )
            .into());
        }

        let device = &self.device;
        let queue = &self.queue;

        let pixel_bytes = (pixel_count * std::mem::size_of::<f32>()) as wgpu::BufferAddress;
        let column_bytes =
            (config.width as usize * std::mem::size_of::<f32>()) as wgpu::BufferAddress;
        let column_stats_bytes =
            (config.width as usize * std::mem::size_of::<ColumnStat>()) as wgpu::BufferAddress;

        let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge-texture-input-buffer"),
            contents: cast_slice(input),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let gamma_buffer = create_storage_buffer(device, pixel_bytes, "edge-texture-gamma-buffer");
        let gaussian_temp_buffer =
            create_storage_buffer(device, pixel_bytes, "edge-texture-gaussian-temp-buffer");
        let gaussian_buffer =
            create_storage_buffer(device, pixel_bytes, "edge-texture-gaussian-output-buffer");
        let gradient_buffer =
            create_storage_buffer(device, pixel_bytes, "edge-texture-gradient-buffer");
        let column_stats_buffer = create_storage_buffer(
            device,
            column_stats_bytes,
            "edge-texture-column-stats-buffer",
        );
        let entropy_buffer =
            create_storage_buffer(device, column_bytes, "edge-texture-entropy-buffer");

        let gamma_readback =
            create_readback_buffer(device, pixel_bytes, "edge-texture-gamma-readback");
        let gaussian_readback =
            create_readback_buffer(device, pixel_bytes, "edge-texture-gaussian-readback");
        let gradient_readback =
            create_readback_buffer(device, pixel_bytes, "edge-texture-gradient-readback");
        let column_stats_readback = create_readback_buffer(
            device,
            column_stats_bytes,
            "edge-texture-column-stats-readback",
        );
        let entropy_readback =
            create_readback_buffer(device, column_bytes, "edge-texture-entropy-readback");

        let kernel = build_gaussian_kernel(config.sanitized_gaussian_kernel());
        let kernel_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge-texture-kernel-buffer"),
            contents: cast_slice(&kernel),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let info = config.to_pipeline_info();
        if info.entropy_bins as usize > 64 {
            return Err("entropy bins exceed shader MAX_BINS".into());
        }
        let info_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge-texture-info-buffer"),
            contents: bytes_of(&info),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let gamma_pipeline = create_pipeline(
            device,
            GAMMA_SHADER,
            "edge-texture-gamma-pipeline",
            "gamma_main",
            &[
                bind_read_storage_entry(0),
                bind_read_write_storage_entry(1),
                bind_read_storage_entry(2),
            ],
            &[&input_buffer, &gamma_buffer, &info_buffer],
        );

        let gaussian_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("edge-texture-gaussian-shader"),
            source: wgpu::ShaderSource::Wgsl(GAUSSIAN_SHADER.into()),
        });
        let gaussian_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("edge-texture-gaussian-bind-group-layout"),
            entries: &[
                bind_read_storage_entry(0),
                bind_read_write_storage_entry(1),
                bind_read_storage_entry(2),
                bind_read_storage_entry(3),
            ],
        });
        let gaussian_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("edge-texture-gaussian-pipeline-layout"),
                bind_group_layouts: &[&gaussian_layout],
                push_constant_ranges: &[],
            });
        let gaussian_horizontal_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("edge-texture-gaussian-horizontal"),
                layout: Some(&gaussian_pipeline_layout),
                module: &gaussian_shader_module,
                entry_point: "gaussian_horizontal",
            });
        let gaussian_vertical_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("edge-texture-gaussian-vertical"),
                layout: Some(&gaussian_pipeline_layout),
                module: &gaussian_shader_module,
                entry_point: "gaussian_vertical",
            });

        let gaussian_horizontal_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edge-texture-gaussian-horizontal-bind-group"),
            layout: &gaussian_layout,
            entries: &[
                bind_storage(&gamma_buffer, 0),
                bind_storage(&gaussian_temp_buffer, 1),
                bind_storage(&kernel_buffer, 2),
                bind_storage(&info_buffer, 3),
            ],
        });
        let gaussian_vertical_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edge-texture-gaussian-vertical-bind-group"),
            layout: &gaussian_layout,
            entries: &[
                bind_storage(&gaussian_temp_buffer, 0),
                bind_storage(&gaussian_buffer, 1),
                bind_storage(&kernel_buffer, 2),
                bind_storage(&info_buffer, 3),
            ],
        });

        let sobel_pipeline = create_pipeline(
            device,
            SOBEL_SHADER,
            "edge-texture-sobel-pipeline",
            "sobel_main",
            &[
                bind_read_storage_entry(0),
                bind_read_write_storage_entry(1),
                bind_read_storage_entry(2),
            ],
            &[&gaussian_buffer, &gradient_buffer, &info_buffer],
        );

        let column_pipeline = create_pipeline(
            device,
            COLUMN_STATS_SHADER,
            "edge-texture-column-pipeline",
            "column_stats",
            &[
                bind_read_storage_entry(0),
                bind_read_storage_entry(1),
                bind_read_storage_entry(2),
                bind_read_write_storage_entry(3),
            ],
            &[
                &gamma_buffer,
                &gradient_buffer,
                &info_buffer,
                &column_stats_buffer,
            ],
        );

        let entropy_pipeline = create_pipeline(
            device,
            ENTROPY_SHADER,
            "edge-texture-entropy-pipeline",
            "entropy_main",
            &[
                bind_read_storage_entry(0),
                bind_read_storage_entry(1),
                bind_read_write_storage_entry(2),
            ],
            &[&gaussian_buffer, &info_buffer, &entropy_buffer],
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("edge-texture-command-encoder"),
        });

        dispatch_1d(
            &mut encoder,
            &gamma_pipeline.pipeline,
            &gamma_pipeline.bind_group,
            (config.pixel_count() as u32 + 255) / 256,
        );
        dispatch_2d(
            &mut encoder,
            &gaussian_horizontal_pipeline,
            &gaussian_horizontal_bind_group,
            config.width,
            config.height,
        );
        dispatch_2d(
            &mut encoder,
            &gaussian_vertical_pipeline,
            &gaussian_vertical_bind_group,
            config.width,
            config.height,
        );
        dispatch_2d(
            &mut encoder,
            &sobel_pipeline.pipeline,
            &sobel_pipeline.bind_group,
            config.width,
            config.height,
        );
        dispatch_1d(
            &mut encoder,
            &column_pipeline.pipeline,
            &column_pipeline.bind_group,
            (config.width + 255) / 256,
        );
        dispatch_1d(
            &mut encoder,
            &entropy_pipeline.pipeline,
            &entropy_pipeline.bind_group,
            (config.width + 255) / 256,
        );

        enqueue_copy(&mut encoder, &gamma_buffer, &gamma_readback, pixel_bytes);
        enqueue_copy(
            &mut encoder,
            &gaussian_buffer,
            &gaussian_readback,
            pixel_bytes,
        );
        enqueue_copy(
            &mut encoder,
            &gradient_buffer,
            &gradient_readback,
            pixel_bytes,
        );
        enqueue_copy(
            &mut encoder,
            &column_stats_buffer,
            &column_stats_readback,
            column_stats_bytes,
        );
        enqueue_copy(
            &mut encoder,
            &entropy_buffer,
            &entropy_readback,
            column_bytes,
        );

        queue.submit(std::iter::once(encoder.finish()));

        let gamma = read_buffer::<f32>(device, &gamma_readback, pixel_count)?;
        let gaussian = read_buffer::<f32>(device, &gaussian_readback, pixel_count)?;
        let gradient = read_buffer::<f32>(device, &gradient_readback, pixel_count)?;
        let entropy = read_buffer::<f32>(device, &entropy_readback, config.width as usize)?;

        let column_stats =
            read_buffer::<ColumnStat>(device, &column_stats_readback, config.width as usize)?;
        let mut mean_intensity = Vec::with_capacity(column_stats.len());
        let mut grad_mean = Vec::with_capacity(column_stats.len());
        let mut grad_variance = Vec::with_capacity(column_stats.len());
        for stat in &column_stats {
            mean_intensity.push(stat.mean_intensity);
            grad_mean.push(stat.grad_mean);
            grad_variance.push(stat.grad_variance);
        }

        Ok(GpuOutputs {
            gamma,
            gaussian,
            gradient,
            mean_intensity,
            grad_mean,
            grad_variance,
            entropy,
        })
    }
}

#[derive(Clone, Copy)]
struct DiffStat {
    max: f32,
    avg: f32,
}

struct DiffSummary {
    gamma: DiffStat,
    gaussian: DiffStat,
    gradient: DiffStat,
    mean_intensity: DiffStat,
    grad_mean: DiffStat,
    grad_variance: DiffStat,
    entropy: DiffStat,
}

impl DiffStat {
    fn new(max: f32, avg: f32) -> Self {
        Self { max, avg }
    }
}

impl DiffSummary {
    fn format_compact(&self) -> String {
        format!(
            "gamma: max={:.6} avg={:.6}; gaussian: max={:.6} avg={:.6}; grad: max={:.6} avg={:.6}; column mean: max={:.6} avg={:.6}; grad mean: max={:.6} avg={:.6}; grad variance: max={:.6} avg={:.6}; entropy: max={:.6} avg={:.6}",
            self.gamma.max,
            self.gamma.avg,
            self.gaussian.max,
            self.gaussian.avg,
            self.gradient.max,
            self.gradient.avg,
            self.mean_intensity.max,
            self.mean_intensity.avg,
            self.grad_mean.max,
            self.grad_mean.avg,
            self.grad_variance.max,
            self.grad_variance.avg,
            self.entropy.max,
            self.entropy.avg,
        )
    }
}

struct LoadedSurface {
    width: u32,
    height: u32,
    pixels: Vec<f32>,
}

struct BenchmarkResult {
    path: PathBuf,
    width: u32,
    height: u32,
    cpu_duration: Duration,
    gpu_duration: Duration,
    diff: DiffSummary,
}

fn main() {
    if let Err(error) = pollster::block_on(run()) {
        eprintln!("wgpu EdgeTexture PoC failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        run_demo().await
    } else {
        run_benchmark(&args).await
    }
}

async fn run_demo() -> Result<(), Box<dyn Error>> {
    let config = EdgeTexturePipelineConfig::new(
        DEMO_WIDTH,
        DEMO_HEIGHT,
        DEFAULT_GAUSSIAN_KERNEL,
        DEFAULT_ENTROPY_WINDOW,
        DEFAULT_ENTROPY_BINS,
        DEFAULT_GAMMA,
    );
    let gaussian_kernel = config.sanitized_gaussian_kernel();
    let entropy_window = config.sanitized_entropy_window();
    let entropy_bins = config.sanitized_entropy_bins();

    let input = generate_demo_surface(config.width, config.height);
    let cpu = compute_cpu_reference(
        config.width as usize,
        config.height as usize,
        &input,
        config.gamma,
        gaussian_kernel,
        entropy_window,
        entropy_bins,
    );

    let context = EdgeTextureGpuContext::new().await?;
    let gpu = context.execute(&input, config)?;
    let summary = validate_results(&cpu, &gpu)?;

    println!("EdgeTexture GPU PoC B1: validation succeeded");
    println!("{}", summary.format_compact());
    Ok(())
}

async fn run_benchmark(args: &[String]) -> Result<(), Box<dyn Error>> {
    let input_paths = collect_input_paths(args)?;
    if input_paths.is_empty() {
        return Err("no supported images found in the provided paths".into());
    }

    let context = EdgeTextureGpuContext::new().await?;
    let mut results = Vec::with_capacity(input_paths.len());

    for path in input_paths {
        let result = evaluate_image(&context, path)?;
        results.push(result);
    }

    print_benchmark_results(&results);
    Ok(())
}

fn evaluate_image(
    context: &EdgeTextureGpuContext,
    path: PathBuf,
) -> Result<BenchmarkResult, Box<dyn Error>> {
    let surface = load_grayscale_surface(&path)?;
    let config = EdgeTexturePipelineConfig::new(
        surface.width,
        surface.height,
        DEFAULT_GAUSSIAN_KERNEL,
        DEFAULT_ENTROPY_WINDOW,
        DEFAULT_ENTROPY_BINS,
        DEFAULT_GAMMA,
    );
    let gaussian_kernel = config.sanitized_gaussian_kernel();
    let entropy_window = config.sanitized_entropy_window();
    let entropy_bins = config.sanitized_entropy_bins();

    let cpu_start = Instant::now();
    let cpu = compute_cpu_reference(
        surface.width as usize,
        surface.height as usize,
        &surface.pixels,
        config.gamma,
        gaussian_kernel,
        entropy_window,
        entropy_bins,
    );
    let cpu_duration = cpu_start.elapsed();

    let gpu_start = Instant::now();
    let gpu = context.execute(&surface.pixels, config)?;
    let gpu_duration = gpu_start.elapsed();

    let diff = validate_results(&cpu, &gpu)?;

    Ok(BenchmarkResult {
        path,
        width: surface.width,
        height: surface.height,
        cpu_duration,
        gpu_duration,
        diff,
    })
}

fn collect_input_paths(args: &[String]) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut inputs = Vec::new();
    for raw in args {
        let path = PathBuf::from(raw);
        if !path.exists() {
            return Err(format!("input path {:?} does not exist", path).into());
        }
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    let file_path = entry.path();
                    if is_supported_image(&file_path) {
                        inputs.push(file_path);
                    }
                }
            }
        } else if is_supported_image(&path) {
            inputs.push(path);
        }
    }
    inputs.sort();
    inputs.dedup();
    Ok(inputs)
}

fn is_supported_image(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "bmp" | "tiff" | "webp"
        ),
        None => false,
    }
}

fn load_grayscale_surface(path: &Path) -> Result<LoadedSurface, Box<dyn Error>> {
    let image = image::open(path)?;
    let gray = image.to_luma8();
    let width = gray.width();
    let height = gray.height();
    let raw = gray.into_raw();
    let pixels = raw.into_iter().map(|value| value as f32).collect();
    Ok(LoadedSurface {
        width,
        height,
        pixels,
    })
}

fn print_benchmark_results(results: &[BenchmarkResult]) {
    println!("Processed {} image(s) with DEFAULT settings", results.len());
    if results.is_empty() {
        return;
    }

    let mut total_cpu = Duration::ZERO;
    let mut total_gpu = Duration::ZERO;
    let mut max_gamma: f32 = 0.0;
    let mut max_gaussian: f32 = 0.0;
    let mut max_gradient: f32 = 0.0;
    let mut max_mean: f32 = 0.0;
    let mut max_grad_mean: f32 = 0.0;
    let mut max_grad_variance: f32 = 0.0;
    let mut max_entropy: f32 = 0.0;

    for result in results {
        total_cpu += result.cpu_duration;
        total_gpu += result.gpu_duration;
        max_gamma = max_gamma.max(result.diff.gamma.max);
        max_gaussian = max_gaussian.max(result.diff.gaussian.max);
        max_gradient = max_gradient.max(result.diff.gradient.max);
        max_mean = max_mean.max(result.diff.mean_intensity.max);
        max_grad_mean = max_grad_mean.max(result.diff.grad_mean.max);
        max_grad_variance = max_grad_variance.max(result.diff.grad_variance.max);
        max_entropy = max_entropy.max(result.diff.entropy.max);

        let speedup = compute_speedup(result.cpu_duration, result.gpu_duration);
        println!(
            "{}: {}x{} cpu={:.2?} gpu={:.2?} speedup={:.2}x | {}",
            result.path.to_string_lossy(),
            result.width,
            result.height,
            result.cpu_duration,
            result.gpu_duration,
            speedup,
            result.diff.format_compact(),
        );
    }

    let aggregate_speedup = compute_speedup(total_cpu, total_gpu);
    println!(
        "Totals -> cpu={:.2?}, gpu={:.2?}, aggregate speedup={:.2}x",
        total_cpu, total_gpu, aggregate_speedup,
    );
    println!(
        "Worst-case diffs -> gamma={:.3e}, gaussian={:.3e}, grad={:.3e}, column mean={:.3e}, grad mean={:.3e}, grad variance={:.3e}, entropy={:.3e}",
        max_gamma,
        max_gaussian,
        max_gradient,
        max_mean,
        max_grad_mean,
        max_grad_variance,
        max_entropy,
    );
}

fn compute_speedup(cpu: Duration, gpu: Duration) -> f64 {
    if gpu.is_zero() {
        f64::INFINITY
    } else {
        cpu.as_secs_f64() / gpu.as_secs_f64()
    }
}

struct PipelineWithBindGroup {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
}

fn create_pipeline(
    device: &wgpu::Device,
    shader_source: &str,
    pipeline_label: &str,
    entry_point: &str,
    layout_entries: &[wgpu::BindGroupLayoutEntry],
    buffers: &[&wgpu::Buffer],
) -> PipelineWithBindGroup {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(pipeline_label),
        source: wgpu::ShaderSource::Wgsl(shader_source.into()),
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(&format!("{pipeline_label}-layout")),
        entries: layout_entries,
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{pipeline_label}-pipeline-layout")),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(pipeline_label),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point,
    });

    let entries: Vec<_> = buffers
        .iter()
        .enumerate()
        .map(|(idx, buffer)| bind_storage(buffer, idx as u32))
        .collect();

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&format!("{pipeline_label}-bind-group")),
        layout: &layout,
        entries: &entries,
    });

    PipelineWithBindGroup {
        pipeline,
        bind_group,
    }
}

fn bind_storage<'a>(buffer: &'a wgpu::Buffer, binding: u32) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn bind_read_storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bind_read_write_storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn create_storage_buffer(
    device: &wgpu::Device,
    size: wgpu::BufferAddress,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_readback_buffer(
    device: &wgpu::Device,
    size: wgpu::BufferAddress,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn dispatch_1d(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bind_group: &wgpu::BindGroup,
    workgroups: u32,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("edge-texture-pass-1d"),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.dispatch_workgroups(workgroups, 1, 1);
}

fn dispatch_2d(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bind_group: &wgpu::BindGroup,
    width: u32,
    height: u32,
) {
    let groups_x = (width + 15) / 16;
    let groups_y = (height + 15) / 16;
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("edge-texture-pass-2d"),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.dispatch_workgroups(groups_x.max(1), groups_y.max(1), 1);
}

fn enqueue_copy(
    encoder: &mut wgpu::CommandEncoder,
    src: &wgpu::Buffer,
    dst: &wgpu::Buffer,
    size: wgpu::BufferAddress,
) {
    encoder.copy_buffer_to_buffer(src, 0, dst, 0, size);
}

fn read_buffer<T: Pod>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    expected_len: usize,
) -> Result<Vec<T>, Box<dyn Error>> {
    let slice = buffer.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender
            .send(result)
            .expect("failed to send map_async result");
    });
    device.poll(wgpu::Maintain::Wait);
    match receiver.recv() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(Box::new(err)),
    }
    let data = slice.get_mapped_range();
    let values: Vec<T> = cast_slice(&data).to_vec();
    drop(data);
    buffer.unmap();
    if values.len() != expected_len {
        return Err(format!(
            "unexpected buffer length: got {}, expected {}",
            values.len(),
            expected_len
        )
        .into());
    }
    Ok(values)
}

fn validate_results(cpu: &CpuReference, gpu: &GpuOutputs) -> Result<DiffSummary, Box<dyn Error>> {
    let summary = compute_diff_summary(cpu, gpu)?;

    ensure_within(summary.gamma.max, 1e-4, "gamma");
    ensure_within(summary.gaussian.max, 1e-4, "gaussian");
    // GPU Sobel accumulation introduces slightly higher round-off error; accept 2e-4.
    ensure_within(summary.gradient.max, 2e-4, "gradient");
    ensure_within(summary.mean_intensity.max, 1e-4, "column mean");
    ensure_within(summary.grad_mean.max, 1e-4, "grad mean");
    // Column variance squares gradient magnitudes, so tolerate larger drift.
    ensure_within(summary.grad_variance.max, 2e-2, "grad variance");
    ensure_within(summary.entropy.max, 1e-3, "entropy");

    Ok(summary)
}

fn compute_diff_summary(
    cpu: &CpuReference,
    gpu: &GpuOutputs,
) -> Result<DiffSummary, Box<dyn Error>> {
    let gamma = diff_stats(&cpu.gamma, &gpu.gamma)?;
    let gaussian = diff_stats(&cpu.gaussian, &gpu.gaussian)?;
    let gradient = diff_stats(&cpu.gradient, &gpu.gradient)?;
    let mean_intensity = diff_stats(&cpu.mean_intensity, &gpu.mean_intensity)?;
    let grad_mean = diff_stats(&cpu.grad_mean, &gpu.grad_mean)?;
    let grad_variance = diff_stats(&cpu.grad_variance, &gpu.grad_variance)?;
    let entropy = diff_stats(&cpu.entropy, &gpu.entropy)?;

    Ok(DiffSummary {
        gamma: DiffStat::new(gamma.0, gamma.1),
        gaussian: DiffStat::new(gaussian.0, gaussian.1),
        gradient: DiffStat::new(gradient.0, gradient.1),
        mean_intensity: DiffStat::new(mean_intensity.0, mean_intensity.1),
        grad_mean: DiffStat::new(grad_mean.0, grad_mean.1),
        grad_variance: DiffStat::new(grad_variance.0, grad_variance.1),
        entropy: DiffStat::new(entropy.0, entropy.1),
    })
}

fn ensure_within(value: f32, tolerance: f32, label: &str) {
    if value > tolerance {
        panic!("{label} diff {value} exceeded tolerance {tolerance}");
    }
}

fn diff_stats(cpu: &[f32], gpu: &[f32]) -> Result<(f32, f32), Box<dyn Error>> {
    if cpu.len() != gpu.len() {
        return Err(format!(
            "length mismatch for diff: cpu={} gpu={}",
            cpu.len(),
            gpu.len()
        )
        .into());
    }
    let mut max_diff = 0.0f32;
    let mut avg_diff = 0.0f32;
    for (lhs, rhs) in cpu.iter().zip(gpu.iter()) {
        let diff = (lhs - rhs).abs();
        if diff > max_diff {
            max_diff = diff;
        }
        avg_diff += diff;
    }
    if !cpu.is_empty() {
        avg_diff /= cpu.len() as f32;
    }
    Ok((max_diff, avg_diff))
}

fn compute_cpu_reference(
    width: usize,
    height: usize,
    input: &[f32],
    gamma: f32,
    gaussian_kernel: u32,
    entropy_window: u32,
    entropy_bins: u32,
) -> CpuReference {
    let gamma_pixels = apply_gamma(input, gamma);
    let gaussian = gaussian_blur(width, height, &gamma_pixels, ensure_odd(gaussian_kernel));
    let gradient = sobel_magnitude(width, height, &gaussian);
    let mean_intensity = compute_column_mean_intensity(width, height, &gamma_pixels);
    let (grad_mean, grad_variance) = compute_column_stats(width, height, &gradient);
    let entropy = compute_entropy(
        width,
        height,
        &gaussian,
        ensure_odd(entropy_window) as usize,
        entropy_bins.max(1) as usize,
    );

    CpuReference {
        gamma: gamma_pixels,
        gaussian,
        gradient,
        mean_intensity,
        grad_mean,
        grad_variance,
        entropy,
    }
}

fn generate_demo_surface(width: u32, height: u32) -> Vec<f32> {
    let mut output = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let value = ((x * 7 + y * 13) % 256) as f32;
            output.push(value);
        }
    }
    output
}

fn apply_gamma(data: &[f32], gamma: f32) -> Vec<f32> {
    if (gamma - 1.0).abs() <= 1e-6 {
        return data.iter().map(|value| value.clamp(0.0, 255.0)).collect();
    }
    let inv = if gamma <= 1e-6 { 1.0 } else { 1.0 / gamma };
    data.iter()
        .map(|value| {
            let normalized = value.clamp(0.0, 255.0) / 255.0;
            (normalized.powf(inv) * 255.0).clamp(0.0, 255.0)
        })
        .collect()
}

fn gaussian_blur(width: usize, height: usize, input: &[f32], kernel_size: u32) -> Vec<f32> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let kernel_size = ensure_odd(kernel_size);
    if kernel_size <= 1 {
        return input.to_vec();
    }

    let kernel = build_gaussian_kernel(kernel_size);
    let radius = (kernel_size / 2) as isize;
    let mut horizontal = vec![0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (idx, &weight) in kernel.iter().enumerate() {
                let offset = idx as isize - radius;
                let sample_x = clamp_i32(x as isize + offset, 0, (width - 1) as isize) as usize;
                acc += input[y * width + sample_x] * weight;
            }
            horizontal[y * width + x] = acc;
        }
    }

    let mut output = vec![0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (idx, &weight) in kernel.iter().enumerate() {
                let offset = idx as isize - radius;
                let sample_y = clamp_i32(y as isize + offset, 0, (height - 1) as isize) as usize;
                acc += horizontal[sample_y * width + x] * weight;
            }
            output[y * width + x] = acc;
        }
    }
    output
}

fn build_gaussian_kernel(size: u32) -> Vec<f32> {
    let size = ensure_odd(size);
    if size <= 1 {
        return vec![1.0];
    }
    let radius = (size / 2) as i32;
    let sigma = gaussian_sigma(size);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut kernel = Vec::with_capacity(size as usize);
    for idx in -radius..=radius {
        let dist = idx as f32;
        let weight = (-(dist * dist) / two_sigma_sq).exp();
        kernel.push(weight);
    }
    let sum: f32 = kernel.iter().sum();
    if sum > 0.0 {
        for weight in kernel.iter_mut() {
            *weight /= sum;
        }
    }
    kernel
}

fn gaussian_sigma(size: u32) -> f32 {
    if size <= 1 {
        0.0
    } else {
        let size_f = size as f32;
        0.3 * ((size_f - 1.0) * 0.5 - 1.0) + 0.8
    }
}

fn sobel_magnitude(width: usize, height: usize, data: &[f32]) -> Vec<f32> {
    let mut output = vec![0f32; width * height];
    if width == 0 || height == 0 {
        return output;
    }

    for y in 0..height {
        for x in 0..width {
            let mut gx = 0.0;
            let mut gy = 0.0;
            for ky in 0..3 {
                for kx in 0..3 {
                    let offset_x = x as isize + kx as isize - 1;
                    let offset_y = y as isize + ky as isize - 1;
                    let sample_x = clamp_i32(offset_x, 0, (width - 1) as isize) as usize;
                    let sample_y = clamp_i32(offset_y, 0, (height - 1) as isize) as usize;
                    let value = data[sample_y * width + sample_x];
                    gx += value * SOBEL_X_CPU[ky][kx];
                    gy += value * SOBEL_Y_CPU[ky][kx];
                }
            }
            output[y * width + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    output
}

const SOBEL_X_CPU: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
const SOBEL_Y_CPU: [[f32; 3]; 3] = [[1.0, 2.0, 1.0], [0.0, 0.0, 0.0], [-1.0, -2.0, -1.0]];

fn compute_column_mean_intensity(width: usize, height: usize, data: &[f32]) -> Vec<f32> {
    if width == 0 {
        return Vec::new();
    }

    let mut sums = vec![0f32; width];
    if height == 0 {
        return sums;
    }

    for y in 0..height {
        let row_offset = y * width;
        for x in 0..width {
            if let Some(&value) = data.get(row_offset + x) {
                sums[x] += value;
            }
        }
    }

    let denom = height.max(1) as f32;
    for value in sums.iter_mut() {
        *value = (*value / denom).clamp(0.0, 255.0);
    }
    sums
}

fn compute_column_stats(width: usize, height: usize, data: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut sum = vec![0f32; width];
    let mut sum_sq = vec![0f32; width];
    for y in 0..height {
        for x in 0..width {
            let value = data[y * width + x];
            sum[x] += value;
            sum_sq[x] += value * value;
        }
    }

    if height == 0 {
        return (sum, sum_sq);
    }

    let inv_height = 1.0 / height as f32;
    let mut mean = vec![0f32; width];
    let mut variance = vec![0f32; width];
    for idx in 0..width {
        let m = sum[idx] * inv_height;
        let v = (sum_sq[idx] * inv_height) - m * m;
        mean[idx] = m;
        variance[idx] = if v < 0.0 { 0.0 } else { v };
    }
    (mean, variance)
}

fn compute_entropy(
    width: usize,
    height: usize,
    data: &[f32],
    window: usize,
    bins: usize,
) -> Vec<f32> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let window = max(1, window);
    let bins = max(1, bins);
    let pad = window / 2;
    let mut entropy = vec![0f32; width];
    let mut histogram = vec![0f32; bins];

    for x in 0..width {
        for value in histogram.iter_mut() {
            *value = 0.0;
        }
        let start = x.saturating_sub(pad);
        let end = min(width, x + pad + 1);
        let span = end.saturating_sub(start);
        let total_pixels = (span * height) as f32;
        if total_pixels <= 0.0 {
            entropy[x] = 0.0;
            continue;
        }

        for col in start..end {
            for row in 0..height {
                let value = data[row * width + col].clamp(0.0, 255.0);
                let mut bin = ((value / 256.0) * bins as f32).floor() as usize;
                if bin >= bins {
                    bin = bins - 1;
                }
                histogram[bin] += 1.0;
            }
        }

        let mut ent = 0.0;
        for &count in &histogram {
            if count <= 0.0 {
                continue;
            }
            let p = count / total_pixels;
            ent -= p * (p + 1e-12).log2();
        }
        entropy[x] = ent;
    }

    entropy
}

fn ensure_odd(value: u32) -> u32 {
    if value == 0 {
        1
    } else if value % 2 == 0 {
        value + 1
    } else {
        value
    }
}

fn clamp_i32(value: isize, min_value: isize, max_value: isize) -> isize {
    if value < min_value {
        min_value
    } else if value > max_value {
        max_value
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "edge-texture-gpu")]
    fn cpu_gpu_reference_consistency() {
        pollster::block_on(async {
            match run().await {
                Ok(()) => {}
                Err(err) => {
                    let message = err.to_string();
                    if message.contains("no compatible GPU adapter found") {
                        eprintln!("skip edge texture GPU PoC test: {message}");
                    } else {
                        panic!("PoC failed during test: {message}");
                    }
                }
            }
        });
    }

    #[test]
    #[cfg(feature = "edge-texture-gpu")]
    fn cpu_gpu_real_sample_consistency() {
        pollster::block_on(async {
            let sample_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("debug_edge.png");
            if !sample_path.exists() {
                eprintln!("skip edge texture sample test: {:?} missing", sample_path);
                return;
            }

            let context = match EdgeTextureGpuContext::new().await {
                Ok(ctx) => ctx,
                Err(err) => {
                    let message = err.to_string();
                    if message.contains("no compatible GPU adapter found") {
                        eprintln!("skip edge texture GPU sample test: {message}");
                        return;
                    }
                    panic!("failed to initialize GPU context: {message}");
                }
            };

            match evaluate_image(&context, sample_path.clone()) {
                Ok(result) => {
                    assert!(
                        result.width > 0 && result.height > 0,
                        "sample dimensions invalid"
                    );
                    assert!(
                        result.cpu_duration > Duration::ZERO,
                        "CPU duration should be positive"
                    );
                    assert!(
                        result.gpu_duration > Duration::ZERO,
                        "GPU duration should be positive"
                    );
                }
                Err(err) => panic!(
                    "edge texture sample benchmark failed for {:?}: {}",
                    sample_path, err
                ),
            }
        });
    }
}
