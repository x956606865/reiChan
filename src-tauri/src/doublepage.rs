use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use image::{DynamicImage, GenericImageView, ImageBuffer, Luma};
use imageproc::contrast::{equalize_histogram, otsu_level};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitCommandOptions {
    pub directory: PathBuf,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub thresholds: Option<SplitThresholdOverrides>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitThresholdOverrides {
    #[serde(default)]
    pub cover_content_ratio: Option<f32>,
    #[serde(default)]
    pub confidence_threshold: Option<f32>,
    #[serde(default)]
    pub edge_exclusion_ratio: Option<f32>,
    #[serde(default)]
    pub min_foreground_ratio: Option<f32>,
    #[serde(default)]
    pub padding_ratio: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitCommandOutcome {
    pub analyzed_files: usize,
    pub emitted_files: usize,
    pub skipped_files: usize,
    pub split_pages: usize,
    pub cover_trims: usize,
    pub fallback_splits: usize,
    pub workspace_directory: Option<PathBuf>,
    pub report_path: Option<PathBuf>,
    pub items: Vec<SplitItemReport>,
    pub warnings: Vec<String>,
}

pub const SPLIT_PROGRESS_EVENT: &str = "doublepage-split-progress";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SplitProgress {
    pub total_files: usize,
    pub processed_files: usize,
    pub current_file: Option<PathBuf>,
    pub stage: SplitProgressStage,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SplitProgressStage {
    Initializing,
    Processing,
    Completed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitItemReport {
    pub source: PathBuf,
    pub mode: SplitMode,
    pub split_x: Option<u32>,
    pub confidence: f32,
    pub content_width_ratio: f32,
    pub outputs: Vec<PathBuf>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SplitMode {
    Skip,
    CoverTrim,
    Split,
    FallbackCenter,
}

#[derive(Debug, Clone, Copy)]
pub struct SplitThresholds {
    pub min_aspect_ratio: f32,
    pub padding_ratio: f32,
    pub confidence_threshold: f32,
    pub cover_content_ratio: f32,
    pub edge_exclusion_ratio: f32,
    pub min_foreground_ratio: f32,
}

impl Default for SplitThresholds {
    fn default() -> Self {
        Self {
            min_aspect_ratio: 1.2,
            padding_ratio: 0.015,
            confidence_threshold: 0.1,
            cover_content_ratio: 0.45,
            edge_exclusion_ratio: 0.12,
            min_foreground_ratio: 0.01,
        }
    }
}

impl SplitThresholds {
    pub fn with_overrides(self, overrides: &SplitThresholdOverrides) -> Self {
        Self {
            cover_content_ratio: overrides
                .cover_content_ratio
                .unwrap_or(self.cover_content_ratio),
            confidence_threshold: overrides
                .confidence_threshold
                .unwrap_or(self.confidence_threshold),
            edge_exclusion_ratio: overrides
                .edge_exclusion_ratio
                .unwrap_or(self.edge_exclusion_ratio),
            min_foreground_ratio: overrides
                .min_foreground_ratio
                .unwrap_or(self.min_foreground_ratio),
            padding_ratio: overrides.padding_ratio.unwrap_or(self.padding_ratio),
            ..self
        }
    }
}

#[derive(Debug)]
pub enum SplitError {
    DirectoryNotFound(PathBuf),
    EmptyDirectory(PathBuf),
    Io(io::Error),
    Image(image::ImageError),
    ReportSerialization(serde_json::Error),
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::DirectoryNotFound(path) => {
                write!(f, "directory not found: {}", path.display())
            }
            SplitError::EmptyDirectory(path) => {
                write!(f, "no supported images found in {}", path.display())
            }
            SplitError::Io(err) => write!(f, "I/O error: {}", err),
            SplitError::Image(err) => write!(f, "image error: {}", err),
            SplitError::ReportSerialization(err) => {
                write!(f, "report serialization failed: {}", err)
            }
        }
    }
}

impl From<io::Error> for SplitError {
    fn from(value: io::Error) -> Self {
        SplitError::Io(value)
    }
}

impl From<image::ImageError> for SplitError {
    fn from(value: image::ImageError) -> Self {
        SplitError::Image(value)
    }
}

impl From<serde_json::Error> for SplitError {
    fn from(value: serde_json::Error) -> Self {
        SplitError::ReportSerialization(value)
    }
}

const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];

pub fn prepare_split<'a>(
    options: SplitCommandOptions,
    progress: Option<&'a mut dyn FnMut(SplitProgress)>,
) -> Result<SplitCommandOutcome, SplitError> {
    prepare_split_internal(options, progress)
}

fn prepare_split_internal<'a>(
    options: SplitCommandOptions,
    mut progress: Option<&'a mut dyn FnMut(SplitProgress)>,
) -> Result<SplitCommandOutcome, SplitError> {
    let SplitCommandOptions {
        directory,
        dry_run,
        overwrite,
        thresholds: thresholds_override,
    } = options;

    if !directory.exists() || !directory.is_dir() {
        return Err(SplitError::DirectoryNotFound(directory));
    }

    let thresholds = if let Some(overrides) = thresholds_override.as_ref() {
        SplitThresholds::default().with_overrides(overrides)
    } else {
        SplitThresholds::default()
    };

    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
            if SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
                entries.push(path);
            }
        }
    }

    if entries.is_empty() {
        return Err(SplitError::EmptyDirectory(directory));
    }

    entries.sort();

    let total_files = entries.len();
    let workspace_directory = if dry_run {
        None
    } else {
        Some(create_workspace(&directory, overwrite)?)
    };

    let mut analyzed_files = 0usize;
    let mut emitted_files = 0usize;
    let mut skipped_files = 0usize;
    let mut split_pages = 0usize;
    let mut cover_trims = 0usize;
    let mut fallback_splits = 0usize;
    let mut warnings: Vec<String> = Vec::new();
    let mut items: Vec<SplitItemReport> = Vec::new();
    let mut processed_files = 0usize;

    emit_progress(
        &mut progress,
        SplitProgress {
            total_files,
            processed_files,
            current_file: None,
            stage: SplitProgressStage::Initializing,
        },
    );

    for path in entries.iter() {
        analyzed_files += 1;

        let image = match image::open(path) {
            Ok(img) => img,
            Err(err) => {
                warnings.push(format!("failed to read {}: {}", path.display(), err));
                skipped_files += 1;
                processed_files += 1;
                emit_progress(
                    &mut progress,
                    SplitProgress {
                        total_files,
                        processed_files,
                        current_file: Some(path.clone()),
                        stage: SplitProgressStage::Processing,
                    },
                );
                continue;
            }
        };

        match process_image(&image, path, thresholds) {
            ProcessResult::Skip(meta) => {
                skipped_files += 1;
                let outputs = if let Some(workspace) = &workspace_directory {
                    let target = workspace.join(path.file_name().unwrap());
                    if let Err(err) = fs::copy(path, &target) {
                        warnings.push(format!(
                            "failed to copy {} into workspace: {}",
                            path.display(),
                            err
                        ));
                    } else {
                        emitted_files += 1;
                    }
                    vec![target]
                } else {
                    Vec::new()
                };

                items.push(SplitItemReport {
                    source: path.clone(),
                    mode: SplitMode::Skip,
                    split_x: None,
                    confidence: 0.0,
                    content_width_ratio: meta.content_width_ratio,
                    outputs,
                    metadata: meta.into_metadata_json(),
                });
            }
            ProcessResult::CoverTrim { image: cover, meta } => {
                cover_trims += 1;
                let (outputs, emitted) = if let Some(workspace) = &workspace_directory {
                    let filename = format!(
                        "{}_cover{}",
                        path.file_stem().unwrap().to_string_lossy(),
                        path.extension()
                            .map(|ext| format!(".{}", ext.to_string_lossy()))
                            .unwrap_or_else(String::new)
                    );
                    let target = workspace.join(&filename);
                    if let Err(err) = save_image(&cover, &target) {
                        warnings.push(format!("failed to write {}: {}", target.display(), err));
                        (Vec::new(), 0)
                    } else {
                        (vec![target], 1)
                    }
                } else {
                    (Vec::new(), 0)
                };
                emitted_files += emitted;

                items.push(SplitItemReport {
                    source: path.clone(),
                    mode: SplitMode::CoverTrim,
                    split_x: None,
                    confidence: 1.0,
                    content_width_ratio: meta.content_width_ratio,
                    outputs,
                    metadata: meta.into_metadata_json(),
                });
            }
            ProcessResult::Split {
                left,
                right,
                split_x,
                confidence,
                content_width_ratio,
                meta,
                fallback,
            } => {
                split_pages += 1;
                if fallback {
                    fallback_splits += 1;
                }

                let (outputs, emitted) = if let Some(workspace) = &workspace_directory {
                    let stem = path.file_stem().unwrap().to_string_lossy();
                    let suffix = path
                        .extension()
                        .map(|ext| format!(".{}", ext.to_string_lossy()))
                        .unwrap_or_else(String::new);
                    let right_name = format!("{}_R{}", stem, suffix);
                    let left_name = format!("{}_L{}", stem, suffix);
                    let right_path = workspace.join(&right_name);
                    let left_path = workspace.join(&left_name);
                    let mut emitted_local = 0;
                    if let Err(err) = save_image(&right, &right_path) {
                        warnings.push(format!(
                            "failed to write {}: {}",
                            right_path.display(),
                            err
                        ));
                    } else {
                        emitted_local += 1;
                    }
                    if let Err(err) = save_image(&left, &left_path) {
                        warnings.push(format!(
                            "failed to write {}: {}",
                            left_path.display(),
                            err
                        ));
                    } else {
                        emitted_local += 1;
                    }
                    (vec![right_path, left_path], emitted_local)
                } else {
                    (Vec::new(), 0)
                };

                emitted_files += emitted;

                items.push(SplitItemReport {
                    source: path.clone(),
                    mode: if fallback {
                        SplitMode::FallbackCenter
                    } else {
                        SplitMode::Split
                    },
                    split_x: Some(split_x),
                    confidence,
                    content_width_ratio,
                    outputs,
                    metadata: meta,
                });
            }
        }

        processed_files += 1;
        emit_progress(
            &mut progress,
            SplitProgress {
                total_files,
                processed_files,
                current_file: Some(path.clone()),
                stage: SplitProgressStage::Processing,
            },
        );
    }

    let report_path = if dry_run {
        None
    } else {
        workspace_directory
            .as_ref()
            .map(|dir| dir.join("split-report.json"))
    };

    if let Some(path) = &report_path {
        let json = serde_json::json!({
            "generatedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            "items": items
                .iter()
                .map(|item| serde_json::json!({
                    "source": item.source,
                    "mode": item.mode,
                    "split_x": item.split_x,
                    "confidence": item.confidence,
                    "content_width_ratio": item.content_width_ratio,
                    "outputs": item.outputs,
                    "metadata": item.metadata,
                }))
                .collect::<Vec<_>>(),
        });
        fs::write(path, format!("{}\n", serde_json::to_string_pretty(&json)?))?;
    }

    emit_progress(
        &mut progress,
        SplitProgress {
            total_files,
            processed_files,
            current_file: None,
            stage: SplitProgressStage::Completed,
        },
    );

    Ok(SplitCommandOutcome {
        analyzed_files,
        emitted_files,
        skipped_files,
        split_pages,
        cover_trims,
        fallback_splits,
        workspace_directory,
        report_path,
        items,
        warnings,
    })
}

