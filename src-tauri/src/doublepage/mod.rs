use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use chrono::{SecondsFormat, Utc};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Luma};
use natord::compare;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod config;
pub use config::{
    EdgeTextureThresholdOverrides, ProjectionConfig, ProjectionThresholdOverrides, SplitConfig,
    SplitModeSelector, SplitPrimaryMode,
};

mod mask;
pub use mask::build_foreground_mask;
use mask::BoundingBox;

mod edge_texture;
use edge_texture::{
    analyze_edges, EdgeTextureConfig, EdgeTextureMetrics, EdgeTextureNotes, EdgeTextureOutcome,
    MarginRegion,
};

mod projection;
use projection::analyze_projection;

mod regions;
use regions::{compute_region_bbox, crop_region_with_padding, RegionBounds};
use walkdir::{DirEntry, WalkDir};

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
    #[serde(default)]
    pub max_center_offset_ratio: Option<f32>,
    #[serde(default)]
    pub edge_texture: Option<EdgeTextureThresholdOverrides>,
    #[serde(default)]
    pub projection: Option<ProjectionThresholdOverrides>,
    #[serde(default)]
    pub mode: Option<SplitModeSelector>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePreviewRequest {
    pub image_path: PathBuf,
    pub brightness_thresholds: [f32; 2],
    #[serde(default)]
    pub brightness_weight: Option<f32>,
    #[serde(default)]
    pub white_threshold: Option<f32>,
    #[serde(default)]
    pub left_search_ratio: Option<f32>,
    #[serde(default)]
    pub right_search_ratio: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePreviewMetrics {
    pub width: u32,
    pub height: u32,
    pub mean_intensity_min: f32,
    pub mean_intensity_max: f32,
    pub mean_intensity_avg: f32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EdgePreviewMode {
    Split,
    CoverTrim,
    Skip,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EdgePreviewOutputRole {
    Left,
    Right,
    CoverTrim,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePreviewOutput {
    pub path: PathBuf,
    pub role: EdgePreviewOutputRole,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePreviewResponse {
    pub original_image: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trimmed_image: Option<PathBuf>,
    #[serde(default)]
    pub outputs: Vec<EdgePreviewOutput>,
    pub mode: EdgePreviewMode,
    pub left_margin: Option<MarginRegion>,
    pub right_margin: Option<MarginRegion>,
    pub brightness_thresholds: [f32; 2],
    pub brightness_weight: f32,
    pub confidence_threshold: f32,
    pub metrics: EdgePreviewMetrics,
    pub search_ratios: [f32; 2],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePreviewCandidate {
    pub path: PathBuf,
    pub file_name: String,
    pub relative_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
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

#[derive(Debug)]
struct FileOutcome {
    index: usize,
    source: PathBuf,
    items: Vec<SplitItemReport>,
    warnings: Vec<String>,
    emitted_files: usize,
    skipped_files: usize,
    split_pages: usize,
    cover_trims: usize,
    fallback_splits: usize,
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
    pub metadata: SplitMetadata,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SplitMode {
    Skip,
    CoverTrim,
    Split,
    FallbackCenter,
}

#[derive(Debug, Clone, Copy, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SplitBoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl From<BoundingBox> for SplitBoundingBox {
    fn from(bbox: BoundingBox) -> Self {
        Self {
            x: bbox.x0,
            y: bbox.y0,
            width: bbox.width(),
            height: bbox.height(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureMetadata {
    pub split_x: Option<u32>,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_margin: Option<MarginRegion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_margin: Option<MarginRegion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center_band: Option<MarginRegion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<EdgeTextureMetrics>,
    pub notes: EdgeTextureNotes,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SplitMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<SplitBoundingBox>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_imbalance: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_edge_margin: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_total_mass: Option<f32>,
    #[serde(rename = "splitMode", skip_serializing_if = "Option::is_none")]
    pub split_mode: Option<SplitMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_x: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_width_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox_height_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_clamped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_texture_confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_texture_threshold: Option<f32>,
    #[serde(rename = "edgeTexture", skip_serializing_if = "Option::is_none")]
    pub edge_texture: Option<EdgeTextureMetadata>,
    #[serde(rename = "splitStrategy", skip_serializing_if = "Option::is_none")]
    pub split_strategy: Option<String>,
}

impl SplitMetadata {
    fn with_reason(reason: &'static str) -> Self {
        let mut metadata = SplitMetadata::default();
        metadata.reason = Some(reason.to_string());
        metadata
    }

    fn with_foreground(mut self, ratio: f32) -> Self {
        self.foreground_ratio = Some(ratio);
        self
    }

    fn with_bbox(mut self, bbox: BoundingBox) -> Self {
        self.bbox = Some(bbox.into());
        self
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

#[derive(Debug, Error)]
pub enum EdgePreviewError {
    #[error("所选文件无法读取，请检查路径或格式")]
    UnsupportedImage,
    #[error("亮白阈值需在 0-255 范围内，且留黑阈值不得高于亮白阈值")]
    InvalidThresholds,
    #[error("图像处理失败: {0}")]
    Image(#[from] image::ImageError),
    #[error("文件系统错误: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum EdgePreviewListError {
    #[error("预览目录不存在或不是文件夹: {0}")]
    DirectoryNotFound(PathBuf),
    #[error("读取目录失败: {0}")]
    Io(#[from] io::Error),
}

const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn list_edge_preview_candidates(
    directory: &Path,
) -> Result<Vec<EdgePreviewCandidate>, EdgePreviewListError> {
    if !directory.exists() || !directory.is_dir() {
        return Err(EdgePreviewListError::DirectoryNotFound(directory.to_path_buf()));
    }

    let root = fs::canonicalize(directory)?;
    let mut entries: Vec<EdgePreviewCandidate> = Vec::new();

    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        if !is_supported_image(&path) {
            continue;
        }

        let relative_path = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace("\\", "/");
        let file_name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| relative_path.clone());

        let file_size = entry.metadata().ok().map(|meta| meta.len());

        entries.push(EdgePreviewCandidate {
            path,
            file_name,
            relative_path,
            file_size,
        });
    }

    entries.sort_by(|a, b| compare(a.relative_path.as_str(), b.relative_path.as_str()));

    Ok(entries)
}

fn should_descend(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if entry.file_type().is_dir() {
        if let Some(name) = entry.file_name().to_str() {
            if name == ".rei_cache" {
                return false;
            }
        }
    }

    true
}

fn collect_supported_entries(path: &Path) -> Result<(Vec<PathBuf>, PathBuf), SplitError> {
    if !path.exists() {
        return Err(SplitError::DirectoryNotFound(path.to_path_buf()));
    }

    if path.is_file() {
        if is_supported_image(path) {
            let parent = path
                .parent()
                .and_then(|dir| {
                    let os_str = dir.as_os_str();
                    if os_str.is_empty() {
                        None
                    } else {
                        Some(dir.to_path_buf())
                    }
                })
                .unwrap_or_else(|| PathBuf::from("."));
            return Ok((vec![path.to_path_buf()], parent));
        }

        return Err(SplitError::EmptyDirectory(path.to_path_buf()));
    }

    if !path.is_dir() {
        return Err(SplitError::DirectoryNotFound(path.to_path_buf()));
    }

    let mut entries: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_descend)
    {
        let entry = entry.map_err(|err| {
            if let Some(io_err) = err.io_error() {
                SplitError::Io(io::Error::new(io_err.kind(), io_err.to_string()))
            } else {
                SplitError::Io(io::Error::new(io::ErrorKind::Other, err.to_string()))
            }
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        if is_supported_image(entry.path()) {
            entries.push(entry.into_path());
        }
    }

    if entries.is_empty() {
        return Err(SplitError::EmptyDirectory(path.to_path_buf()));
    }

    entries.sort();
    Ok((entries, path.to_path_buf()))
}

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

    let config = if let Some(overrides) = thresholds_override.as_ref() {
        SplitConfig::default().with_overrides(overrides)
    } else {
        SplitConfig::default()
    };

    let (collected_entries, workspace_root) = collect_supported_entries(&directory)?;

    let entries = Arc::new(collected_entries);
    let total_files = entries.len();
    let workspace_directory = if dry_run {
        None
    } else {
        Some(Arc::new(create_workspace(&workspace_root, overwrite)?))
    };

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

    let results: Arc<Mutex<Vec<FileOutcome>>> =
        Arc::new(Mutex::new(Vec::with_capacity(total_files)));
    let progress_state: Arc<(Mutex<BTreeMap<usize, PathBuf>>, Condvar)> =
        Arc::new((Mutex::new(BTreeMap::new()), Condvar::new()));
    let config_for_workers = config;
    let workspace_for_workers = workspace_directory.clone();
    let results_handle = Arc::clone(&results);
    let progress_handle = Arc::clone(&progress_state);
    let worker_count = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(total_files.max(1));
    let task_cursor = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let entries = Arc::clone(&entries);
            let results_collector = Arc::clone(&results_handle);
            let progress_tracker = Arc::clone(&progress_handle);
            let cursor = Arc::clone(&task_cursor);
            let worker_workspace = workspace_for_workers.clone();

            scope.spawn(move || loop {
                let index = cursor.fetch_add(1, Ordering::SeqCst);
                if index >= entries.len() {
                    break;
                }

                let path = entries[index].clone();
                let workspace_entry = worker_workspace.as_ref().map(Arc::clone);
                let outcome = process_entry(index, path, config_for_workers, workspace_entry);

                {
                    let (lock, cvar) = &*progress_tracker;
                    let mut pending = lock.lock().expect("progress tracker lock poisoned");
                    pending.insert(outcome.index, outcome.source.clone());
                    cvar.notify_one();
                }

                let mut guard = results_collector
                    .lock()
                    .expect("results collector poisoned");
                guard.push(outcome);
            });
        }

        let (lock, cvar) = &*progress_handle;
        while processed_files < total_files {
            let path = {
                let mut pending = lock.lock().expect("progress tracker lock poisoned");
                while !pending.contains_key(&processed_files) {
                    pending = cvar
                        .wait(pending)
                        .expect("progress tracker condvar poisoned");
                }
                pending
                    .remove(&processed_files)
                    .expect("missing progress entry")
            };

            processed_files += 1;
            emit_progress(
                &mut progress,
                SplitProgress {
                    total_files,
                    processed_files,
                    current_file: Some(path),
                    stage: SplitProgressStage::Processing,
                },
            );
        }
    });

    drop(results_handle);
    drop(progress_handle);

    let mut results = Arc::try_unwrap(results)
        .expect("dangling references to results collector")
        .into_inner()
        .expect("results collector poisoned");

    results.sort_by_key(|outcome| outcome.index);

    let mut emitted_files = 0usize;
    let mut skipped_files = 0usize;
    let mut split_pages = 0usize;
    let mut cover_trims = 0usize;
    let mut fallback_splits = 0usize;
    let mut warnings: Vec<String> = Vec::new();
    let mut items: Vec<SplitItemReport> = Vec::new();

    for outcome in results.into_iter() {
        emitted_files += outcome.emitted_files;
        skipped_files += outcome.skipped_files;
        split_pages += outcome.split_pages;
        cover_trims += outcome.cover_trims;
        fallback_splits += outcome.fallback_splits;
        warnings.extend(outcome.warnings);
        items.extend(outcome.items);
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
        analyzed_files: total_files,
        emitted_files,
        skipped_files,
        split_pages,
        cover_trims,
        fallback_splits,
        workspace_directory: workspace_directory
            .as_ref()
            .map(|dir| dir.as_path().to_path_buf()),
        report_path,
        items,
        warnings,
    })
}

fn emit_progress(callback: &mut Option<&mut dyn FnMut(SplitProgress)>, payload: SplitProgress) {
    if let Some(listener) = callback.as_mut() {
        listener(payload);
    }
}

fn process_entry(
    index: usize,
    path: PathBuf,
    config: SplitConfig,
    workspace: Option<Arc<PathBuf>>,
) -> FileOutcome {
    let mut warnings: Vec<String> = Vec::new();
    let mut items: Vec<SplitItemReport> = Vec::new();
    let mut emitted_files = 0usize;
    let mut skipped_files = 0usize;
    let mut split_pages = 0usize;
    let mut cover_trims = 0usize;
    let mut fallback_splits = 0usize;

    let workspace_path = workspace.as_ref().map(|dir| dir.as_path());

    let image = match image::open(&path) {
        Ok(img) => img,
        Err(err) => {
            warnings.push(format!("failed to read {}: {}", path.display(), err));
            skipped_files = 1;
            return FileOutcome {
                index,
                source: path,
                items,
                warnings,
                emitted_files,
                skipped_files,
                split_pages,
                cover_trims,
                fallback_splits,
            };
        }
    };

    match process_image(&image, &path, config) {
        ProcessResult::Skip {
            content_width_ratio,
            metadata,
        } => {
            skipped_files += 1;
            let outputs = if let Some(dir) = workspace_path {
                let target = dir.join(path.file_name().unwrap());
                match fs::copy(&path, &target) {
                    Ok(_) => {
                        emitted_files += 1;
                        vec![target]
                    }
                    Err(err) => {
                        warnings.push(format!(
                            "failed to copy {} into workspace: {}",
                            path.display(),
                            err
                        ));
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            items.push(SplitItemReport {
                source: path.clone(),
                mode: SplitMode::Skip,
                split_x: None,
                confidence: 0.0,
                content_width_ratio,
                outputs,
                metadata,
            });
        }
        ProcessResult::CoverTrim {
            image: cover,
            content_width_ratio,
            meta,
        } => {
            cover_trims += 1;
            let (outputs, emitted) = if let Some(dir) = workspace_path {
                let filename = format!(
                    "{}_cover{}",
                    path.file_stem().unwrap().to_string_lossy(),
                    path.extension()
                        .map(|ext| format!(".{}", ext.to_string_lossy()))
                        .unwrap_or_else(String::new)
                );
                let target = dir.join(&filename);
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
                content_width_ratio,
                outputs,
                metadata: meta,
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

            let (outputs, emitted) = if let Some(dir) = workspace_path {
                let stem = path.file_stem().unwrap().to_string_lossy();
                let suffix = path
                    .extension()
                    .map(|ext| format!(".{}", ext.to_string_lossy()))
                    .unwrap_or_else(String::new);
                let right_name = format!("{}_R{}", stem, suffix);
                let left_name = format!("{}_L{}", stem, suffix);
                let right_path = dir.join(&right_name);
                let left_path = dir.join(&left_name);
                let mut emitted_local = 0usize;
                if let Err(err) = save_image(&right, &right_path) {
                    warnings.push(format!("failed to write {}: {}", right_path.display(), err));
                } else {
                    emitted_local += 1;
                }
                if let Err(err) = save_image(&left, &left_path) {
                    warnings.push(format!("failed to write {}: {}", left_path.display(), err));
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

    FileOutcome {
        index,
        source: path,
        items,
        warnings,
        emitted_files,
        skipped_files,
        split_pages,
        cover_trims,
        fallback_splits,
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

enum ProcessResult {
    Skip {
        content_width_ratio: f32,
        metadata: SplitMetadata,
    },
    CoverTrim {
        image: DynamicImage,
        content_width_ratio: f32,
        meta: SplitMetadata,
    },
    Split {
        left: DynamicImage,
        right: DynamicImage,
        split_x: u32,
        confidence: f32,
        content_width_ratio: f32,
        meta: SplitMetadata,
        fallback: bool,
    },
}

fn process_image(image: &DynamicImage, _path: &Path, config: SplitConfig) -> ProcessResult {
    let (width, height) = image.dimensions();
    if width < height {
        return ProcessResult::Skip {
            content_width_ratio: 0.0,
            metadata: SplitMetadata::with_reason("aspect_ratio"),
        };
    }

    let aspect_ratio = width as f32 / height as f32;
    if aspect_ratio < config.min_aspect_ratio {
        return ProcessResult::Skip {
            content_width_ratio: 0.0,
            metadata: SplitMetadata::with_reason("aspect_ratio"),
        };
    }

    let mask_result = match build_foreground_mask(image) {
        Ok(result) => result,
        Err(_err) => {
            return ProcessResult::Skip {
                content_width_ratio: 0.0,
                metadata: SplitMetadata::with_reason("mask_error"),
            };
        }
    };

    let mask = mask_result.mask;
    let foreground_ratio = mask_result.foreground_ratio;

    let bbox = match mask_result.bounding_box {
        Some(value) => value,
        None => {
            return ProcessResult::Skip {
                content_width_ratio: 0.0,
                metadata: SplitMetadata::with_reason("no_foreground")
                    .with_foreground(foreground_ratio),
            };
        }
    };

    let content_width_ratio = bbox.width() as f32 / width as f32;
    let bbox_height_ratio = bbox.height() as f32 / height as f32;

    if foreground_ratio < config.min_foreground_ratio {
        return ProcessResult::Skip {
            content_width_ratio,
            metadata: SplitMetadata::with_reason("no_foreground").with_foreground(foreground_ratio),
        };
    }

    let padding_x = (config.padding_ratio * width as f32).max(1.0) as u32;
    let padding_y = (config.padding_ratio * height as f32).max(1.0) as u32;

    let mut base_metadata = SplitMetadata::default()
        .with_foreground(foreground_ratio)
        .with_bbox(bbox);
    base_metadata.content_width_ratio = Some(content_width_ratio);

    if content_width_ratio < config.cover_content_ratio && bbox_height_ratio > 0.8 {
        let region_bounds = RegionBounds { bbox };
        let crop = crop_region_with_padding(image, &region_bounds, padding_x, padding_y);

        let mut cover_metadata = base_metadata.clone();
        cover_metadata.split_mode = Some(SplitMode::CoverTrim);
        cover_metadata.bbox_height_ratio = Some(bbox_height_ratio);

        return ProcessResult::CoverTrim {
            image: crop,
            content_width_ratio,
            meta: cover_metadata,
        };
    }
    let mut strategy_order: Vec<SplitPrimaryMode> = Vec::with_capacity(2);
    match config.mode {
        SplitModeSelector::EdgeTextureOnly => strategy_order.push(SplitPrimaryMode::EdgeTexture),
        SplitModeSelector::ProjectionOnly => strategy_order.push(SplitPrimaryMode::Projection),
        SplitModeSelector::Hybrid { primary, fallback } => {
            strategy_order.push(primary);
            if fallback != primary {
                strategy_order.push(fallback);
            }
        }
    }

    let enforce_center_offset_limit = !matches!(config.mode, SplitModeSelector::EdgeTextureOnly);
    let force_center_split = matches!(config.mode, SplitModeSelector::EdgeTextureOnly);

    let mut edge_outcome: Option<EdgeTextureOutcome> = None;
    let mut projection_outcome: Option<projection::ProjectionOutcome> = None;
    let mut projection_stats: Option<ProjectionStats> = None;
    let mut selected_strategy: Option<SplitPrimaryMode> = None;
    let mut candidate_split: Option<u32> = None;
    let mut candidate_confidence = 0.0f32;

    for strategy in strategy_order.iter().copied() {
        match strategy {
            SplitPrimaryMode::EdgeTexture => {
                if edge_outcome.is_none() {
                    edge_outcome = Some(analyze_edges(image, config.edge_texture));
                }
                if let Some(outcome) = edge_outcome.as_ref() {
                    if force_center_split {
                        candidate_split = Some(width / 2);
                        candidate_confidence = outcome.confidence;
                        selected_strategy = Some(SplitPrimaryMode::EdgeTexture);
                        break;
                    }
                    if let Some(split) = outcome.split_x {
                        if outcome.confidence >= config.confidence_threshold {
                            candidate_split = Some(split);
                            candidate_confidence = outcome.confidence;
                            selected_strategy = Some(SplitPrimaryMode::EdgeTexture);
                            break;
                        }
                    }
                }
            }
            SplitPrimaryMode::Projection => {
                if projection_outcome.is_none() {
                    let outcome = analyze_projection(&mask, config.projection);
                    projection_stats = Some(ProjectionStats {
                        imbalance: outcome.imbalance,
                        edge_margin: outcome.edge_margin,
                        total_mass: outcome.total_mass,
                    });
                    projection_outcome = Some(outcome);
                }
                if let Some(outcome) = projection_outcome.as_ref() {
                    if let Some(split) = outcome.split_x {
                        if outcome.confidence >= config.confidence_threshold {
                            candidate_split = Some(split);
                            candidate_confidence = outcome.confidence;
                            selected_strategy = Some(SplitPrimaryMode::Projection);
                            break;
                        }
                    }
                }
            }
        }
    }

    let fallback_source_split = candidate_split
        .or_else(|| {
            projection_outcome
                .as_ref()
                .and_then(|outcome| outcome.split_x)
        })
        .or_else(|| edge_outcome.as_ref().and_then(|outcome| outcome.split_x))
        .unwrap_or(width / 2);

    let mut fallback_required = candidate_split.is_none();
    let safe_split_x = fallback_source_split.clamp(1, width.saturating_sub(1).max(1));
    let (clamped_candidate, clamped_to_center) = if enforce_center_offset_limit {
        clamp_split_to_center(safe_split_x, width, config.max_center_offset_ratio)
    } else {
        (safe_split_x, false)
    };

    let mut final_split_x = if enforce_center_offset_limit && clamped_to_center {
        (width / 2).clamp(1, width.saturating_sub(1).max(1))
    } else {
        clamped_candidate
    };

    final_split_x = final_split_x.clamp(1, width.saturating_sub(1).max(1));
    if enforce_center_offset_limit && clamped_to_center {
        fallback_required = true;
    }

    let effective_confidence = if fallback_required {
        0.0
    } else {
        candidate_confidence
    };

    let mut split_metadata = base_metadata;
    split_metadata.split_mode = Some(if fallback_required {
        SplitMode::FallbackCenter
    } else {
        SplitMode::Split
    });
    split_metadata.confidence = Some(effective_confidence);

    if let Some(stats) = projection_stats {
        split_metadata.projection_imbalance = Some(stats.imbalance);
        split_metadata.projection_edge_margin = Some(stats.edge_margin);
        split_metadata.projection_total_mass = Some(stats.total_mass);
    }

    if enforce_center_offset_limit && clamped_to_center {
        split_metadata.split_clamped = Some(true);
    }
    split_metadata.split_x = Some(final_split_x);

    if let Some(outcome) = edge_outcome.as_ref() {
        split_metadata.edge_texture_confidence = Some(outcome.confidence);
        split_metadata.edge_texture_threshold = Some(config.edge_texture.white_threshold);
        split_metadata.edge_texture = Some(EdgeTextureMetadata {
            split_x: outcome.split_x,
            confidence: outcome.confidence,
            left_margin: outcome.left_margin,
            right_margin: outcome.right_margin,
            center_band: outcome.center_band,
            metrics: Some(outcome.metrics.clone()),
            notes: outcome.notes.clone(),
        });
    }

    let mut split_strategy = selected_strategy.map(|strategy| match strategy {
        SplitPrimaryMode::EdgeTexture => "edgeTexture".to_string(),
        SplitPrimaryMode::Projection => "projection".to_string(),
    });

    if split_strategy.is_none() && fallback_required {
        split_strategy = Some("fallbackCenter".to_string());
    }

    split_metadata.split_strategy = split_strategy;

    let right_region = compute_region_bbox(&mask, final_split_x, width);
    let left_region = compute_region_bbox(&mask, 0, final_split_x);

    let mut right = crop_region_with_padding(image, &right_region, padding_x, padding_y);
    let mut left = crop_region_with_padding(image, &left_region, padding_x, padding_y);

    if force_center_split {
        left = trim_page_with_edge_texture(left, config.edge_texture, config.confidence_threshold);
        right =
            trim_page_with_edge_texture(right, config.edge_texture, config.confidence_threshold);
    }

    ProcessResult::Split {
        left,
        right,
        split_x: final_split_x,
        confidence: effective_confidence,
        content_width_ratio,
        meta: split_metadata,
        fallback: fallback_required,
    }
}

fn locate_split(
    mask: &ImageBuffer<Luma<u8>, Vec<u8>>,
    config: SplitConfig,
) -> (Option<u32>, f32, bool, ProjectionStats) {
    let outcome = analyze_projection(mask, config.projection);
    let fallback = outcome.split_x.is_none() || outcome.confidence < config.confidence_threshold;

    (
        outcome.split_x,
        outcome.confidence,
        fallback,
        ProjectionStats {
            imbalance: outcome.imbalance,
            edge_margin: outcome.edge_margin,
            total_mass: outcome.total_mass,
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct ProjectionStats {
    imbalance: f32,
    edge_margin: u32,
    total_mass: f32,
}

fn clamp_split_to_center(split_x: u32, width: u32, max_ratio: f32) -> (u32, bool) {
    if width <= 1 {
        return (0, false);
    }

    let normalized_ratio = max_ratio.clamp(0.0, 0.5);
    let center = width as f32 / 2.0;
    let max_offset = (width as f32 * normalized_ratio).max(1.0);
    let offset = split_x as f32 - center;

    if offset.abs() > max_offset {
        let candidate = center + offset.signum() * max_offset;
        let clamped = candidate
            .round()
            .clamp(1.0, (width.saturating_sub(1).max(1)) as f32) as u32;
        (clamped, true)
    } else {
        let clamped = split_x.clamp(1, width.saturating_sub(1).max(1));
        (clamped, false)
    }
}

fn compute_trim_bounds(
    outcome: &EdgeTextureOutcome,
    width: u32,
    confidence_threshold: f32,
) -> Option<(u32, u32)> {
    let mut x_start = 0u32;
    let mut x_end = width;

    if let Some(left) = outcome.left_margin {
        if left.confidence >= confidence_threshold {
            let candidate = left.end_x.saturating_add(1).min(width);
            if candidate < x_end {
                x_start = x_start.max(candidate);
            }
        }
    }

    if let Some(right) = outcome.right_margin {
        if right.confidence >= confidence_threshold {
            let candidate = right.start_x.min(width);
            if candidate > x_start {
                x_end = x_end.min(candidate);
            }
        }
    }

    if x_end <= x_start || x_end - x_start < 2 {
        return None;
    }

    Some((x_start, x_end))
}

fn trim_page_with_edge_texture(
    page: DynamicImage,
    config: EdgeTextureConfig,
    confidence_threshold: f32,
) -> DynamicImage {
    let width = page.width();
    let height = page.height();
    if width <= 1 || height == 0 {
        return page;
    }

    let outcome = analyze_edges(&page, config);

    if let Some((x_start, x_end)) = compute_trim_bounds(&outcome, width, confidence_threshold) {
        page.crop_imm(x_start, 0, x_end - x_start, height)
    } else {
        page
    }
}

pub fn preview_edge_texture_trim(
    cache_root: &Path,
    request: EdgePreviewRequest,
) -> Result<EdgePreviewResponse, EdgePreviewError> {
    if !request.image_path.exists() || !is_supported_image(&request.image_path) {
        return Err(EdgePreviewError::UnsupportedImage);
    }

    let [raw_bright, raw_dark] = request.brightness_thresholds;
    if raw_dark > raw_bright {
        return Err(EdgePreviewError::InvalidThresholds);
    }

    let bright = raw_bright.clamp(0.0, 255.0);
    let mut dark = raw_dark.clamp(0.0, 255.0);
    if dark > bright {
        dark = bright;
    }

    let canonical_image_path =
        fs::canonicalize(&request.image_path).map_err(|_| EdgePreviewError::UnsupportedImage)?;
    let image = image::open(&canonical_image_path)?;

    let mut edge_overrides = EdgeTextureThresholdOverrides::default();
    edge_overrides.brightness_thresholds = Some([bright, dark]);
    if let Some(weight) = request.brightness_weight {
        edge_overrides.brightness_weight = Some(weight.clamp(0.0, 1.0));
    } else {
        edge_overrides.brightness_weight = Some(0.5);
    }
    if let Some(white_threshold) = request.white_threshold {
        edge_overrides.white_threshold = Some(white_threshold);
    }
    if let Some(left_ratio) = request.left_search_ratio {
        edge_overrides.left_search_ratio = Some(left_ratio.clamp(0.0, 0.5));
    }
    if let Some(right_ratio) = request.right_search_ratio {
        edge_overrides.right_search_ratio = Some(right_ratio.clamp(0.0, 0.5));
    }

    let split_overrides = SplitThresholdOverrides {
        cover_content_ratio: None,
        confidence_threshold: None,
        edge_exclusion_ratio: None,
        min_foreground_ratio: None,
        padding_ratio: None,
        max_center_offset_ratio: None,
        edge_texture: Some(edge_overrides),
        projection: None,
        mode: Some(SplitModeSelector::EdgeTextureOnly),
    };

    let split_config = SplitConfig::default().with_overrides(&split_overrides);
    let config_edge = split_config.edge_texture;
    let brightness_weight = config_edge.brightness_weight;
    let search_ratios = [config_edge.left_search_ratio, config_edge.right_search_ratio];
    let confidence_threshold = split_config.confidence_threshold;

    let outcome = analyze_edges(&image, config_edge);

    let width = image.width();
    let height = image.height();

    let preview_dir = cache_root.join("edge-preview");
    fs::create_dir_all(&preview_dir)?;
    let timestamp = Utc::now().format("%Y%m%d%H%M%S%3f");

    let mut trimmed_image: Option<PathBuf> = None;
    let mut outputs: Vec<EdgePreviewOutput> = Vec::new();

    let mode = match process_image(&image, &canonical_image_path, split_config) {
        ProcessResult::Split { left, right, .. } => {
            let left_path = preview_dir.join(format!("edge-preview-{}-left.png", timestamp));
            let right_path = preview_dir.join(format!("edge-preview-{}-right.png", timestamp));
            left.save_with_format(&left_path, ImageFormat::Png)?;
            right.save_with_format(&right_path, ImageFormat::Png)?;
            let left_path = fs::canonicalize(left_path)?;
            let right_path = fs::canonicalize(right_path)?;
            outputs.push(EdgePreviewOutput {
                path: left_path.clone(),
                role: EdgePreviewOutputRole::Left,
            });
            outputs.push(EdgePreviewOutput {
                path: right_path.clone(),
                role: EdgePreviewOutputRole::Right,
            });
            EdgePreviewMode::Split
        }
        ProcessResult::CoverTrim { image: trimmed, .. } => {
            let trimmed_path = preview_dir.join(format!("edge-preview-{}-trim.png", timestamp));
            trimmed.save_with_format(&trimmed_path, ImageFormat::Png)?;
            let trimmed_path = fs::canonicalize(trimmed_path)?;
            outputs.push(EdgePreviewOutput {
                path: trimmed_path.clone(),
                role: EdgePreviewOutputRole::CoverTrim,
            });
            trimmed_image = Some(trimmed_path);
            EdgePreviewMode::CoverTrim
        }
        ProcessResult::Skip { .. } => EdgePreviewMode::Skip,
    };

    let mean_values = &outcome.metrics.mean_intensity;
    let (min_intensity, max_intensity, avg_intensity) = if mean_values.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let mut min_v = f32::MAX;
        let mut max_v = f32::MIN;
        let mut sum = 0.0f32;
        for value in mean_values {
            min_v = min_v.min(*value);
            max_v = max_v.max(*value);
            sum += *value;
        }
        (min_v, max_v, sum / mean_values.len() as f32)
    };

    let response = EdgePreviewResponse {
        original_image: canonical_image_path,
        trimmed_image,
        outputs,
        mode,
        left_margin: outcome.left_margin,
        right_margin: outcome.right_margin,
        brightness_thresholds: [bright, dark],
        brightness_weight,
        confidence_threshold,
        metrics: EdgePreviewMetrics {
            width,
            height,
            mean_intensity_min: min_intensity,
            mean_intensity_max: max_intensity,
            mean_intensity_avg: avg_intensity,
        },
        search_ratios,
    };

    Ok(response)
}

fn save_image(image: &DynamicImage, target: &Path) -> Result<(), SplitError> {
    image.save(target)?;
    Ok(())
}

pub fn estimate_split_candidates(directory: &Path) -> Result<SplitDetectionSummary, SplitError> {
    let (entries, _) = collect_supported_entries(directory)?;

    let mut candidates = 0usize;
    for path in entries.iter() {
        let Ok(dimensions) = image::image_dimensions(path) else {
            continue;
        };
        let (width, height) = dimensions;
        if width as f32 >= height as f32 * SplitConfig::default().min_aspect_ratio {
            candidates += 1;
        }
    }

    Ok(SplitDetectionSummary {
        total: entries.len(),
        candidates,
    })
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
    use image::DynamicImage;
    use std::fs;
    use std::path::{Path, PathBuf};
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
    fn preview_generates_split_outputs() {
        let cache_dir = TempDir::new().expect("cache dir");
        let request = EdgePreviewRequest {
            image_path: fixture_path("double_page_story.png"),
            brightness_thresholds: [200.0, 75.0],
            brightness_weight: None,
            white_threshold: None,
            left_search_ratio: None,
            right_search_ratio: None,
        };

        let response =
            preview_edge_texture_trim(cache_dir.path(), request).expect("preview should succeed");

        assert_eq!(response.mode, EdgePreviewMode::Split);
        assert!(response.trimmed_image.is_none());
        assert_eq!(response.outputs.len(), 2);
        assert!(response
            .outputs
            .iter()
            .any(|output| output.role == EdgePreviewOutputRole::Left));
        assert!(response
            .outputs
            .iter()
            .any(|output| output.role == EdgePreviewOutputRole::Right));
        for output in response.outputs.iter() {
            assert!(output.path.exists());
        }
        let (orig_width, orig_height) =
            image::image_dimensions(&response.original_image).expect("original dimensions");
        assert_eq!(response.brightness_thresholds, [200.0, 75.0]);
        assert_eq!(response.metrics.width, orig_width);
        assert_eq!(response.metrics.height, orig_height);
    }

    #[test]
    fn preview_rejects_invalid_thresholds() {
        let cache_dir = TempDir::new().expect("cache dir");
        let request = EdgePreviewRequest {
            image_path: fixture_path("double_page_story.png"),
            brightness_thresholds: [80.0, 120.0],
            brightness_weight: None,
            white_threshold: None,
            left_search_ratio: None,
            right_search_ratio: None,
        };

        let err = preview_edge_texture_trim(cache_dir.path(), request)
            .expect_err("invalid thresholds should fail");
        assert!(matches!(err, EdgePreviewError::InvalidThresholds));
    }

    #[test]
    fn list_edge_preview_candidates_returns_files() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("docs")
            .join("assets")
            .join("manga-content-aware-split")
            .join("phase1_input");
        let canonical_directory =
            fs::canonicalize(&directory).expect("canonical directory should resolve");

        let entries =
            list_edge_preview_candidates(&directory).expect("preview candidates should load");

        assert!(!entries.is_empty(), "expected test fixture to contain images");
        for entry in entries.iter() {
            assert!(entry.path.starts_with(&canonical_directory));
            assert!(is_supported_image(&entry.path));
        }

        let relative_paths: Vec<String> = entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();
        let mut sorted = relative_paths.clone();
        sorted.sort_by(|a, b| compare(a.as_str(), b.as_str()));
        assert_eq!(relative_paths, sorted, "entries should be natural-sorted");
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

        let split_item = outcome
            .items
            .iter()
            .find(|item| item.mode == SplitMode::Split)
            .expect("split item expected");
        assert_eq!(split_item.metadata.split_mode, Some(SplitMode::Split));
        assert!(split_item.metadata.bbox.is_some());
        let metadata_ratio = split_item
            .metadata
            .content_width_ratio
            .expect("metadata content width ratio");
        assert!((metadata_ratio - split_item.content_width_ratio).abs() < 1e-5);
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

        assert!(
            events.len() >= 3,
            "expected init, per-file, and completion events"
        );
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

        let cover_item = outcome
            .items
            .iter()
            .find(|item| item.mode == SplitMode::CoverTrim)
            .expect("cover trim item expected");
        assert_eq!(cover_item.metadata.split_mode, Some(SplitMode::CoverTrim));
        assert!(cover_item.metadata.bbox_height_ratio.unwrap_or_default() > 0.8);
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

    #[test]
    fn recursive_directory_traversal_processes_nested_files() {
        let temp = TempDir::new().expect("temp dir");
        let nested_dir = temp.path().join("nested");
        fs::create_dir(&nested_dir).expect("create nested dir");

        let fixture = fixture_path("double_page_story.png");
        let nested_file = nested_dir.join("double_page_story.png");
        fs::copy(&fixture, &nested_file).expect("copy nested fixture");

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

        assert_eq!(outcome.analyzed_files, 1);
        let workspace = outcome
            .workspace_directory
            .clone()
            .expect("workspace directory");
        assert!(workspace.join("double_page_story_R.png").exists());
        assert!(workspace.join("double_page_story_L.png").exists());
    }

    #[test]
    fn existing_cache_directories_are_skipped() {
        let temp = TempDir::new().expect("temp dir");
        let cache_dir = temp
            .path()
            .join(".rei_cache")
            .join("doublepage")
            .join("session-keep");
        fs::create_dir_all(&cache_dir).expect("create cache dir");

        let cache_fixture = fixture_path("cover_layout.png");
        fs::copy(&cache_fixture, cache_dir.join("cover_layout.png")).expect("copy cache fixture");

        let fixture = fixture_path("double_page_story.png");
        let target = temp.path().join("double_page_story.png");
        fs::copy(&fixture, &target).expect("copy primary fixture");

        let outcome = prepare_split(
            SplitCommandOptions {
                directory: temp.path().to_path_buf(),
                dry_run: true,
                overwrite: true,
                thresholds: None,
            },
            None,
        )
        .expect("split outcome");

        assert_eq!(outcome.analyzed_files, 1);
        assert_eq!(outcome.split_pages, 1);
        assert!(outcome
            .items
            .iter()
            .all(|item| !item.source.to_string_lossy().contains(".rei_cache")));
    }

    #[test]
    fn projection_aligns_with_python_reference_for_story_sample() {
        let fixture = image::open(fixture_path("double_page_story.png")).expect("load fixture");
        let mask = build_foreground_mask(&fixture).expect("mask computation");

        let (split_x, confidence, fallback, stats) =
            locate_split(&mask.mask, SplitConfig::default());

        let split_x = split_x.expect("content-aware split expected");
        assert!(!fallback, "double page story should not fallback");
        assert!(
            (split_x as i32 - 460).abs() <= 5,
            "unexpected split position"
        );
        assert!(
            confidence >= 0.9,
            "confidence should be high for aligned sample"
        );
        assert!(stats.imbalance >= 0.0);
        assert_eq!(stats.edge_margin, 115);
        assert!(stats.total_mass > 0.0);
    }

    #[test]
    fn projection_signals_fallback_for_dense_panorama() {
        let fixture = image::open(fixture_path("panorama_dense.png")).expect("load fixture");
        let mask = build_foreground_mask(&fixture).expect("mask computation");

        let (split_x, confidence, fallback, _stats) =
            locate_split(&mask.mask, SplitConfig::default());

        let split_x_val = split_x.unwrap_or(0);
        assert!(fallback, "dense panorama should fallback to center");
        assert!(
            (split_x_val as i32 - 500).abs() <= 5,
            "fallback split should be near center"
        );
        assert!(confidence <= 0.1, "confidence should stay low on fallback");
    }

    #[test]
    fn hybrid_mode_records_edge_texture_metadata_on_fallback() {
        let fixture = image::open(fixture_path("double_page_story.png")).expect("load fixture");
        let mut config = SplitConfig::default();
        config.confidence_threshold = 1.2; // force algorithms to fall back

        let (meta, fallback) = match super::process_image(&fixture, Path::new("story.png"), config)
        {
            ProcessResult::Split { meta, fallback, .. } => (meta, fallback),
            _ => panic!("expected split outcome"),
        };

        assert!(fallback, "high threshold should force fallback center");
        assert_eq!(meta.split_strategy.as_deref(), Some("fallbackCenter"));
        assert!(
            meta.edge_texture.is_some(),
            "edge texture telemetry missing"
        );
        assert!(
            meta.edge_texture_confidence.is_some(),
            "edge texture confidence should be recorded"
        );
        assert_eq!(
            meta.edge_texture_threshold,
            Some(config.edge_texture.white_threshold)
        );
    }

    #[test]
    fn projection_only_mode_sets_strategy_and_hides_edge_metadata() {
        let fixture = image::open(fixture_path("double_page_story.png")).expect("load fixture");
        let mut config = SplitConfig::default();
        config.mode = SplitModeSelector::ProjectionOnly;

        let (meta, fallback) = match super::process_image(&fixture, Path::new("story.png"), config)
        {
            ProcessResult::Split { meta, fallback, .. } => (meta, fallback),
            _ => panic!("expected split outcome"),
        };

        assert!(!fallback, "projection mode should find confident split");
        assert_eq!(meta.split_strategy.as_deref(), Some("projection"));
        assert!(meta.edge_texture.is_none());
        assert!(meta.edge_texture_confidence.is_none());
        assert!(meta.edge_texture_threshold.is_none());
    }

    #[test]
    fn skip_metadata_serializes_without_split_mode() {
        let metadata = SplitMetadata::with_reason("aspect_ratio");
        let value = serde_json::to_value(&metadata).expect("serialize skip metadata");
        assert!(value.get("splitMode").is_none());
        assert!(value.get("splitStrategy").is_none());
        assert!(value.get("edgeTexture").is_none());
        assert_eq!(
            value.get("reason").and_then(|val| val.as_str()),
            Some("aspect_ratio")
        );
    }

    #[test]
    fn tall_image_skip_has_reason_aspect_ratio() {
        let buffer = image::ImageBuffer::from_pixel(400, 900, image::Rgb([255u8, 255, 255]));
        let image = DynamicImage::ImageRgb8(buffer);
        let outcome = super::process_image(&image, Path::new("dummy.png"), SplitConfig::default());

        match outcome {
            super::ProcessResult::Skip { metadata, .. } => {
                assert_eq!(metadata.reason.as_deref(), Some("aspect_ratio"));
                assert!(metadata.split_mode.is_none());
            }
            _ => panic!("expected skip outcome for tall image"),
        }
    }

    #[test]
    fn clamp_split_to_center_enforces_max_offset() {
        let width = 1920;
        let (clamped, was_clamped) = super::clamp_split_to_center(500, width, 0.12);
        assert!(was_clamped);
        let center = width as f32 / 2.0;
        let max_offset = width as f32 * 0.12;
        let offset = clamped as f32 - center;
        assert!(offset.abs() <= max_offset + 1.0);

        let (unchanged, not_clamped) = super::clamp_split_to_center(900, width, 0.12);
        assert!(!not_clamped);
        assert_eq!(unchanged, 900);
    }
}
