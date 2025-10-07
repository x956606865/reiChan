use std::cmp::{max, min};
use std::sync::{Arc, Mutex, OnceLock};

use image::{DynamicImage, ImageBuffer, Luma};
use serde::{Deserialize, Serialize};

use super::config::EdgeTextureThresholdOverrides;

#[cfg(not(feature = "edge-texture-gpu"))]
use super::edge_texture_gpu::disabled::{
    EdgeTextureGpuAnalyzer, EdgeTextureGpuError, EdgeTextureGpuOutputs,
};
#[cfg(feature = "edge-texture-gpu")]
use super::edge_texture_gpu::enabled::{
    EdgeTextureGpuAnalyzer, EdgeTextureGpuError, EdgeTextureGpuOutputs,
};

const EPSILON: f32 = 1e-5;
const EDGE_TEXTURE_ACCELERATOR_ENV: &str = "EDGE_TEXTURE_ACCELERATOR";
const SOBEL_X: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
const SOBEL_Y: [[f32; 3]; 3] = [[1.0, 2.0, 1.0], [0.0, 0.0, 0.0], [-1.0, -2.0, -1.0]];

static GPU_ANALYZER: OnceLock<Mutex<Option<Arc<EdgeTextureGpuAnalyzer>>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureConfig {
    pub gamma: f32,
    pub gaussian_kernel: u32,
    pub entropy_window: u32,
    pub entropy_bins: u32,
    pub white_threshold: f32,
    pub brightness_thresholds: [f32; 2],
    pub brightness_weight: f32,
    pub enable_dual_brightness: bool,
    pub left_search_ratio: f32,
    pub right_search_ratio: f32,
    pub center_search_ratio: f32,
    pub min_margin_ratio: f32,
    pub center_max_ratio: f32,
    pub score_weights: [f32; 3],
}

impl Default for EdgeTextureConfig {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            gaussian_kernel: 5,
            entropy_window: 15,
            entropy_bins: 32,
            white_threshold: 0.45,
            brightness_thresholds: [200.0, 40.0],
            brightness_weight: 0.4,
            enable_dual_brightness: true,
            left_search_ratio: 0.18,
            right_search_ratio: 0.18,
            center_search_ratio: 0.3,
            min_margin_ratio: 0.025,
            center_max_ratio: 0.06,
            score_weights: [0.4, 0.35, 0.25],
        }
    }
}