fn emit_progress(
    callback: &mut Option<&mut dyn FnMut(SplitProgress)>,
    payload: SplitProgress,
) {
    if let Some(listener) = callback.as_mut() {
        listener(payload);
    }
}

fn create_workspace(directory: &Path, overwrite: bool) -> Result<PathBuf, SplitError> {
    let cache_root = directory.join(".rei_cache").join("doublepage");
    fs::create_dir_all(&cache_root)?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let workspace = cache_root.join(format!("session-{}", timestamp));
    if workspace.exists() {
        if overwrite {
            fs::remove_dir_all(&workspace)?;
        } else {
            return Ok(workspace);
        }
    }
    fs::create_dir_all(&workspace)?;
    Ok(workspace)
}

struct SkipMeta {
    content_width_ratio: f32,
}

impl SkipMeta {
    fn into_metadata_json(self) -> serde_json::Value {
        serde_json::json!({
            "contentWidthRatio": self.content_width_ratio,
            "splitMode": "skip",
        })
    }
}

enum ProcessResult {
    Skip(SkipMeta),
    CoverTrim {
        image: DynamicImage,
        meta: CoverMeta,
    },
    Split {
        left: DynamicImage,
        right: DynamicImage,
        split_x: u32,
        confidence: f32,
        content_width_ratio: f32,
        meta: serde_json::Value,
        fallback: bool,
    },
}

