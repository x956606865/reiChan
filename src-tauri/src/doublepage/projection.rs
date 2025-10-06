use image::{ImageBuffer, Luma};

use super::ProjectionConfig;

#[derive(Debug, Clone)]
pub struct ProjectionOutcome {
    pub split_x: Option<u32>,
    pub confidence: f32,
    pub imbalance: f32,
    pub edge_margin: u32,
    pub total_mass: f32,
}

impl ProjectionOutcome {
    fn fallback() -> Self {
        Self {
            split_x: None,
            confidence: 0.0,
            imbalance: 0.0,
            edge_margin: 0,
            total_mass: 0.0,
        }
    }
}

pub fn analyze_projection(
    mask: &ImageBuffer<Luma<u8>, Vec<u8>>,
    config: ProjectionConfig,
) -> ProjectionOutcome {
    let (width, height) = mask.dimensions();
    if width == 0 || height == 0 {
        return ProjectionOutcome::fallback();
    }

    let mut projection: Vec<f32> = vec![0.0; width as usize];
    for (x, _, pixel) in mask.enumerate_pixels() {
        if pixel[0] > 0 {
            projection[x as usize] += 1.0;
        }
    }

    if projection.iter().all(|&value| value <= 0.0) {
        return ProjectionOutcome::fallback();
    }

    let sigma = (width as f32 / 200.0).max(1.0);
    let smoothed = gaussian_smooth(&projection, sigma);

    let mut edge_margin = (width as f32 * config.edge_exclusion_ratio).floor() as isize;
    if edge_margin < 5 {
        edge_margin = 5;
    }

    if edge_margin * 2 >= width as isize {
        return ProjectionOutcome::fallback();
    }

    let start = edge_margin as usize;
    let end = (width as isize - edge_margin) as usize;
    if start >= end {
        return ProjectionOutcome::fallback();
    }

    let search_slice = &smoothed[start..end];
    if search_slice.is_empty() {
        return ProjectionOutcome::fallback();
    }

    let candidates = collect_valleys(&smoothed, start, end);

    let mut candidate_indices = candidates;
    if candidate_indices.is_empty() {
        let mut min_idx = start;
        let mut min_value = search_slice[0];
        for (offset, value) in search_slice.iter().enumerate() {
            if *value < min_value {
                min_value = *value;
                min_idx = start + offset;
            }
        }
        candidate_indices.push(min_idx);
    }

    let cumulative = cumulative_sum(&smoothed);
    let total_mass = *cumulative.last().unwrap_or(&0.0);
    if total_mass <= f32::EPSILON {
        return ProjectionOutcome::fallback();
    }

    let mut max_val = 0.0f32;
    for value in search_slice.iter().copied() {
        if value > max_val {
            max_val = value;
        }
    }

    let mut best_idx = candidate_indices[0];
    let mut best_score = f32::MAX;
    for idx in candidate_indices.into_iter() {
        let valley_value = smoothed[idx];
        let left_mass = cumulative[idx];
        let balance_score = ((left_mass / (total_mass + 1e-6)) - 0.5).abs();
        let depth_score = valley_value / (max_val + 1e-6);
        let score = balance_score + 0.1 * depth_score;
        if score < best_score {
            best_score = score;
            best_idx = idx;
        }
    }

    let valley_value = smoothed[best_idx];
    let confidence = if max_val <= f32::EPSILON {
        0.0
    } else {
        ((max_val - valley_value) / (max_val + 1e-6)).clamp(0.0, 1.0)
    };

    let left_mass = cumulative[best_idx];
    let right_mass = total_mass - left_mass;
    let imbalance = (left_mass - right_mass).abs() / (total_mass + 1e-6);

    ProjectionOutcome {
        split_x: Some(best_idx as u32),
        confidence,
        imbalance,
        edge_margin: edge_margin as u32,
        total_mass,
    }
}

fn gaussian_smooth(data: &[f32], sigma: f32) -> Vec<f32> {
    if data.is_empty() {
        return Vec::new();
    }

    let radius = (sigma * 3.0).ceil() as isize;
    if radius <= 0 {
        return data.to_vec();
    }

    let mut kernel: Vec<f32> = Vec::with_capacity((radius * 2 + 1) as usize);
    let sigma_sq = 2.0 * sigma * sigma;
    let mut kernel_sum = 0.0f32;
    for i in -radius..=radius {
        let value = (-((i as f32).powi(2)) / sigma_sq).exp();
        kernel.push(value);
        kernel_sum += value;
    }

    if kernel_sum <= f32::EPSILON {
        return data.to_vec();
    }

    for value in kernel.iter_mut() {
        *value /= kernel_sum;
    }

    let len = data.len() as isize;
    let mut output = vec![0.0f32; len as usize];
    for idx in 0..len {
        let mut acc = 0.0f32;
        for (kernel_offset, kernel_value) in kernel.iter().enumerate() {
            let offset = kernel_offset as isize - radius;
            let sample_idx = (idx + offset).clamp(0, len - 1) as usize;
            acc += data[sample_idx] * *kernel_value;
        }
        output[idx as usize] = acc;
    }

    output
}

fn cumulative_sum(data: &[f32]) -> Vec<f32> {
    let mut cumulative = Vec::with_capacity(data.len());
    let mut sum = 0.0f32;
    for value in data.iter().copied() {
        sum += value;
        cumulative.push(sum);
    }
    cumulative
}

fn collect_valleys(data: &[f32], start: usize, end: usize) -> Vec<usize> {
    let mut valleys = Vec::new();
    if data.len() < 3 {
        return valleys;
    }
    let upper = end.min(data.len() - 1);
    for idx in start.max(1)..upper {
        if data[idx] <= data[idx - 1] && data[idx] <= data[idx + 1] {
            valleys.push(idx);
        }
    }
    valleys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaussian_smooth_preserves_length() {
        let data = vec![0.0f32, 1.0, 0.0, 1.0, 0.0];
        let output = gaussian_smooth(&data, 1.0);
        assert_eq!(output.len(), data.len());
    }

    #[test]
    fn collect_valleys_identifies_local_minima() {
        let data = vec![3.0, 2.0, 3.0, 1.0, 4.0];
        let valleys = collect_valleys(&data, 0, data.len());
        assert_eq!(valleys, vec![1, 3]);
    }
}
