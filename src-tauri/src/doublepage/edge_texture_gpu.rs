#[cfg(feature = "edge-texture-gpu")]
pub(crate) mod enabled {
    use std::borrow::Cow;
    use std::sync::mpsc;

    use bytemuck::{bytes_of, cast_slice, Pod, Zeroable};
    use image::DynamicImage;
    use pollster::block_on;
    use thiserror::Error;
    use wgpu::util::DeviceExt;

    use crate::doublepage::edge_texture::{build_gaussian_kernel, EdgeTextureConfig};

    const MAX_ENTROPY_BINS: u32 = 64;

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

fn sample_index(x: i32, y: i32) -> u32 {
    let sx = clamp(x, 0, i32(info.width) - 1);
    let sy = clamp(y, 0, i32(info.height) - 1);
    return u32(sy) * info.width + u32(sx);
}

@compute @workgroup_size(16, 16, 1)
fn sobel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= info.width || y >= info.height) {
        return;
    }

    var gx = 0.0;
    var gy = 0.0;
    var row = 0u;
    loop {
        if (row >= 3u) {
            break;
        }
        var col = 0u;
        loop {
            if (col >= 3u) {
                break;
            }
            let offset_x = i32(col) - 1;
            let offset_y = i32(row) - 1;
            let sample_x = clamp(i32(x) + offset_x, 0, i32(info.width) - 1);
            let sample_y = clamp(i32(y) + offset_y, 0, i32(info.height) - 1);
            let value = blurred_pixels[u32(sample_y) * info.width + u32(sample_x)];
            gx = gx + value * sobel_weight_x(row, col);
            gy = gy + value * sobel_weight_y(row, col);
            col = col + 1u;
        }
        row = row + 1u;
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

struct ColumnStat {
    mean_intensity: f32,
    grad_mean: f32,
    grad_variance: f32,
    padding: f32,
};

@group(0) @binding(0)
var<storage, read> gamma_pixels: array<f32>;

@group(0) @binding(1)
var<storage, read> gradient_pixels: array<f32>;

@group(0) @binding(2)
var<storage, read> info: PipelineInfo;

@group(0) @binding(3)
var<storage, read_write> stats: array<ColumnStat>;

@compute @workgroup_size(64, 1, 1)
fn column_stats(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    if (x >= info.width) {
        return;
    }

    let column_start = x;
    let stride = info.width;
    var sum_intensity = 0.0;
    var sum_grad = 0.0;
    var sum_grad_sq = 0.0;
    var row = 0u;
    loop {
        if (row >= info.height) {
            break;
        }
        let idx = column_start + row * stride;
        let intensity = gamma_pixels[idx];
        let grad = gradient_pixels[idx];
        sum_intensity = sum_intensity + intensity;
        sum_grad = sum_grad + grad;
        sum_grad_sq = sum_grad_sq + grad * grad;
        row = row + 1u;
    }

    let height_f = info.height_f;
    if (height_f <= 0.0) {
        stats[x] = ColumnStat(0.0, 0.0, 0.0, 0.0);
        return;
    }

    let mean_intensity = sum_intensity / height_f;
    let grad_mean = sum_grad / height_f;
    let grad_variance = max(sum_grad_sq / height_f - grad_mean * grad_mean, 0.0);

    stats[x] = ColumnStat(mean_intensity, grad_mean, grad_variance, 0.0);
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
var<storage, read_write> entropy: array<f32>;

@group(0) @binding(2)
var<storage, read> info: PipelineInfo;

@compute @workgroup_size(64, 1, 1)
fn entropy_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    if (x >= info.width) {
        return;
    }

    let window = max(info.entropy_window, 1u);
    let radius = window / 2u;
    let bins = clamp(info.entropy_bins, 1u, MAX_BINS);
    var histogram: array<f32, MAX_BINS>;
    for (var i: u32 = 0u; i < MAX_BINS; i = i + 1u) {
        histogram[i] = 0.0;
    }

    let height = info.height;
    var row = 0u;
    loop {
        if (row >= height) {
            break;
        }
        var offset = 0u;
        loop {
            if (offset >= window) {
                break;
            }
            let sample_row = clamp(i32(row) + i32(offset) - i32(radius), 0, i32(height) - 1);
            let idx = u32(sample_row) * info.width + x;
            let value = clamp(blurred_pixels[idx], 0.0, 255.0);
            let bin = u32(value / 255.0 * f32(bins - 1u));
            histogram[bin] = histogram[bin] + 1.0;
            offset = offset + 1u;
        }
        row = row + window;
    }

    var entropy_value = 0.0;
    var total = 0.0;
    for (var i: u32 = 0u; i < bins; i = i + 1u) {
        total = total + histogram[i];
    }
    if (total <= 0.0) {
        entropy[x] = 0.0;
        return;
    }
    for (var i: u32 = 0u; i < bins; i = i + 1u) {
        let probability = histogram[i] / total;
        if (probability > 0.0) {
            entropy_value = entropy_value - probability * log2(probability);
        }
    }
    entropy[x] = entropy_value;
}
"#;

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
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

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Pod, Zeroable)]
    struct ColumnStat {
        mean_intensity: f32,
        grad_mean: f32,
        grad_variance: f32,
        padding: f32,
    }

    #[derive(Debug, Error)]
    pub enum EdgeTextureGpuError {
        #[error("edge texture GPU adapter unavailable")]
        AdapterUnavailable,
        #[error("edge texture GPU device initialization failed: {0}")]
        DeviceInit(String),
        #[error("edge texture GPU pipeline input invalid: {0}")]
        InvalidInput(&'static str),
        #[error("edge texture GPU execution failed: {0}")]
        Execution(String),
    }
    #[derive(Debug, Clone)]
    pub struct EdgeTextureGpuOutputs {
        pub width: u32,
        pub mean_intensity: Vec<f32>,
        pub grad_mean: Vec<f32>,
        pub grad_variance: Vec<f32>,
        pub entropy: Vec<f32>,
    }

    impl EdgeTextureGpuOutputs {
        fn empty(width: u32) -> Self {
            let len = width as usize;
            Self {
                width,
                mean_intensity: vec![0.0; len],
                grad_mean: vec![0.0; len],
                grad_variance: vec![0.0; len],
                entropy: vec![0.0; len],
            }
        }
    }

    pub struct EdgeTextureGpuAnalyzer {
        context: EdgeTextureGpuContext,
    }

    impl EdgeTextureGpuAnalyzer {
        pub fn new() -> Result<Self, EdgeTextureGpuError> {
            let context = EdgeTextureGpuContext::new()?;
            Ok(Self { context })
        }

        pub fn analyze(
            &self,
            image: &DynamicImage,
            config: EdgeTextureConfig,
        ) -> Result<EdgeTextureGpuOutputs, EdgeTextureGpuError> {
            let surface = load_surface(image);
            if surface.width == 0 || surface.height == 0 {
                return Ok(EdgeTextureGpuOutputs::empty(surface.width));
            }

            let pipeline_config = EdgeTexturePipelineConfig::new(
                surface.width,
                surface.height,
                config.gaussian_kernel,
                config.entropy_window,
                config.entropy_bins,
                config.gamma,
            );

            let outputs = self.context.execute(&surface.pixels, pipeline_config)?;
            Ok(EdgeTextureGpuOutputs {
                width: surface.width,
                // height: surface.height,
                mean_intensity: outputs.mean_intensity,
                grad_mean: outputs.grad_mean,
                grad_variance: outputs.grad_variance,
                entropy: outputs.entropy,
            })
        }
    }

    struct EdgeTextureGpuContext {
        device: wgpu::Device,
        queue: wgpu::Queue,
    }

    impl EdgeTextureGpuContext {
        fn new() -> Result<Self, EdgeTextureGpuError> {
            let instance = wgpu::Instance::default();
            let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            }))
            .ok_or(EdgeTextureGpuError::AdapterUnavailable)?;

            let required_limits = wgpu::Limits::downlevel_defaults();
            let (device, queue) = block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("edge-texture-gpu-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                },
                None,
            ))
            .map_err(|err| EdgeTextureGpuError::DeviceInit(err.to_string()))?;

            Ok(Self { device, queue })
        }

        fn execute(
            &self,
            input: &[f32],
            config: EdgeTexturePipelineConfig,
        ) -> Result<GpuOutputs, EdgeTextureGpuError> {
            if config.is_empty() {
                return Ok(GpuOutputs::empty(config.width));
            }

            let pixel_count = config.pixel_count();
            if input.len() != pixel_count {
                return Err(EdgeTextureGpuError::InvalidInput("input length mismatch"));
            }

            let device = &self.device;
            let queue = &self.queue;

            let pixel_bytes = (pixel_count * std::mem::size_of::<f32>()) as wgpu::BufferAddress;
            let column_count = config.width as usize;
            let column_bytes = (column_count * std::mem::size_of::<f32>()) as wgpu::BufferAddress;
            let column_stats_bytes =
                (column_count * std::mem::size_of::<ColumnStat>()) as wgpu::BufferAddress;

            let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("edge-texture-input-buffer"),
                contents: cast_slice(input),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });

            let gamma_buffer =
                create_storage_buffer(device, pixel_bytes, "edge-texture-gamma-buffer");
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
            if info.entropy_bins > MAX_ENTROPY_BINS {
                return Err(EdgeTextureGpuError::InvalidInput(
                    "entropy bins exceed shader MAX_BINS",
                ));
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

            let gaussian_shader_module =
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("edge-texture-gaussian-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(GAUSSIAN_SHADER)),
                });
            let gaussian_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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

            let gaussian_horizontal_bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("edge-texture-gaussian-horizontal-bind-group"),
                    layout: &gaussian_layout,
                    entries: &[
                        bind_storage(&gamma_buffer, 0),
                        bind_storage(&gaussian_temp_buffer, 1),
                        bind_storage(&kernel_buffer, 2),
                        bind_storage(&info_buffer, 3),
                    ],
                });
            let gaussian_vertical_bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                    bind_read_write_storage_entry(1),
                    bind_read_storage_entry(2),
                ],
                &[&gaussian_buffer, &entropy_buffer, &info_buffer],
            );

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("edge-texture-command-encoder"),
            });

            dispatch_1d(
                &mut encoder,
                &gamma_pipeline.pipeline,
                &gamma_pipeline.bind_group,
                ((config.pixel_count() as u32) + 255) / 256,
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
                ((config.width as u32) + 63) / 64,
            );
            dispatch_1d(
                &mut encoder,
                &entropy_pipeline.pipeline,
                &entropy_pipeline.bind_group,
                ((config.width as u32) + 63) / 64,
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

            queue.submit(Some(encoder.finish()));

            let column_stats =
                read_buffer::<ColumnStat>(device, &column_stats_readback, column_count)?;
            let entropy = read_buffer::<f32>(device, &entropy_readback, column_count)?;

            let mut mean_intensity = Vec::with_capacity(column_count);
            let mut grad_mean = Vec::with_capacity(column_count);
            let mut grad_variance = Vec::with_capacity(column_count);
            for stat in column_stats {
                mean_intensity.push(stat.mean_intensity);
                grad_mean.push(stat.grad_mean);
                grad_variance.push(stat.grad_variance);
            }

            Ok(GpuOutputs {
                mean_intensity,
                grad_mean,
                grad_variance,
                entropy,
            })
        }
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

        fn is_empty(&self) -> bool {
            self.width == 0 || self.height == 0
        }

        fn pixel_count(&self) -> usize {
            (self.width as usize) * (self.height as usize)
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

    struct LoadedSurface {
        width: u32,
        height: u32,
        pixels: Vec<f32>,
    }

    fn load_surface(image: &DynamicImage) -> LoadedSurface {
        let gray = image.to_luma8();
        let width = gray.width();
        let height = gray.height();
        let pixels = gray
            .into_raw()
            .into_iter()
            .map(|value| value as f32)
            .collect();
        LoadedSurface {
            width,
            height,
            pixels,
        }
    }

    struct GpuOutputs {
        mean_intensity: Vec<f32>,
        grad_mean: Vec<f32>,
        grad_variance: Vec<f32>,
        entropy: Vec<f32>,
    }

    impl GpuOutputs {
        fn empty(width: u32) -> Self {
            let len = width as usize;
            Self {
                mean_intensity: vec![0.0; len],
                grad_mean: vec![0.0; len],
                grad_variance: vec![0.0; len],
                entropy: vec![0.0; len],
            }
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
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shader_source)),
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
        pass.dispatch_workgroups(workgroups.max(1), 1, 1);
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
    ) -> Result<Vec<T>, EdgeTextureGpuError> {
        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device.poll(wgpu::Maintain::Wait);
        match receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(EdgeTextureGpuError::Execution(err.to_string())),
            Err(err) => return Err(EdgeTextureGpuError::Execution(err.to_string())),
        }
        let data = slice.get_mapped_range();
        let values: Vec<T> = cast_slice(&data).to_vec();
        drop(data);
        buffer.unmap();
        if values.len() != expected_len {
            return Err(EdgeTextureGpuError::Execution(
                "unexpected readback length".to_string(),
            ));
        }
        Ok(values)
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
}

#[cfg(not(feature = "edge-texture-gpu"))]
pub(crate) mod disabled {
    use image::DynamicImage;
    use thiserror::Error;

    use crate::doublepage::edge_texture::EdgeTextureConfig;

    #[derive(Debug, Error)]
    #[error("edge texture GPU feature is disabled")]
    pub struct EdgeTextureGpuError;

    #[derive(Debug, Clone)]
    pub struct EdgeTextureGpuOutputs {
        pub width: u32,
        pub mean_intensity: Vec<f32>,
        pub grad_mean: Vec<f32>,
        pub grad_variance: Vec<f32>,
        pub entropy: Vec<f32>,
    }

    pub struct EdgeTextureGpuAnalyzer;

    impl EdgeTextureGpuAnalyzer {
        pub fn new() -> Result<Self, EdgeTextureGpuError> {
            Err(EdgeTextureGpuError)
        }

        pub fn analyze(
            &self,
            _image: &DynamicImage,
            _config: EdgeTextureConfig,
        ) -> Result<EdgeTextureGpuOutputs, EdgeTextureGpuError> {
            Err(EdgeTextureGpuError)
        }
    }
}