struct CoverMeta {
    content_width_ratio: f32,
    bbox_height_ratio: f32,
}

impl CoverMeta {
    fn into_metadata_json(self) -> serde_json::Value {
        serde_json::json!({
            "splitMode": "cover-trim",
            "contentWidthRatio": self.content_width_ratio,
            "bboxHeightRatio": self.bbox_height_ratio,
        })
    }
}

#[allow(unused_variables)]
fn process_image(image: &DynamicImage, path: &Path, thresholds: SplitThresholds) -> ProcessResult {
    let (width, height) = image.dimensions();
    if width < height {
        return ProcessResult::Skip(SkipMeta {
            content_width_ratio: 0.0,
        });
    }

    let aspect_ratio = width as f32 / height as f32;
    if aspect_ratio < thresholds.min_aspect_ratio {
        return ProcessResult::Skip(SkipMeta {
            content_width_ratio: 0.0,
        });
    }

    let gray = image.to_luma8();
    let equalized = equalize_histogram(&gray);
    let threshold_level = otsu_level(&equalized);

    let binary: ImageBuffer<Luma<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
        if equalized.get_pixel(x, y)[0] <= threshold_level {
            Luma([255])
        } else {
            Luma([0])
        }
    });

    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    let mut foreground_pixels = 0usize;
    for (x, y, pixel) in binary.enumerate_pixels() {
        if pixel[0] > 0 {
            foreground_pixels += 1;
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((min_x, min_y, max_x, max_y)) => {
                    (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
                }
            });
        }
    }

    if foreground_pixels == 0 {
        return ProcessResult::Skip(SkipMeta {
            content_width_ratio: 0.0,
        });
    }

    let content_width_ratio;
    let bbox_height_ratio;
    let bbox_cast;
    if let Some((min_x, min_y, max_x, max_y)) = bbox {
        let bbox_width = (max_x - min_x + 1) as f32;
        content_width_ratio = bbox_width / width as f32;
        bbox_height_ratio = (max_y - min_y + 1) as f32 / height as f32;
        bbox_cast = (min_x, min_y, max_x, max_y);
    } else {
        return ProcessResult::Skip(SkipMeta {
            content_width_ratio: 0.0,
        });
    }

    let foreground_ratio = foreground_pixels as f32 / (width * height) as f32;
    if foreground_ratio < thresholds.min_foreground_ratio {
        return ProcessResult::Skip(SkipMeta {
            content_width_ratio,
        });
    }

    if content_width_ratio < thresholds.cover_content_ratio && bbox_height_ratio > 0.8 {
        let padding_x = (thresholds.padding_ratio * width as f32).max(1.0) as u32;
        let padding_y = (thresholds.padding_ratio * height as f32).max(1.0) as u32;
        let crop = crop_with_padding(image, bbox_cast, padding_x, padding_y);

        return ProcessResult::CoverTrim {
            image: crop,
            meta: CoverMeta {
                content_width_ratio,
                bbox_height_ratio,
            },
        };
    }

    let split_info = locate_split(&binary, thresholds);
    let (split_x, confidence, fallback, projection_meta) = split_info;
    let padding_x = (thresholds.padding_ratio * width as f32).max(1.0) as u32;

    let actual_split_x = split_x.unwrap_or(width / 2).clamp(1, width - 1);

    let (right, left) = extract_pages(image, actual_split_x, padding_x);

    ProcessResult::Split {
        left,
        right,
        split_x: actual_split_x,
        confidence,
        content_width_ratio,
        meta: projection_meta,
        fallback,
    }
}