impl EdgeTextureConfig {
    pub fn apply_overrides(mut self, overrides: &EdgeTextureThresholdOverrides) -> Self {
        if let Some(value) = overrides.gamma {
            self.gamma = value;
        }
        if let Some(value) = overrides.gaussian_kernel {
            self.gaussian_kernel = value;
        }
        if let Some(value) = overrides.entropy_window {
            self.entropy_window = value;
        }
        if let Some(value) = overrides.entropy_bins {
            self.entropy_bins = value;
        }
        if let Some(value) = overrides.white_threshold {
            self.white_threshold = value;
        }
        if let Some(value) = overrides.brightness_thresholds {
            let bright = value[0].clamp(0.0, 255.0);
            let dark = value[1].clamp(0.0, bright);
            self.brightness_thresholds = [bright, dark];
        }
        if let Some(value) = overrides.brightness_weight {
            self.brightness_weight = value.clamp(0.0, 1.0);
        }
        if let Some(value) = overrides.enable_dual_brightness {
            self.enable_dual_brightness = value;
        }
        if let Some(value) = overrides.left_search_ratio {
            self.left_search_ratio = value;
        }
        if let Some(value) = overrides.right_search_ratio {
            self.right_search_ratio = value;
        }
        if let Some(value) = overrides.center_search_ratio {
            self.center_search_ratio = value;
        }
        if let Some(value) = overrides.min_margin_ratio {
            self.min_margin_ratio = value;
        }
        if let Some(value) = overrides.center_max_ratio {
            self.center_max_ratio = value;
        }
        if let Some(value) = overrides.score_weights {
            self.score_weights = value;
        }

        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginRegion {
    pub start_x: u32,
    pub end_x: u32,
    pub mean_score: f32,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureMetrics {
    pub width: u32,
    pub grad_mean: Vec<f32>,
    pub grad_variance: Vec<f32>,
    pub entropy: Vec<f32>,
    pub white_score: Vec<f32>,
    pub mean_intensity: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureNotes {
    pub left_limit: u32,
    pub right_start: u32,
    pub center_start: u32,
    pub center_end: u32,
    pub white_threshold: f32,
    pub brightness_thresholds: [f32; 2],
    pub brightness_weight: f32,
    pub dual_brightness_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureOutcome {
    pub split_x: Option<u32>,
    pub confidence: f32,
    pub left_margin: Option<MarginRegion>,
    pub right_margin: Option<MarginRegion>,
    pub center_band: Option<MarginRegion>,
    pub metrics: EdgeTextureMetrics,
    pub notes: EdgeTextureNotes,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeTextureAccelerator {
    Cpu,
    Gpu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeTextureAcceleratorPreference {
    Auto,
    Cpu,
    Gpu,
}

impl Default for EdgeTextureAcceleratorPreference {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone)]
pub struct EdgeTextureAnalysis {
    pub outcome: EdgeTextureOutcome,
    pub accelerator: EdgeTextureAccelerator,
}

pub fn analyze_edges_with_acceleration(
    image: &DynamicImage,
    config: EdgeTextureConfig,
    preference: EdgeTextureAcceleratorPreference,
) -> EdgeTextureAnalysis {
    match resolve_accelerator_directive(preference) {
        AcceleratorDirective::ForceCpu => cpu_analysis(image, config, EdgeTextureAccelerator::Cpu),
        AcceleratorDirective::MockGpu => cpu_analysis(image, config, EdgeTextureAccelerator::Gpu),
        AcceleratorDirective::ForceGpu | AcceleratorDirective::Auto => {
            match analyze_with_gpu(image, config) {
                Ok(outcome) => EdgeTextureAnalysis {
                    outcome,
                    accelerator: EdgeTextureAccelerator::Gpu,
                },
                Err(_) => cpu_analysis(image, config, EdgeTextureAccelerator::Cpu),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceleratorDirective {
    ForceCpu,
    ForceGpu,
    Auto,
    MockGpu,
}

fn resolve_accelerator_directive(
    preference: EdgeTextureAcceleratorPreference,
) -> AcceleratorDirective {
    if let Ok(raw) = std::env::var(EDGE_TEXTURE_ACCELERATOR_ENV) {
        let value = raw.trim().to_ascii_lowercase();
        return match value.as_str() {
            "cpu" => AcceleratorDirective::ForceCpu,
            "gpu" => AcceleratorDirective::ForceGpu,
            "auto" => AcceleratorDirective::Auto,
            "mock-gpu" if cfg!(test) => AcceleratorDirective::MockGpu,
            _ => AcceleratorDirective::Auto,
        };
    }

    match preference {
        EdgeTextureAcceleratorPreference::Cpu => AcceleratorDirective::ForceCpu,
        EdgeTextureAcceleratorPreference::Gpu => AcceleratorDirective::ForceGpu,
        EdgeTextureAcceleratorPreference::Auto => AcceleratorDirective::Auto,
    }
}

fn cpu_analysis(
    image: &DynamicImage,
    config: EdgeTextureConfig,
    accelerator: EdgeTextureAccelerator,
) -> EdgeTextureAnalysis {
    EdgeTextureAnalysis {
        outcome: analyze_edges(image, config),
        accelerator,
    }
}

fn analyze_with_gpu(
    image: &DynamicImage,
    config: EdgeTextureConfig,
) -> Result<EdgeTextureOutcome, EdgeTextureGpuError> {
    let analyzer = gpu_analyzer()?;
    let outputs = analyzer.analyze(image, config)?;
    Ok(build_outcome_from_gpu(config, outputs))
}

fn gpu_analyzer() -> Result<Arc<EdgeTextureGpuAnalyzer>, EdgeTextureGpuError> {
    let cell = GPU_ANALYZER.get_or_init(|| Mutex::new(None));

    {
        let guard = cell.lock().expect("gpu analyzer mutex poisoned");
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
    }

    let analyzer = Arc::new(EdgeTextureGpuAnalyzer::new()?);

    let mut guard = cell.lock().expect("gpu analyzer mutex poisoned");
    if let Some(existing) = guard.as_ref() {
        return Ok(Arc::clone(existing));
    }
    *guard = Some(Arc::clone(&analyzer));
    Ok(analyzer)
}

pub fn analyze_edges(image: &DynamicImage, config: EdgeTextureConfig) -> EdgeTextureOutcome {
    let gray = image.to_luma8();
    let width = gray.width();
    let height = gray.height();

    if width == 0 || height == 0 {
        return empty_outcome(width, &config);
    }

    let metrics = compute_cpu_metrics(&gray, config);
    build_outcome_from_metrics(config, metrics)
}

fn compute_cpu_metrics(
    gray: &ImageBuffer<Luma<u8>, Vec<u8>>,
    config: EdgeTextureConfig,
) -> EdgeTextureMetrics {
    let width = gray.width();
    let height = gray.height();

    let gamma_corrected = apply_gamma(gray, config.gamma);
    let mean_intensity =
        compute_column_mean_intensity(width as usize, height as usize, &gamma_corrected);
    let kernel_size = ensure_odd(config.gaussian_kernel);
    let blurred = gaussian_blur(
        width as usize,
        height as usize,
        &gamma_corrected,
        kernel_size,
    );
    let grad_mag = sobel_magnitude(width as usize, height as usize, &blurred);
    let (grad_mean, grad_variance) =
        compute_column_stats(width as usize, height as usize, &grad_mag);
    let entropy = compute_entropy(
        width as usize,
        height as usize,
        &blurred,
        ensure_odd(config.entropy_window) as usize,
        config.entropy_bins.max(1) as usize,
    );

    let grad_mean_norm = normalize(&grad_mean);
    let grad_variance_norm = normalize(&grad_variance);
    let entropy_norm = normalize(&entropy);
    let white_score = compute_white_score(
        &grad_mean_norm,
        &grad_variance_norm,
        &entropy_norm,
        config.score_weights,
    );

    EdgeTextureMetrics {
        width,
        grad_mean,
        grad_variance,
        entropy,
        white_score,
        mean_intensity,
    }
}

fn build_outcome_from_metrics(
    config: EdgeTextureConfig,
    metrics: EdgeTextureMetrics,
) -> EdgeTextureOutcome {
    let width = metrics.width;

    let mut left_limit = ((width as f32) * config.left_search_ratio).floor() as i32;
    if left_limit <= 0 {
        left_limit = 1;
    }
    if left_limit as u32 > width {
        left_limit = width as i32;
    }

    let mut right_start =
        width.saturating_sub(((width as f32) * config.right_search_ratio).floor() as u32);
    if right_start >= width {
        right_start = width.saturating_sub(1);
    }

    let center_half = (config.center_search_ratio * 0.5).clamp(0.0, 0.5);
    let mut center_start = ((width as f32) * (0.5 - center_half)).floor() as i32;
    let mut center_end = ((width as f32) * (0.5 + center_half)).ceil() as i32;
    let max_start = width.saturating_sub(1) as i32;
    center_start = center_start.clamp(0, max_start);
    center_end = center_end.clamp(center_start + 1, width as i32);

    let min_margin_width = max(3, ((width as f32) * config.min_margin_ratio).floor() as u32);
    let center_max_width = max(3, ((width as f32) * config.center_max_ratio).floor() as u32);

    let white_score = &metrics.white_score;
    let mean_intensity = &metrics.mean_intensity;

    let left_region = if left_limit > 0 {
        let limit = left_limit as usize;
        let slice_limit = min(limit, white_score.len());
        find_margin(
            &white_score[..slice_limit],
            &mean_intensity[..slice_limit],
            config.white_threshold,
            config.brightness_thresholds,
            config.brightness_weight,
            config.enable_dual_brightness,
            min_margin_width,
            SearchDirection::Left,
        )
    } else {
        None
    };

    let right_region = if right_start < width {
        let offset = right_start as usize;
        let region = find_margin(
            &white_score[offset..],
            &mean_intensity[offset..],
            config.white_threshold,
            config.brightness_thresholds,
            config.brightness_weight,
            config.enable_dual_brightness,
            min_margin_width,
            SearchDirection::Right,
        );
        region.map(|mut r| {
            r.start_x += right_start;
            r.end_x += right_start;
            r
        })
    } else {
        None
    };

    let center_region = if (center_end - center_start) as usize <= white_score.len() {
        let start = center_start as usize;
        let end = center_end as usize;
        if start < end {
            let region = find_center_band(
                &white_score[start..end],
                config.white_threshold,
                center_max_width,
            );
            region.map(|mut r| {
                r.start_x += center_start as u32;
                r.end_x += center_start as u32;
                r
            })
        } else {
            None
        }
    } else {
        None
    };

    let split_x = center_region
        .as_ref()
        .map(|region| ((region.start_x + region.end_x) / 2) as u32);

    let confidence = combine_confidence(
        center_region.as_ref(),
        left_region.as_ref(),
        right_region.as_ref(),
    );

    let notes = EdgeTextureNotes {
        left_limit: left_limit as u32,
        right_start,
        center_start: center_start as u32,
        center_end: center_end as u32,
        white_threshold: config.white_threshold,
        brightness_thresholds: config.brightness_thresholds,
        brightness_weight: config.brightness_weight,
        dual_brightness_enabled: config.enable_dual_brightness,
    };

    EdgeTextureOutcome {
        split_x,
        confidence,
        left_margin: left_region,
        right_margin: right_region,
        center_band: center_region,
        metrics,
        notes,
    }
}

fn build_outcome_from_gpu(
    config: EdgeTextureConfig,
    outputs: EdgeTextureGpuOutputs,
) -> EdgeTextureOutcome {
    let EdgeTextureGpuOutputs {
        width,
        mean_intensity,
        grad_mean,
        grad_variance,
        entropy,
    } = outputs;

    if width == 0 {
        return empty_outcome(width, &config);
    }

    let grad_mean_norm = normalize(&grad_mean);
    let grad_variance_norm = normalize(&grad_variance);
    let entropy_norm = normalize(&entropy);
    let white_score = compute_white_score(
        &grad_mean_norm,
        &grad_variance_norm,
        &entropy_norm,
        config.score_weights,
    );

    let metrics = EdgeTextureMetrics {
        width,
        grad_mean,
        grad_variance,
        entropy,
        white_score,
        mean_intensity,
    };

    build_outcome_from_metrics(config, metrics)
}

enum SearchDirection {
    Left,
    Right,
}

fn find_margin(
    scores: &[f32],
    brightness: &[f32],
    threshold: f32,
    brightness_thresholds: [f32; 2],
    brightness_weight: f32,
    dual_brightness_enabled: bool,
    min_width: u32,
    direction: SearchDirection,
) -> Option<MarginRegion> {
    let len = scores.len();
    if len == 0 {
        return None;
    }

    match direction {
        SearchDirection::Left => {
            let mut end = 0usize;
            let mut score_sum = 0.0f32;
            let mut brightness_conf_sum = 0.0f32;

            while end < len {
                let value = scores[end];
                let intensity = brightness.get(end).copied().unwrap_or(0.0);
                if let Some(column_conf) = evaluate_margin_column(
                    value,
                    intensity,
                    threshold,
                    brightness_thresholds,
                    dual_brightness_enabled,
                ) {
                    score_sum += value;
                    if dual_brightness_enabled {
                        brightness_conf_sum += column_conf;
                    }
                    end += 1;
                } else {
                    break;
                }
            }

            if end as u32 >= min_width && end > 0 {
                let mean = score_sum / end as f32;
                let brightness_conf = if dual_brightness_enabled {
                    brightness_conf_sum / end as f32
                } else {
                    0.0
                };
                let confidence = compute_margin_confidence(
                    mean,
                    threshold,
                    brightness_weight,
                    brightness_conf,
                    dual_brightness_enabled,
                );
                Some(MarginRegion {
                    start_x: 0,
                    end_x: end as u32 - 1,
                    mean_score: mean,
                    confidence,
                })
            } else {
                None
            }
        }
        SearchDirection::Right => {
            let mut idx = len as isize - 1;
            let mut count = 0usize;
            let mut score_sum = 0.0f32;
            let mut brightness_conf_sum = 0.0f32;

            while idx >= 0 {
                let index = idx as usize;
                let value = scores[index];
                let intensity = brightness.get(index).copied().unwrap_or(0.0);
                if let Some(column_conf) = evaluate_margin_column(
                    value,
                    intensity,
                    threshold,
                    brightness_thresholds,
                    dual_brightness_enabled,
                ) {
                    score_sum += value;
                    if dual_brightness_enabled {
                        brightness_conf_sum += column_conf;
                    }
                    count += 1;
                    idx -= 1;
                } else {
                    break;
                }
            }

            if count as u32 >= min_width && count > 0 {
                let run_start = len - count;
                let mean = score_sum / count as f32;
                let brightness_conf = if dual_brightness_enabled {
                    brightness_conf_sum / count as f32
                } else {
                    0.0
                };
                let confidence = compute_margin_confidence(
                    mean,
                    threshold,
                    brightness_weight,
                    brightness_conf,
                    dual_brightness_enabled,
                );
                Some(MarginRegion {
                    start_x: run_start as u32,
                    end_x: (len - 1) as u32,
                    mean_score: mean,
                    confidence,
                })
            } else {
                None
            }
        }
    }
}

fn compute_margin_confidence(
    mean_score: f32,
    threshold: f32,
    brightness_weight: f32,
    brightness_conf: f32,
    dual_brightness_enabled: bool,
) -> f32 {
    let base_conf = (1.0 - (mean_score / (threshold + EPSILON)).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    if dual_brightness_enabled {
        let weight = brightness_weight.clamp(0.0, 1.0);
        ((1.0 - weight) * base_conf + weight * brightness_conf).clamp(0.0, 1.0)
    } else {
        base_conf
    }
}

fn evaluate_margin_column(
    score: f32,
    brightness: f32,
    threshold: f32,
    brightness_thresholds: [f32; 2],
    dual_brightness_enabled: bool,
) -> Option<f32> {
    if dual_brightness_enabled {
        if score > threshold {
            return None;
        }

        let clamped = brightness.clamp(0.0, 255.0);
        let class = classify_brightness(clamped, brightness_thresholds);
        if matches!(class, BrightnessClass::Neutral) {
            return None;
        }
        Some(compute_brightness_confidence(
            clamped,
            class,
            brightness_thresholds,
        ))
    } else if score <= threshold {
        Some(0.0)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrightnessClass {
    Bright,
    Dark,
    Neutral,
}

fn classify_brightness(value: f32, thresholds: [f32; 2]) -> BrightnessClass {
    let bright_min = thresholds[0].clamp(0.0, 255.0);
    let dark_max = thresholds[1].clamp(0.0, bright_min);

    if value >= bright_min {
        BrightnessClass::Bright
    } else if value <= dark_max {
        BrightnessClass::Dark
    } else {
        BrightnessClass::Neutral
    }
}

fn compute_brightness_confidence(value: f32, class: BrightnessClass, thresholds: [f32; 2]) -> f32 {
    match class {
        BrightnessClass::Bright => {
            let bright_min = thresholds[0].clamp(0.0, 255.0);
            let denominator = (255.0 - bright_min).max(EPSILON);
            ((value - bright_min) / denominator).clamp(0.0, 1.0)
        }
        BrightnessClass::Dark => {
            let bright_min = thresholds[0].clamp(0.0, 255.0);
            let dark_max = thresholds[1].clamp(0.0, bright_min);
            if dark_max <= EPSILON {
                1.0
            } else {
                ((dark_max - value) / dark_max).clamp(0.0, 1.0)
            }
        }
        BrightnessClass::Neutral => 0.0,
    }
}

fn find_center_band(scores: &[f32], threshold: f32, max_width: u32) -> Option<MarginRegion> {
    let mut best: Option<MarginRegion> = None;
    let mut run_start: Option<usize> = None;
    for (idx, &value) in scores.iter().enumerate() {
        if value <= threshold {
            if run_start.is_none() {
                run_start = Some(idx);
            }
        } else if let Some(start) = run_start.take() {
            let end = idx - 1;
            let width = end - start + 1;
            if width as u32 <= max_width {
                let segment = &scores[start..=end];
                let mean = segment.iter().sum::<f32>() / segment.len() as f32;
                let confidence = 1.0 - (mean / (threshold + 1e-5)).clamp(0.0, 1.0);
                let candidate = MarginRegion {
                    start_x: start as u32,
                    end_x: end as u32,
                    mean_score: mean,
                    confidence,
                };
                if best
                    .map(|region| candidate.confidence > region.confidence)
                    .unwrap_or(true)
                {
                    best = Some(candidate);
                }
            }
        }
    }

    if let Some(start) = run_start {
        let end = scores.len().saturating_sub(1);
        if end >= start {
            let width = end - start + 1;
            if width as u32 <= max_width {
                let segment = &scores[start..=end];
                let mean = segment.iter().sum::<f32>() / segment.len() as f32;
                let confidence = 1.0 - (mean / (threshold + 1e-5)).clamp(0.0, 1.0);
                let candidate = MarginRegion {
                    start_x: start as u32,
                    end_x: end as u32,
                    mean_score: mean,
                    confidence,
                };
                if best
                    .as_ref()
                    .map(|region| candidate.confidence > region.confidence)
                    .unwrap_or(true)
                {
                    best = Some(candidate);
                }
            }
        }
    }

    best
}

fn apply_gamma(gray: &ImageBuffer<Luma<u8>, Vec<u8>>, gamma: f32) -> Vec<f32> {
    if (gamma - 1.0).abs() <= EPSILON {
        return gray
            .pixels()
            .map(|pixel| pixel[0] as f32)
            .collect::<Vec<f32>>();
    }

    let gamma = gamma.max(EPSILON);
    let inv = 1.0 / gamma;
    let mut lut = [0f32; 256];
    for value in 0..=255 {
        let normalized = (value as f32) / 255.0;
        let corrected = normalized.powf(inv);
        lut[value as usize] = (corrected * 255.0).clamp(0.0, 255.0);
    }

    gray.pixels()
        .map(|pixel| lut[pixel[0] as usize])
        .collect::<Vec<f32>>()
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
            for (idx, weight) in kernel.iter().enumerate() {
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
            for (idx, weight) in kernel.iter().enumerate() {
                let offset = idx as isize - radius;
                let sample_y = clamp_i32(y as isize + offset, 0, (height - 1) as isize) as usize;
                acc += horizontal[sample_y * width + x] * weight;
            }
            output[y * width + x] = acc;
        }
    }
    output
}

pub(crate) fn build_gaussian_kernel(size: u32) -> Vec<f32> {
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
        let weight = (-((dist * dist) / two_sigma_sq)).exp();
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
                    gx += value * SOBEL_X[ky][kx];
                    gy += value * SOBEL_Y[ky][kx];
                }
            }
            output[y * width + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    output
}

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

fn normalize(values: &[f32]) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    for &value in values {
        if value < min_val {
            min_val = value;
        }
        if value > max_val {
            max_val = value;
        }
    }
    let range = max_val - min_val;
    if range <= EPSILON {
        return vec![0.0; values.len()];
    }
    values
        .iter()
        .map(|&value| ((value - min_val) / range).clamp(0.0, 1.0))
        .collect()
}

fn compute_white_score(
    grad_mean: &[f32],
    grad_variance: &[f32],
    entropy: &[f32],
    weights: [f32; 3],
) -> Vec<f32> {
    let len = grad_mean.len().min(grad_variance.len()).min(entropy.len());
    let mut output = Vec::with_capacity(len);
    for idx in 0..len {
        let value = ((1.0 - grad_mean[idx]).clamp(0.0, 1.0) * weights[0])
            + ((1.0 - grad_variance[idx]).clamp(0.0, 1.0) * weights[1])
            + ((1.0 - entropy[idx]).clamp(0.0, 1.0) * weights[2]);
        output.push(value.clamp(0.0, 1.0));
    }
    output
}

fn combine_confidence(
    center: Option<&MarginRegion>,
    left: Option<&MarginRegion>,
    right: Option<&MarginRegion>,
) -> f32 {
    let center_conf = center.map(|region| region.confidence).unwrap_or(0.0);
    let margin_conf = match (left, right) {
        (Some(l), Some(r)) => (l.confidence + r.confidence) * 0.5,
        (Some(l), None) => l.confidence,
        (None, Some(r)) => r.confidence,
        (None, None) => 0.0,
    };

    let combined = if center_conf > 0.0 && margin_conf > 0.0 {
        center_conf * margin_conf
    } else if center_conf > 0.0 {
        center_conf
    } else {
        margin_conf
    };
    combined.clamp(0.0, 1.0)
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

fn empty_outcome(width: u32, config: &EdgeTextureConfig) -> EdgeTextureOutcome {
    EdgeTextureOutcome {
        split_x: None,
        confidence: 0.0,
        left_margin: None,
        right_margin: None,
        center_band: None,
        metrics: EdgeTextureMetrics {
            width,
            grad_mean: vec![0.0; width as usize],
            grad_variance: vec![0.0; width as usize],
            entropy: vec![0.0; width as usize],
            white_score: vec![0.0; width as usize],
            mean_intensity: vec![0.0; width as usize],
        },
        notes: EdgeTextureNotes {
            left_limit: 0,
            right_start: width,
            center_start: width / 2,
            center_end: width / 2,
            white_threshold: config.white_threshold,
            brightness_thresholds: config.brightness_thresholds,
            brightness_weight: config.brightness_weight,
            dual_brightness_enabled: config.enable_dual_brightness,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_python_reference() {
        let config = EdgeTextureConfig::default();
        assert_eq!(config.gamma, 1.0);
        assert_eq!(config.gaussian_kernel, 5);
        assert_eq!(config.entropy_window, 15);
        assert_eq!(config.entropy_bins, 32);
        assert!((config.white_threshold - 0.45).abs() < f32::EPSILON);
        assert!((config.left_search_ratio - 0.18).abs() < f32::EPSILON);
        assert!((config.right_search_ratio - 0.18).abs() < f32::EPSILON);
        assert!((config.center_search_ratio - 0.3).abs() < f32::EPSILON);
        assert!((config.min_margin_ratio - 0.025).abs() < f32::EPSILON);
        assert!((config.center_max_ratio - 0.06).abs() < f32::EPSILON);
        assert_eq!(config.score_weights, [0.4, 0.35, 0.25]);
        assert_eq!(config.brightness_thresholds, [200.0, 40.0]);
        assert!((config.brightness_weight - 0.4).abs() < f32::EPSILON);
        assert!(config.enable_dual_brightness);
    }

    #[test]
    fn find_margin_detects_left_segment_with_dual_brightness() {
        let scores = vec![0.28, 0.3, 0.52, 0.6];
        let brightness = vec![220.0, 215.0, 180.0, 175.0];
        let result = find_margin(
            &scores,
            &brightness,
            0.45,
            [200.0, 40.0],
            0.4,
            true,
            2,
            SearchDirection::Left,
        )
        .expect("left margin");
        assert_eq!(result.start_x, 0);
        assert_eq!(result.end_x, 1);
        assert!(result.confidence > 0.2);
    }

    #[test]
    fn find_margin_detects_right_segment_with_dual_brightness() {
        let scores = vec![0.55, 0.48, 0.3, 0.2];
        let brightness = vec![80.0, 60.0, 35.0, 20.0];
        let result = find_margin(
            &scores,
            &brightness,
            0.45,
            [200.0, 40.0],
            0.4,
            true,
            2,
            SearchDirection::Right,
        )
        .expect("right margin");
        assert_eq!(result.start_x, 2);
        assert_eq!(result.end_x, 3);
        assert!(result.confidence > 0.2);
    }

    #[test]
    fn find_margin_supports_single_threshold_when_disabled() {
        let scores = vec![0.2, 0.3, 0.32, 0.4, 0.5, 0.6];
        let brightness = vec![180.0; scores.len()];
        let result = find_margin(
            &scores,
            &brightness,
            0.45,
            [200.0, 40.0],
            0.4,
            false,
            3,
            SearchDirection::Left,
        )
        .expect("legacy left margin");
        assert_eq!(result.start_x, 0);
        assert_eq!(result.end_x, 3);
        assert!(result.mean_score < 0.35);
        assert!(result.confidence > 0.2);
    }

    #[test]
    fn find_center_band_prefers_high_confidence_sequence() {
        let scores = vec![0.6, 0.2, 0.2, 0.6, 0.3, 0.3, 0.6];
        let result = find_center_band(&scores, 0.45, 3).expect("center band");
        assert_eq!(result.start_x, 1);
        assert_eq!(result.end_x, 2);
        assert!(result.confidence > 0.2);
    }

    #[test]
    fn analyze_edges_returns_metrics_sized_to_width() {
        let gray = ImageBuffer::from_pixel(16, 8, Luma([128u8]));
        let image = DynamicImage::ImageLuma8(gray);
        let outcome = analyze_edges(&image, EdgeTextureConfig::default());
        assert_eq!(outcome.metrics.width, 16);
        assert_eq!(outcome.metrics.grad_mean.len(), 16);
        assert_eq!(outcome.metrics.grad_variance.len(), 16);
        assert_eq!(outcome.metrics.entropy.len(), 16);
        assert_eq!(outcome.metrics.white_score.len(), 16);
        assert_eq!(outcome.metrics.mean_intensity.len(), 16);
        assert!(outcome.left_margin.is_none());
        assert!(outcome.right_margin.is_none());
        assert!(outcome.center_band.is_none());
        assert_eq!(outcome.split_x, None);
    }

    #[test]
    fn analyze_edges_with_acceleration_respects_cpu_preference() {
        let gray = ImageBuffer::from_pixel(8, 4, Luma([200u8]));
        let image = DynamicImage::ImageLuma8(gray);
        let config = EdgeTextureConfig::default();

        let baseline = analyze_edges(&image, config);
        let analysis =
            analyze_edges_with_acceleration(&image, config, EdgeTextureAcceleratorPreference::Cpu);

        assert_eq!(analysis.accelerator, EdgeTextureAccelerator::Cpu);
        assert_eq!(
            analysis.outcome.metrics.mean_intensity,
            baseline.metrics.mean_intensity
        );
    }

    #[test]
    fn analyze_edges_with_acceleration_supports_mock_gpu() {
        let gray = ImageBuffer::from_pixel(4, 4, Luma([255u8]));
        let image = DynamicImage::ImageLuma8(gray);
        let config = EdgeTextureConfig::default();

        std::env::set_var("EDGE_TEXTURE_ACCELERATOR", "mock-gpu");

        let analysis =
            analyze_edges_with_acceleration(&image, config, EdgeTextureAcceleratorPreference::Auto);

        std::env::remove_var("EDGE_TEXTURE_ACCELERATOR");

        assert_eq!(analysis.accelerator, EdgeTextureAccelerator::Gpu);
    }
}