fn crop_with_padding(
    image: &DynamicImage,
    bbox: (u32, u32, u32, u32),
    padding_x: u32,
    padding_y: u32,
) -> DynamicImage {
    let (width, height) = image.dimensions();
    let (min_x, min_y, max_x, max_y) = bbox;

    let x0 = min_x.saturating_sub(padding_x).min(width - 1);
    let y0 = min_y.saturating_sub(padding_y).min(height - 1);
    let x1 = (max_x + padding_x + 1).min(width);
    let y1 = (max_y + padding_y + 1).min(height);

    image.crop_imm(x0, y0, x1 - x0, y1 - y0)
}

fn locate_split(
    mask: &ImageBuffer<Luma<u8>, Vec<u8>>,
    thresholds: SplitThresholds,
) -> (Option<u32>, f32, bool, serde_json::Value) {
    let (width, _height) = mask.dimensions();
    let width_usize = width as usize;

    let mut projection: Vec<f32> = vec![0.0; width_usize];
    for (x, _, pixel) in mask.enumerate_pixels() {
        if pixel[0] > 0 {
            projection[x as usize] += 1.0;
        }
    }

    if projection.iter().all(|&value| value == 0.0) {
        return (
            None,
            0.0,
            true,
            serde_json::json!({ "splitMode": "no-foreground" }),
        );
    }

    let kernel_radius = (width as f32 * 0.01).max(3.0) as usize;
    let smooth = smooth_projection(&projection, kernel_radius);
    let total: f32 = smooth.iter().sum();
    let mean = total / width as f32;

    let edge_margin = (width as f32 * thresholds.edge_exclusion_ratio).max(4.0) as usize;
    let start = edge_margin.min(width_usize - 2);
    let end = width_usize.saturating_sub(edge_margin).max(start + 2);

    let mut prefix: Vec<f32> = Vec::with_capacity(width_usize + 1);
    prefix.push(0.0);
    for value in smooth.iter() {
        let last = *prefix.last().unwrap();
        prefix.push(last + value);
    }

    let mut best_index = None;
    let mut best_score = f32::MAX;
    let mut best_value = 0.0f32;

    for idx in start..end {
        let left = prefix[idx];
        let right = total - left;
        if left == 0.0 || right == 0.0 {
            continue;
        }
        let balance = (left - right).abs() / total.max(f32::EPSILON);
        let valley = smooth[idx];
        let score = valley * (1.0 + balance * 1.8);
        if score < best_score {
            best_score = score;
            best_index = Some(idx);
            best_value = valley;
        }
    }

    let split_index = best_index.map(|idx| idx as u32);
    let confidence = if mean == 0.0 {
        0.0
    } else {
        ((mean - best_value).max(0.0) / mean.max(f32::EPSILON)).clamp(0.0, 1.0)
    };

    let fallback = confidence < thresholds.confidence_threshold;
    let meta = serde_json::json!({
        "splitMode": if fallback { "fallback-center" } else { "content-aware" },
        "confidence": confidence,
        "projectionMean": mean,
        "projectionValue": best_value,
        "edgeMargin": edge_margin,
        "index": split_index,
    });

    (split_index, confidence, fallback, meta)
}

fn smooth_projection(projection: &[f32], radius: usize) -> Vec<f32> {
    let len = projection.len();
    if len == 0 || radius == 0 {
        return projection.to_vec();
    }
    let window = radius * 2 + 1;
    let mut output = vec![0.0f32; len];
    let mut sum = 0.0f32;
    for i in 0..len + radius {
        if i < len {
            sum += projection[i];
        }
        if i >= window {
            sum -= projection[i - window];
        }
        if i >= radius {
            let idx = i - radius;
            if idx < len {
                output[idx] = sum / window as f32;
            }
        }
    }
    output
}

fn extract_pages(
    image: &DynamicImage,
    split_x: u32,
    padding_x: u32,
) -> (DynamicImage, DynamicImage) {
    let (width, height) = image.dimensions();
    let right_start = split_x.saturating_sub(padding_x).min(width - 1);
    let left_end = split_x + padding_x;
    let right = image.crop_imm(right_start, 0, width - right_start, height);
    let left = image.crop_imm(0, 0, left_end.min(width), height);
    (right, left)
}

fn save_image(image: &DynamicImage, target: &Path) -> Result<(), SplitError> {
    image.save(target)?;
    Ok(())
}

pub fn estimate_split_candidates(directory: &Path) -> Result<SplitDetectionSummary, SplitError> {
    if !directory.exists() || !directory.is_dir() {
        return Err(SplitError::DirectoryNotFound(directory.to_path_buf()));
    }

    let mut candidates = 0usize;
    let mut total = 0usize;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let supported = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|value| value.to_ascii_lowercase())
            .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.as_str()))
            .unwrap_or(false);
        if !supported {
            continue;
        }
        total += 1;

        let Ok(dimensions) = image::image_dimensions(&path) else {
            continue;
        };
        let (width, height) = dimensions;
        if width as f32 >= height as f32 * SplitThresholds::default().min_aspect_ratio {
            candidates += 1;
        }
    }

    Ok(SplitDetectionSummary { total, candidates })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SplitDetectionSummary {
    pub total: usize,
    pub candidates: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("docs")
            .join("assets")
            .join("manga-content-aware-split")
            .join("phase1_input")
            .join(name)
    }

    #[test]
    fn split_double_page_produces_two_outputs() {
        let temp = TempDir::new().expect("temp dir");
        let fixture = fixture_path("double_page_story.png");
        let dst = temp.path().join("double_page_story.png");
        fs::copy(&fixture, &dst).expect("copy fixture");

        let outcome = prepare_split(
            SplitCommandOptions {
                directory: temp.path().to_path_buf(),
                dry_run: false,
                overwrite: true,
                thresholds: None,
            },
            None,
        )
        .expect("split outcome");

        let workspace = outcome
            .workspace_directory
            .clone()
            .expect("workspace directory");
        assert_eq!(outcome.analyzed_files, 1);
        assert_eq!(outcome.split_pages, 1);
        assert_eq!(outcome.emitted_files, 2);
        assert!(workspace.join("double_page_story_R.png").exists());
        assert!(workspace.join("double_page_story_L.png").exists());
        if let Some(report_path) = &outcome.report_path {
            assert!(report_path.exists());
        }
    }

    #[test]
    fn progress_callback_receives_events_per_file() {
        let temp = TempDir::new().expect("temp dir");
        let fixtures = [
            fixture_path("double_page_story.png"),
            fixture_path("cover_layout.png"),
        ];
        for path in fixtures.iter() {
            let filename = path.file_name().expect("fixture name");
            fs::copy(path, temp.path().join(filename)).expect("copy fixture");
        }

        let mut events: Vec<SplitProgress> = Vec::new();
        {
            let mut recorder = |progress: SplitProgress| {
                events.push(progress);
            };

            prepare_split(
                SplitCommandOptions {
                    directory: temp.path().to_path_buf(),
                    dry_run: true,
                    overwrite: true,
                    thresholds: None,
                },
                Some(&mut recorder),
            )
            .expect("progress run");
        }

        assert!(events.len() >= 3, "expected init, per-file, and completion events");
        let total_files = fixtures.len();

        let first = &events[0];
        assert_eq!(first.stage, SplitProgressStage::Initializing);
        assert_eq!(first.processed_files, 0);
        assert_eq!(first.total_files, total_files);

        let last = events.last().expect("completion event");
        assert_eq!(last.stage, SplitProgressStage::Completed);
        assert_eq!(last.total_files, total_files);
        assert_eq!(last.processed_files, total_files);

        let processing_events: Vec<_> = events
            .iter()
            .filter(|event| event.stage == SplitProgressStage::Processing)
            .collect();
        assert_eq!(processing_events.len(), total_files);
        for (index, event) in processing_events.iter().enumerate() {
            assert_eq!(event.processed_files, index + 1);
            assert!(event.current_file.is_some());
            assert_eq!(event.total_files, total_files);
        }
    }

    #[test]
    fn cover_image_trims_without_split() {
        let temp = TempDir::new().expect("temp dir");
        let fixture = fixture_path("cover_layout.png");
        let dst = temp.path().join("cover_layout.png");
        fs::copy(&fixture, &dst).expect("copy fixture");

        let outcome = prepare_split(
            SplitCommandOptions {
                directory: temp.path().to_path_buf(),
                dry_run: false,
                overwrite: true,
                thresholds: None,
            },
            None,
        )
        .expect("split outcome");

        let workspace = outcome
            .workspace_directory
            .clone()
            .expect("workspace directory");
        assert_eq!(outcome.cover_trims, 1);
        assert_eq!(outcome.emitted_files, 1);
        assert!(workspace.join("cover_layout_cover.png").exists());
    }

    #[test]
    fn estimate_split_candidates_counts_wide_images() {
        let temp = TempDir::new().expect("temp dir");
        let fixtures = [
            "double_page_story.png",
            "cover_layout.png",
            "panorama_dense.png",
        ];
        for name in fixtures.iter() {
            let fixture = fixture_path(name);
            let dst = temp.path().join(name);
            fs::copy(&fixture, &dst).expect("copy fixture");
        }

        let summary = estimate_split_candidates(temp.path()).expect("estimate");
        assert_eq!(summary.total, fixtures.len());
        assert!(summary.candidates >= 2);
    }
}
