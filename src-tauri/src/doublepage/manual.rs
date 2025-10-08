use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::edge_texture::{
    analyze_edges_with_acceleration, EdgeTextureAccelerator, EdgeTextureAcceleratorPreference,
    EdgeTextureConfig,
};
use super::{SplitItemReport, SplitMetadata, SplitMode};
use chrono::{SecondsFormat, Utc};
use image::{imageops::resize, DynamicImage, GenericImageView, ImageFormat};
use natord::compare;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitContextRequest {
    pub workspace: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitContextEntry {
    pub source_path: PathBuf,
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_lines: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_lines: Option<[f32; 4]>,
    #[serde(default)]
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_applied_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_kind: Option<ManualImageKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate90: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitReportSummary {
    pub generated_at: String,
    pub total: u32,
    pub applied: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualSplitRevertRecord {
    path: PathBuf,
    backup: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualSplitRevertManifest {
    workspace: PathBuf,
    timestamp: String,
    backup_dir: PathBuf,
    created_paths: Vec<PathBuf>,
    original_records: Vec<ManualSplitRevertRecord>,
    overrides_backup: Option<PathBuf>,
    split_report_backup: Option<PathBuf>,
    manual_split_report_backup: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitContext {
    pub workspace: PathBuf,
    pub entries: Vec<ManualSplitContextEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_summary: Option<ManualSplitReportSummary>,
    #[serde(default)]
    pub has_revert_history: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitPreviewRequest {
    pub workspace: PathBuf,
    pub source_path: PathBuf,
    pub lines: [f32; 4],
    #[serde(default)]
    pub target_width: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitPreviewResponse {
    pub source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_preview_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_preview_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gutter_preview_path: Option<PathBuf>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManualImageKind {
    Content,
    Cover,
    Spread,
}

impl Default for ManualImageKind {
    fn default() -> Self {
        ManualImageKind::Content
    }
}

fn default_manual_image_kind() -> ManualImageKind {
    ManualImageKind::Content
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitLine {
    pub source: PathBuf,
    pub left_trim: f32,
    pub left_page_end: f32,
    pub right_page_start: f32,
    pub right_trim: f32,
    #[serde(default)]
    pub gutter_ratio: Option<f32>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "default_manual_image_kind")]
    pub image_kind: ManualImageKind,
    #[serde(default)]
    pub rotate90: bool,
}

fn default_manual_accelerator() -> EdgeTextureAcceleratorPreference {
    EdgeTextureAcceleratorPreference::Auto
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitApplyRequest {
    pub workspace: PathBuf,
    #[serde(default)]
    pub overrides: Vec<ManualSplitLine>,
    #[serde(default = "default_manual_accelerator")]
    pub accelerator: EdgeTextureAcceleratorPreference,
    #[serde(default)]
    pub generate_preview: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitApplyEntry {
    pub source_path: PathBuf,
    pub outputs: Vec<PathBuf>,
    pub applied_at: String,
    pub lines: [f32; 4],
    pub pixels: [u32; 4],
    pub accelerator: EdgeTextureAccelerator,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub image_kind: ManualImageKind,
    pub rotate90: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitApplyResponse {
    pub workspace: PathBuf,
    pub applied: Vec<ManualSplitApplyEntry>,
    pub skipped: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_overrides_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_summary: Option<ManualSplitReportSummary>,
    #[serde(default)]
    pub can_revert: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitRevertRequest {
    pub workspace: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitRevertResponse {
    pub workspace: PathBuf,
    pub restored_outputs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_summary: Option<ManualSplitReportSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitTelemetryRequest {
    pub event: String,
    #[serde(default)]
    pub properties: serde_json::Value,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitTemplateEntry {
    pub source: PathBuf,
    pub lines: [f32; 4],
    #[serde(default)]
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub image_kind: Option<ManualImageKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub rotate90: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitTemplateExportRequest {
    pub workspace: PathBuf,
    pub output_path: PathBuf,
    pub gutter_ratio: f32,
    pub accelerator: String,
    #[serde(default)]
    pub entries: Vec<ManualSplitTemplateEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitTemplateExportResponse {
    pub output_path: PathBuf,
    pub entry_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualSplitReportEntry {
    pub source: PathBuf,
    pub outputs: Vec<PathBuf>,
    pub lines: [f32; 4],
    pub pixels: [u32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gutter_ratio: Option<f32>,
    pub accelerator: String,
    pub width: u32,
    pub height: u32,
    pub applied_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default = "default_manual_image_kind")]
    pub image_kind: ManualImageKind,
    #[serde(default)]
    pub rotate90: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualSplitReportFile {
    pub version: u32,
    pub generated_at: String,
    pub total: usize,
    pub applied: usize,
    pub skipped: usize,
    pub entries: Vec<ManualSplitReportEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareManualSplitWorkspaceRequest {
    pub source_directory: PathBuf,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareManualSplitWorkspaceResponse {
    pub workspace: PathBuf,
    pub entries: Vec<ManualSplitContextEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_split_report_summary: Option<ManualSplitReportSummary>,
    #[serde(default)]
    pub has_revert_history: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitProgress {
    pub workspace: PathBuf,
    pub total: usize,
    pub completed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<PathBuf>,
}

fn read_manual_report_summary(
    path: &Path,
) -> Result<Option<ManualSplitReportSummary>, ManualSplitError> {
    if !path.exists() {
        return Ok(None);
    }

    let data =
        fs::read_to_string(path).map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;
    let report: ManualSplitReportFile =
        serde_json::from_str(&data).map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;

    Ok(Some(ManualSplitReportSummary {
        generated_at: report.generated_at,
        total: report.total.min(u32::MAX as usize) as u32,
        applied: report.applied.min(u32::MAX as usize) as u32,
        skipped: report.skipped.min(u32::MAX as usize) as u32,
    }))
}

fn remove_file_if_exists(path: &Path) -> Result<(), ManualSplitError> {
    match fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(ManualSplitError::RevertRestore(err.to_string())),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitApplyStarted {
    pub workspace: PathBuf,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualSplitApplyFailed {
    pub workspace: PathBuf,
    pub message: String,
}

pub const MANUAL_SPLIT_APPLY_STARTED_EVENT: &str = "manual-split/apply-started";
pub const MANUAL_SPLIT_APPLY_PROGRESS_EVENT: &str = "manual-split/apply-progress";
pub const MANUAL_SPLIT_APPLY_SUCCEEDED_EVENT: &str = "manual-split/apply-succeeded";
pub const MANUAL_SPLIT_APPLY_FAILED_EVENT: &str = "manual-split/apply-failed";
const MANUAL_TELEMETRY_FILENAME: &str = "manual_split_telemetry.jsonl";

const MANUAL_SUPPORTED_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];
const MANUAL_REVERT_MANIFEST: &str = "last_apply.json";

fn is_supported_manual_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| MANUAL_SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn should_descend_manual(entry: &DirEntry) -> bool {
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

fn collect_manual_sources(root: &Path) -> Result<Vec<PathBuf>, ManualSplitError> {
    if !root.exists() {
        return Err(ManualSplitError::SourceDirectoryNotFound(
            root.to_path_buf(),
        ));
    }

    if root.is_file() {
        if is_supported_manual_image(root) {
            let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            return Ok(vec![canonical]);
        }
        return Ok(Vec::new());
    }

    if !root.is_dir() {
        return Err(ManualSplitError::SourceDirectoryNotFound(
            root.to_path_buf(),
        ));
    }

    let mut entries: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_descend_manual)
    {
        let entry = entry.map_err(|err| ManualSplitError::CollectSources(err.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !is_supported_manual_image(path) {
            continue;
        }

        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        entries.push(canonical);
    }

    entries.sort_by(|a, b| compare(a.to_string_lossy().as_ref(), b.to_string_lossy().as_ref()));
    entries.dedup();

    Ok(entries)
}

pub fn prepare_manual_split_workspace(
    request: PrepareManualSplitWorkspaceRequest,
) -> Result<PrepareManualSplitWorkspaceResponse, ManualSplitError> {
    let mut source_root = request.source_directory.clone();
    if !source_root.exists() {
        return Err(ManualSplitError::SourceDirectoryNotFound(source_root));
    }
    source_root = fs::canonicalize(&source_root).unwrap_or(source_root);

    let workspace_root = if let Some(root) = request.workspace_root.as_ref() {
        if root.is_absolute() {
            root.clone()
        } else {
            source_root.join(root)
        }
    } else {
        source_root.join("split-manual")
    };

    if workspace_root.exists() && !request.overwrite {
        if let Ok(context) = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace_root.clone(),
        }) {
            let ManualSplitContext {
                workspace,
                entries,
                manual_split_report_path,
                manual_split_report_summary,
                has_revert_history,
            } = context;
            return Ok(PrepareManualSplitWorkspaceResponse {
                workspace,
                entries,
                manual_split_report_path,
                manual_split_report_summary,
                has_revert_history,
            });
        }
        fs::remove_dir_all(&workspace_root)
            .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    }

    if request.overwrite && workspace_root.exists() {
        fs::remove_dir_all(&workspace_root)
            .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    }

    fs::create_dir_all(&workspace_root)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let manual_dir = workspace_root.join("manual");
    fs::create_dir_all(&manual_dir)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let overrides_dir = workspace_root.join("manual-overrides");
    fs::create_dir_all(&overrides_dir)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    fs::create_dir_all(overrides_dir.join("previews"))
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    fs::create_dir_all(overrides_dir.join("backups"))
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let sources = collect_manual_sources(&source_root)?;

    let mut context_entries: Vec<ManualSplitContextEntry> = Vec::new();
    let mut report_items: Vec<SplitItemReport> = Vec::new();

    for source in sources {
        let image =
            image::open(&source).map_err(|err| ManualSplitError::ImageRead(err.to_string()))?;
        let (width, height) = image.dimensions();

        let recommended_lines = compute_recommended_lines(None, width);

        let display_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| source.to_string_lossy().to_string());

        context_entries.push(ManualSplitContextEntry {
            source_path: source.clone(),
            display_name,
            width,
            height,
            recommended_lines,
            existing_lines: None,
            locked: false,
            last_applied_at: None,
            thumbnail_path: None,
            image_kind: Some(ManualImageKind::Content),
            rotate90: Some(false),
        });

        report_items.push(SplitItemReport {
            source: source.clone(),
            mode: SplitMode::Manual,
            split_x: None,
            confidence: 0.0,
            content_width_ratio: 1.0,
            outputs: Vec::new(),
            metadata: SplitMetadata::default(),
        });
    }

    let report = SplitReportFull {
        generated_at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
        items: report_items,
    };

    let report_json = serde_json::to_string_pretty(&report)
        .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;
    fs::write(
        workspace_root.join("split-report.json"),
        format!("{}\n", report_json),
    )
    .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;

    let overrides = ManualOverridesFile::default();
    let overrides_json = serde_json::to_string_pretty(&overrides)
        .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?;
    fs::write(
        overrides_dir.join("manual_overrides.json"),
        format!("{}\n", overrides_json),
    )
    .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?;

    let resolved_workspace = fs::canonicalize(&workspace_root).unwrap_or(workspace_root);

    Ok(PrepareManualSplitWorkspaceResponse {
        workspace: resolved_workspace,
        entries: context_entries,
        manual_split_report_path: None,
        manual_split_report_summary: None,
        has_revert_history: false,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitReportFile {
    #[allow(dead_code)]
    generated_at: Option<String>,
    items: Vec<SplitReportItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitReportItem {
    source: PathBuf,
    #[serde(default)]
    split_x: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitReportFull {
    generated_at: Option<String>,
    items: Vec<SplitItemReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManualOverridesFile {
    #[serde(default = "default_overrides_version")]
    pub version: u32,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub entries: Vec<ManualOverrideEntry>,
}

fn default_overrides_version() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualOverrideEntry {
    pub source: PathBuf,
    pub width: u32,
    pub height: u32,
    pub lines: [f32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixels: Option<[u32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gutter_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<EdgeTextureAcceleratorPreference>,
    #[serde(default)]
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_applied_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<PathBuf>>,
    #[serde(default = "default_manual_image_kind")]
    pub image_kind: ManualImageKind,
    #[serde(default)]
    pub rotate90: bool,
}

struct ManualWorkItem {
    source: PathBuf,
    lines: [f32; 4],
    locked: bool,
    gutter_ratio: Option<f32>,
    image_kind: ManualImageKind,
    rotate90: bool,
}

struct ManualOutputPaths {
    left_root: PathBuf,
    right_root: PathBuf,
    left_manual: PathBuf,
    right_manual: PathBuf,
}

struct ManualSingleOutputPaths {
    root: PathBuf,
    manual: PathBuf,
}

#[derive(Debug, Error)]
pub enum ManualSplitError {
    #[error("source directory not found: {0}")]
    SourceDirectoryNotFound(PathBuf),
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(PathBuf),
    #[error("failed to read split report: {0}")]
    ReportRead(String),
    #[error("failed to read source image: {0}")]
    ImageRead(String),
    #[error("failed to write preview: {0}")]
    PreviewWrite(String),
    #[error("invalid manual override: {0}")]
    InvalidOverrides(String),
    #[error("failed to write manual output: {0}")]
    OutputWrite(String),
    #[error("failed to update split report: {0}")]
    ReportWrite(String),
    #[error("failed to write manual overrides: {0}")]
    OverridesWrite(String),
    #[error("failed to collect manual sources: {0}")]
    CollectSources(String),
    #[error("manual split revert manifest missing at {0}")]
    RevertManifestMissing(PathBuf),
    #[error("failed to read manual split revert manifest: {0}")]
    RevertManifestRead(String),
    #[error("failed to parse manual split revert manifest: {0}")]
    RevertManifestParse(String),
    #[error("failed to write manual split revert manifest: {0}")]
    RevertManifestWrite(String),
    #[error("failed to restore manual split backup: {0}")]
    RevertRestore(String),
    #[error("failed to record telemetry: {0}")]
    TelemetryWrite(String),
    #[error("failed to write manual split template: {0}")]
    TemplateWrite(String),
}

pub fn load_manual_split_context(
    request: ManualSplitContextRequest,
) -> Result<ManualSplitContext, ManualSplitError> {
    if !request.workspace.exists() {
        return Err(ManualSplitError::WorkspaceNotFound(
            request.workspace.clone(),
        ));
    }

    let report_path = request.workspace.join("split-report.json");
    let overrides_path = request
        .workspace
        .join("manual-overrides")
        .join("manual_overrides.json");
    let manual_report_path = request.workspace.join("manual_split_report.json");
    let manual_report_summary = read_manual_report_summary(&manual_report_path)?;
    let has_revert_history = request
        .workspace
        .join("manual-overrides")
        .join("backups")
        .join(MANUAL_REVERT_MANIFEST)
        .exists();

    let mut overrides_map: HashMap<PathBuf, ManualOverrideEntry> = HashMap::new();
    if overrides_path.exists() {
        let mut buffer = String::new();
        if let Err(err) =
            fs::File::open(&overrides_path).and_then(|mut file| file.read_to_string(&mut buffer))
        {
            return Err(ManualSplitError::ReportRead(err.to_string()));
        }
        match serde_json::from_str::<ManualOverridesFile>(&buffer) {
            Ok(file) => {
                for entry in file.entries {
                    overrides_map.insert(entry.source.clone(), entry);
                }
            }
            Err(err) => {
                return Err(ManualSplitError::ReportRead(err.to_string()));
            }
        }
    }

    if !report_path.exists() {
        return Ok(ManualSplitContext {
            workspace: request.workspace,
            entries: Vec::new(),
            manual_split_report_path: manual_report_summary
                .as_ref()
                .map(|_| manual_report_path.clone()),
            manual_split_report_summary: manual_report_summary,
            has_revert_history,
        });
    }

    let report_data = fs::read_to_string(&report_path)
        .map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;
    let report: SplitReportFile = serde_json::from_str(&report_data)
        .map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;

    let mut entries: Vec<ManualSplitContextEntry> = Vec::new();

    for report_item in report.items {
        let SplitReportItem { source, split_x } = report_item;
        if !source.exists() {
            continue;
        }
        let image =
            image::open(&source).map_err(|err| ManualSplitError::ImageRead(err.to_string()))?;
        let (width, height) = image.dimensions();

        let override_entry = overrides_map.get(&source);
        let existing_lines = override_entry.map(|entry| entry.lines);
        let locked = override_entry.map(|entry| entry.locked).unwrap_or(false);
        let last_applied_at = override_entry.and_then(|entry| entry.last_applied_at.clone());
        let thumbnail_path = override_entry.and_then(|entry| entry.thumbnail_path.clone());
        let (image_kind, rotate90) = override_entry
            .map(|entry| (entry.image_kind, entry.rotate90))
            .unwrap_or((ManualImageKind::Content, false));

        let recommended_lines = compute_recommended_lines(split_x, width);

        let display_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| source.to_string_lossy().to_string());

        entries.push(ManualSplitContextEntry {
            source_path: source,
            display_name,
            width,
            height,
            recommended_lines,
            existing_lines,
            locked,
            last_applied_at,
            thumbnail_path,
            image_kind: Some(image_kind),
            rotate90: Some(rotate90),
        });
    }

    entries.sort_by(|a, b| a.source_path.cmp(&b.source_path));

    Ok(ManualSplitContext {
        workspace: request.workspace,
        entries,
        manual_split_report_path: manual_report_summary.as_ref().map(|_| manual_report_path),
        manual_split_report_summary: manual_report_summary,
        has_revert_history,
    })
}

pub fn render_manual_split_preview(
    request: ManualSplitPreviewRequest,
) -> Result<ManualSplitPreviewResponse, ManualSplitError> {
    if !request.source_path.exists() {
        return Err(ManualSplitError::ImageRead(format!(
            "source not found: {}",
            request.source_path.display()
        )));
    }

    let image = image::open(&request.source_path)
        .map_err(|err| ManualSplitError::ImageRead(err.to_string()))?;
    let (width, height) = image.dimensions();

    let (left_trim_px, left_page_end_px, right_page_start_px, right_trim_px) =
        normalize_lines(request.lines, width);

    let mut left_preview_path = None;
    let mut right_preview_path = None;
    let mut gutter_preview_path = None;

    let previews_dir = request.workspace.join("manual-overrides").join("previews");
    if let Err(err) = fs::create_dir_all(&previews_dir) {
        return Err(ManualSplitError::PreviewWrite(err.to_string()));
    }

    let base_name = request
        .source_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("page");

    let digest = preview_digest(&request.source_path, &request.lines);

    if let Some(path) = crop_and_save(
        &image,
        left_trim_px,
        0,
        left_page_end_px.saturating_sub(left_trim_px).max(1),
        height,
        &previews_dir,
        base_name,
        digest,
        "left",
        request.target_width,
    )? {
        left_preview_path = Some(path);
    }

    if let Some(path) = crop_and_save(
        &image,
        right_page_start_px,
        0,
        right_trim_px.saturating_sub(right_page_start_px).max(1),
        height,
        &previews_dir,
        base_name,
        digest,
        "right",
        request.target_width,
    )? {
        right_preview_path = Some(path);
    }

    if right_page_start_px > left_page_end_px {
        if let Some(path) = crop_and_save(
            &image,
            left_page_end_px,
            0,
            right_page_start_px - left_page_end_px,
            height,
            &previews_dir,
            base_name,
            digest,
            "gutter",
            request.target_width,
        )? {
            gutter_preview_path = Some(path);
        }
    }

    let generated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    Ok(ManualSplitPreviewResponse {
        source_path: request.source_path,
        left_preview_path,
        right_preview_path,
        gutter_preview_path,
        generated_at,
    })
}

pub fn apply_manual_splits(
    request: ManualSplitApplyRequest,
    mut progress: Option<&mut dyn FnMut(ManualSplitProgress)>,
) -> Result<ManualSplitApplyResponse, ManualSplitError> {
    if !request.workspace.exists() {
        return Err(ManualSplitError::WorkspaceNotFound(request.workspace));
    }

    let ManualSplitApplyRequest {
        workspace,
        overrides,
        accelerator,
        generate_preview: _,
    } = request;

    if overrides.is_empty() {
        return Err(ManualSplitError::InvalidOverrides(
            "no overrides provided".to_string(),
        ));
    }

    let workspace = fs::canonicalize(&workspace).unwrap_or(workspace);
    let manual_dir = workspace.join("manual");
    let overrides_dir = workspace.join("manual-overrides");
    let manual_overrides_path = overrides_dir.join("manual_overrides.json");
    let split_report_path = workspace.join("split-report.json");
    let manual_split_report_path = workspace.join("manual_split_report.json");

    if !split_report_path.exists() {
        return Err(ManualSplitError::ReportRead(format!(
            "split report not found at {}",
            split_report_path.display()
        )));
    }

    fs::create_dir_all(&manual_dir)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    fs::create_dir_all(&overrides_dir)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let mut unique_sources: HashSet<PathBuf> = HashSet::new();
    let mut work_items: Vec<ManualWorkItem> = Vec::new();

    for override_line in overrides {
        let source_candidate = if override_line.source.is_relative() {
            workspace.join(&override_line.source)
        } else {
            override_line.source.clone()
        };

        let source_path = fs::canonicalize(&source_candidate).unwrap_or(source_candidate);
        if !source_path.exists() {
            return Err(ManualSplitError::ImageRead(format!(
                "source not found: {}",
                source_path.display()
            )));
        }

        if !unique_sources.insert(source_path.clone()) {
            continue;
        }

        work_items.push(ManualWorkItem {
            source: source_path,
            lines: [
                override_line.left_trim,
                override_line.left_page_end,
                override_line.right_page_start,
                override_line.right_trim,
            ],
            locked: override_line.locked,
            gutter_ratio: override_line.gutter_ratio,
            image_kind: override_line.image_kind,
            rotate90: override_line.rotate90,
        });
    }

    if work_items.is_empty() {
        return Err(ManualSplitError::InvalidOverrides(
            "no usable overrides provided".to_string(),
        ));
    }

    let total = work_items.len();
    emit_manual_progress(&mut progress, &workspace, total, 0, None);

    let backups_root = overrides_dir.join(".tmp");
    fs::create_dir_all(&backups_root)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
    let backup_stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = backups_root.join(&backup_stamp);
    fs::create_dir_all(&backup_dir)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let mut backup_overrides_path: Option<PathBuf> = None;
    if manual_overrides_path.exists() {
        let backup_path = backup_dir.join("manual_overrides.json");
        fs::copy(&manual_overrides_path, &backup_path)
            .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
        backup_overrides_path = Some(backup_path);
    }

    let backup_report_path = backup_dir.join("split-report.json");
    fs::copy(&split_report_path, &backup_report_path)
        .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;

    let mut backup_manual_report_path: Option<PathBuf> = None;
    if manual_split_report_path.exists() {
        let backup_path = backup_dir.join("manual_split_report.json");
        fs::copy(&manual_split_report_path, &backup_path)
            .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
        backup_manual_report_path = Some(backup_path);
    }

    let mut overrides_file = if manual_overrides_path.exists() {
        let data = fs::read_to_string(&manual_overrides_path)
            .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?;
        serde_json::from_str::<ManualOverridesFile>(&data)
            .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?
    } else {
        ManualOverridesFile::default()
    };

    let report_data = fs::read_to_string(&split_report_path)
        .map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;
    let mut report: SplitReportFull = serde_json::from_str(&report_data)
        .map_err(|err| ManualSplitError::ReportRead(err.to_string()))?;

    let mut applied_entries: Vec<ManualSplitApplyEntry> = Vec::new();
    let mut skipped: Vec<PathBuf> = Vec::new();
    let mut backup_records: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created_paths: Vec<PathBuf> = Vec::new();
    let mut report_entries: Vec<ManualSplitReportEntry> = Vec::new();

    let mut manual_report_summary: Option<ManualSplitReportSummary> = None;
    let edge_config = EdgeTextureConfig::default();

    let process_result: Result<(), ManualSplitError> = (|| {
        for (index, item) in work_items.iter().enumerate() {
            emit_manual_progress(&mut progress, &workspace, total, index, Some(&item.source));

            let iteration_start = Instant::now();

            let image = match image::open(&item.source) {
                Ok(img) => img,
                Err(_) => {
                    skipped.push(item.source.clone());
                    emit_manual_progress(
                        &mut progress,
                        &workspace,
                        total,
                        index + 1,
                        Some(&item.source),
                    );
                    continue;
                }
            };

            let (width, height) = image.dimensions();
            if width == 0 || height == 0 {
                skipped.push(item.source.clone());
                emit_manual_progress(
                    &mut progress,
                    &workspace,
                    total,
                    index + 1,
                    Some(&item.source),
                );
                continue;
            }

            let (left_trim, left_end, right_start, right_trim) = normalize_lines(item.lines, width);
            if left_end <= left_trim || right_trim <= right_start {
                skipped.push(item.source.clone());
                emit_manual_progress(
                    &mut progress,
                    &workspace,
                    total,
                    index + 1,
                    Some(&item.source),
                );
                continue;
            }

            let accelerator_used = match accelerator {
                EdgeTextureAcceleratorPreference::Cpu => EdgeTextureAccelerator::Cpu,
                _ => analyze_edges_with_acceleration(&image, edge_config, accelerator).accelerator,
            };
            let accelerator_label = match accelerator_used {
                EdgeTextureAccelerator::Cpu => "cpu".to_string(),
                EdgeTextureAccelerator::Gpu => "gpu".to_string(),
            };

            let mut created_this_round: Vec<PathBuf> = Vec::new();
            let outputs_for_entry: Vec<PathBuf> = match item.image_kind {
                ManualImageKind::Content => {
                    let outputs = derive_manual_output_paths(&workspace, &manual_dir, &item.source)
                        .map_err(ManualSplitError::InvalidOverrides)?;

                    for path in [
                        &outputs.left_root,
                        &outputs.right_root,
                        &outputs.left_manual,
                        &outputs.right_manual,
                    ] {
                        if let Some(backup) = backup_existing_file(path, &backup_dir)? {
                            backup_records.push((path.clone(), backup));
                        }
                    }

                    let left_width = left_end.saturating_sub(left_trim).max(1);
                    let right_width = right_trim.saturating_sub(right_start).max(1);

                    if let Err(err) = image
                        .crop_imm(left_trim, 0, left_width, height)
                        .save(&outputs.left_manual)
                    {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(outputs.left_manual.clone());

                    if let Err(err) = image
                        .crop_imm(right_start, 0, right_width, height)
                        .save(&outputs.right_manual)
                    {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(outputs.right_manual.clone());

                    if let Err(err) = fs::copy(&outputs.left_manual, &outputs.left_root) {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(outputs.left_root.clone());

                    if let Err(err) = fs::copy(&outputs.right_manual, &outputs.right_root) {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(outputs.right_root.clone());

                    vec![outputs.right_root.clone(), outputs.left_root.clone()]
                }
                ManualImageKind::Cover | ManualImageKind::Spread => {
                    let single_paths = derive_manual_single_output_paths(
                        &workspace,
                        &manual_dir,
                        &item.source,
                        item.image_kind,
                    )
                    .map_err(ManualSplitError::InvalidOverrides)?;

                    for path in [&single_paths.root, &single_paths.manual] {
                        if let Some(backup) = backup_existing_file(path, &backup_dir)? {
                            backup_records.push((path.clone(), backup));
                        }
                    }

                    let single_width = right_trim.saturating_sub(left_trim).max(1);
                    let single_image = image.crop_imm(left_trim, 0, single_width, height);
                    let final_single = if item.rotate90 {
                        single_image.rotate90()
                    } else {
                        single_image
                    };

                    if let Err(err) = final_single.save(&single_paths.manual) {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(single_paths.manual.clone());

                    if let Err(err) = fs::copy(&single_paths.manual, &single_paths.root) {
                        cleanup_files(&created_this_round);
                        return Err(ManualSplitError::OutputWrite(err.to_string()));
                    }
                    created_this_round.push(single_paths.root.clone());

                    vec![single_paths.root.clone()]
                }
            };

            created_paths.extend(created_this_round.iter().cloned());

            let width_f = width as f32;
            let pixel_values = match item.image_kind {
                ManualImageKind::Content => [left_trim, left_end, right_start, right_trim],
                ManualImageKind::Cover | ManualImageKind::Spread => {
                    [left_trim, left_trim, right_trim, right_trim]
                }
            };
            let sanitized = [
                (pixel_values[0] as f32 / width_f).clamp(0.0, 1.0),
                (pixel_values[1] as f32 / width_f).clamp(0.0, 1.0),
                (pixel_values[2] as f32 / width_f).clamp(0.0, 1.0),
                (pixel_values[3] as f32 / width_f).clamp(0.0, 1.0),
            ];

            let gutter_ratio = match item.image_kind {
                ManualImageKind::Content => item.gutter_ratio.or_else(|| {
                    if right_start > left_end {
                        Some((right_start - left_end) as f32 / width_f)
                    } else {
                        None
                    }
                }),
                ManualImageKind::Cover | ManualImageKind::Spread => item.gutter_ratio,
            };

            let applied_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let duration_ms = iteration_start.elapsed().as_millis() as u64;

            overrides_file
                .entries
                .retain(|entry| entry.source != item.source);
            overrides_file.entries.push(ManualOverrideEntry {
                source: item.source.clone(),
                width,
                height,
                lines: sanitized,
                pixels: Some(pixel_values),
                gutter_ratio,
                accelerator: Some(accelerator),
                locked: item.locked,
                last_applied_at: Some(applied_at.clone()),
                thumbnail_path: None,
                outputs: Some(outputs_for_entry.clone()),
                image_kind: item.image_kind,
                rotate90: item.rotate90,
            });

            let apply_metadata = |metadata: &mut SplitMetadata| {
                metadata.manual_lines = Some(pixel_values);
                metadata.manual_percentages = Some(sanitized);
                metadata.manual_source = Some("user".to_string());
                metadata.manual_applied_at = Some(applied_at.clone());
                metadata.manual_accelerator = Some(accelerator_label.clone());
                metadata.split_mode = Some(SplitMode::Manual);
                metadata.manual_image_kind = Some(item.image_kind);
                metadata.manual_rotate90 = Some(item.rotate90);
            };

            if let Some(entry) = report
                .items
                .iter_mut()
                .find(|existing| existing.source == item.source)
            {
                entry.mode = SplitMode::Manual;
                entry.split_x = if matches!(item.image_kind, ManualImageKind::Content) {
                    Some(right_start)
                } else {
                    None
                };
                entry.confidence = 1.0;
                entry.content_width_ratio =
                    (right_trim.saturating_sub(left_trim) as f32 / width_f).clamp(0.0, 1.0);
                entry.outputs = outputs_for_entry.clone();
                apply_metadata(&mut entry.metadata);
            } else {
                let mut metadata = SplitMetadata::default();
                apply_metadata(&mut metadata);
                report.items.push(SplitItemReport {
                    source: item.source.clone(),
                    mode: SplitMode::Manual,
                    split_x: if matches!(item.image_kind, ManualImageKind::Content) {
                        Some(right_start)
                    } else {
                        None
                    },
                    confidence: 1.0,
                    content_width_ratio: (right_trim.saturating_sub(left_trim) as f32 / width_f)
                        .clamp(0.0, 1.0),
                    outputs: outputs_for_entry.clone(),
                    metadata,
                });
            }

            applied_entries.push(ManualSplitApplyEntry {
                source_path: item.source.clone(),
                outputs: outputs_for_entry.clone(),
                applied_at: applied_at.clone(),
                lines: sanitized,
                pixels: pixel_values,
                accelerator: accelerator_used,
                width,
                height,
                duration_ms: Some(duration_ms),
                image_kind: item.image_kind,
                rotate90: item.rotate90,
            });

            report_entries.push(ManualSplitReportEntry {
                source: item.source.clone(),
                outputs: outputs_for_entry.clone(),
                lines: sanitized,
                pixels: pixel_values,
                gutter_ratio,
                accelerator: accelerator_label,
                width,
                height,
                applied_at: applied_at.clone(),
                duration_ms: Some(duration_ms),
                image_kind: item.image_kind,
                rotate90: item.rotate90,
            });

            emit_manual_progress(
                &mut progress,
                &workspace,
                total,
                index + 1,
                Some(&item.source),
            );
        }

        overrides_file.updated_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));

        let overrides_json = serde_json::to_string_pretty(&overrides_file)
            .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?;
        fs::write(&manual_overrides_path, format!("{}\n", overrides_json))
            .map_err(|err| ManualSplitError::OverridesWrite(err.to_string()))?;

        let report_json = serde_json::to_string_pretty(&report)
            .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;
        fs::write(&split_report_path, format!("{}\n", report_json))
            .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;

        let manual_report = ManualSplitReportFile {
            version: 2,
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            total,
            applied: applied_entries.len(),
            skipped: skipped.len(),
            entries: report_entries.clone(),
        };

        let manual_report_json = serde_json::to_string_pretty(&manual_report)
            .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;
        fs::write(
            &manual_split_report_path,
            format!("{}\n", manual_report_json),
        )
        .map_err(|err| ManualSplitError::ReportWrite(err.to_string()))?;

        manual_report_summary = Some(ManualSplitReportSummary {
            generated_at: manual_report.generated_at.clone(),
            total: total.min(u32::MAX as usize) as u32,
            applied: applied_entries.len().min(u32::MAX as usize) as u32,
            skipped: skipped.len().min(u32::MAX as usize) as u32,
        });

        emit_manual_progress(&mut progress, &workspace, total, total, None);

        Ok(())
    })();

    if let Err(err) = process_result {
        for path in created_paths.iter().rev() {
            let _ = fs::remove_file(path);
        }

        for (original, backup) in backup_records.iter().rev() {
            let _ = fs::copy(backup, original);
        }

        if let Some(backup_path) = backup_overrides_path {
            let _ = fs::copy(&backup_path, &manual_overrides_path);
        } else {
            let _ = fs::remove_file(&manual_overrides_path);
        }

        let _ = fs::copy(&backup_report_path, &split_report_path);

        if let Some(backup_path) = backup_manual_report_path {
            let _ = fs::copy(&backup_path, &manual_split_report_path);
        } else {
            let _ = fs::remove_file(&manual_split_report_path);
        }

        emit_manual_progress(&mut progress, &workspace, total, total, None);

        return Err(err);
    }

    let can_revert = !applied_entries.is_empty();

    if can_revert {
        fs::create_dir_all(overrides_dir.join("backups"))
            .map_err(|err| ManualSplitError::RevertManifestWrite(err.to_string()))?;
        let manifest = ManualSplitRevertManifest {
            workspace: workspace.clone(),
            timestamp: backup_stamp,
            backup_dir: backup_dir.clone(),
            created_paths: created_paths.clone(),
            original_records: backup_records
                .iter()
                .map(|(path, backup)| ManualSplitRevertRecord {
                    path: path.clone(),
                    backup: backup.clone(),
                })
                .collect(),
            overrides_backup: backup_overrides_path.clone(),
            split_report_backup: Some(backup_report_path.clone()),
            manual_split_report_backup: backup_manual_report_path.clone(),
        };

        let manifest_path = overrides_dir.join("backups").join(MANUAL_REVERT_MANIFEST);
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|err| ManualSplitError::RevertManifestWrite(err.to_string()))?;
        fs::write(&manifest_path, format!("{}\n", manifest_json))
            .map_err(|err| ManualSplitError::RevertManifestWrite(err.to_string()))?;
    }

    Ok(ManualSplitApplyResponse {
        workspace: workspace.clone(),
        applied: applied_entries,
        skipped,
        manual_overrides_path: Some(manual_overrides_path),
        split_report_path: Some(split_report_path),
        manual_split_report_path: Some(manual_split_report_path),
        manual_split_report_summary: manual_report_summary,
        can_revert,
    })
}

pub fn revert_manual_splits(
    request: ManualSplitRevertRequest,
) -> Result<ManualSplitRevertResponse, ManualSplitError> {
    if !request.workspace.exists() {
        return Err(ManualSplitError::WorkspaceNotFound(request.workspace));
    }

    let resolved_workspace =
        fs::canonicalize(&request.workspace).unwrap_or(request.workspace.clone());
    let overrides_dir = resolved_workspace.join("manual-overrides");
    let manifest_path = overrides_dir.join("backups").join(MANUAL_REVERT_MANIFEST);

    if !manifest_path.exists() {
        return Err(ManualSplitError::RevertManifestMissing(manifest_path));
    }

    let manifest_data = fs::read_to_string(&manifest_path)
        .map_err(|err| ManualSplitError::RevertManifestRead(err.to_string()))?;
    let manifest: ManualSplitRevertManifest = serde_json::from_str(&manifest_data)
        .map_err(|err| ManualSplitError::RevertManifestParse(err.to_string()))?;

    let manifest_workspace =
        fs::canonicalize(&manifest.workspace).unwrap_or(manifest.workspace.clone());
    if manifest_workspace != resolved_workspace {
        return Err(ManualSplitError::RevertRestore(format!(
            "revert manifest workspace mismatch: {} vs {}",
            manifest.workspace.display(),
            resolved_workspace.display()
        )));
    }

    for created in &manifest.created_paths {
        remove_file_if_exists(created)?;
    }

    let mut restored_outputs = 0usize;
    for record in &manifest.original_records {
        if !record.backup.exists() {
            return Err(ManualSplitError::RevertRestore(format!(
                "backup file missing: {}",
                record.backup.display()
            )));
        }
        if let Some(parent) = record.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| ManualSplitError::RevertRestore(err.to_string()))?;
        }
        fs::copy(&record.backup, &record.path)
            .map_err(|err| ManualSplitError::RevertRestore(err.to_string()))?;
        restored_outputs += 1;
    }

    let manual_overrides_path = overrides_dir.join("manual_overrides.json");
    match manifest.overrides_backup.as_ref() {
        Some(backup) => {
            fs::copy(backup, &manual_overrides_path)
                .map_err(|err| ManualSplitError::RevertRestore(err.to_string()))?;
        }
        None => {
            if manual_overrides_path.exists() {
                remove_file_if_exists(&manual_overrides_path)?;
            }
        }
    }

    let split_report_path = resolved_workspace.join("split-report.json");
    if let Some(backup) = manifest.split_report_backup.as_ref() {
        fs::copy(backup, &split_report_path)
            .map_err(|err| ManualSplitError::RevertRestore(err.to_string()))?;
    }

    let manual_report_path = resolved_workspace.join("manual_split_report.json");
    match manifest.manual_split_report_backup.as_ref() {
        Some(backup) => {
            fs::copy(backup, &manual_report_path)
                .map_err(|err| ManualSplitError::RevertRestore(err.to_string()))?;
        }
        None => {
            remove_file_if_exists(&manual_report_path)?;
        }
    }

    let _ = fs::remove_file(&manifest_path);

    let manual_report_summary = read_manual_report_summary(&manual_report_path)?;
    let manual_report_path_opt = manual_report_summary.as_ref().map(|_| manual_report_path);

    Ok(ManualSplitRevertResponse {
        workspace: resolved_workspace,
        restored_outputs,
        manual_split_report_path: manual_report_path_opt,
        manual_split_report_summary: manual_report_summary,
    })
}

pub fn export_manual_split_template(
    request: ManualSplitTemplateExportRequest,
) -> Result<ManualSplitTemplateExportResponse, ManualSplitError> {
    if request.entries.is_empty() {
        return Err(ManualSplitError::InvalidOverrides(
            "no template entries provided".to_string(),
        ));
    }

    if !request.workspace.exists() {
        return Err(ManualSplitError::WorkspaceNotFound(request.workspace));
    }

    let ManualSplitTemplateExportRequest {
        workspace,
        output_path,
        gutter_ratio,
        accelerator,
        entries,
    } = request;

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| ManualSplitError::TemplateWrite(err.to_string()))?;
    }

    let rendered_entries: Vec<ManualSplitTemplateEntry> = entries;
    let entry_count = rendered_entries.len();
    let payload = serde_json::json!({
        "generatedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        "workspace": workspace,
        "accelerator": accelerator,
        "gutterRatio": gutter_ratio,
        "entryCount": entry_count,
        "entries": rendered_entries,
    });

    let serialized = serde_json::to_string_pretty(&payload)
        .map_err(|err| ManualSplitError::TemplateWrite(err.to_string()))?;
    fs::write(&output_path, serialized)
        .map_err(|err| ManualSplitError::TemplateWrite(err.to_string()))?;

    Ok(ManualSplitTemplateExportResponse {
        output_path,
        entry_count,
    })
}

pub fn track_manual_split_event(
    request: ManualSplitTelemetryRequest,
) -> Result<(), ManualSplitError> {
    let ManualSplitTelemetryRequest {
        event,
        properties,
        workspace,
    } = request;

    if event.trim().is_empty() {
        return Ok(());
    }

    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
    let record = serde_json::json!({
        "timestamp": timestamp,
        "event": event,
        "properties": properties,
    });

    if let Some(workspace) = workspace {
        let log_dir = workspace.join("manual-overrides");
        fs::create_dir_all(&log_dir)
            .map_err(|err| ManualSplitError::TelemetryWrite(err.to_string()))?;
        let log_path = log_dir.join(MANUAL_TELEMETRY_FILENAME);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|err| ManualSplitError::TelemetryWrite(err.to_string()))?;
        let mut line = serde_json::to_string(&record)
            .map_err(|err| ManualSplitError::TelemetryWrite(err.to_string()))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(|err| ManualSplitError::TelemetryWrite(err.to_string()))?;
    }

    Ok(())
}

fn cleanup_files(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}

fn emit_manual_progress(
    progress: &mut Option<&mut dyn FnMut(ManualSplitProgress)>,
    workspace: &Path,
    total: usize,
    completed: usize,
    current: Option<&Path>,
) {
    if let Some(callback) = progress.as_mut() {
        callback(ManualSplitProgress {
            workspace: workspace.to_path_buf(),
            total,
            completed,
            current: current.map(|value| value.to_path_buf()),
        });
    }
}

fn derive_manual_output_paths(
    workspace: &Path,
    manual_dir: &Path,
    source: &Path,
) -> Result<ManualOutputPaths, String> {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("invalid source filename: {}", source.display()))?;
    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value))
        .unwrap_or_else(String::new);

    let right_name = format!("{}_R{}", stem, ext);
    let left_name = format!("{}_L{}", stem, ext);
    let manual_right_name = format!("{}_manual_R{}", stem, ext);
    let manual_left_name = format!("{}_manual_L{}", stem, ext);

    Ok(ManualOutputPaths {
        left_root: workspace.join(left_name),
        right_root: workspace.join(right_name),
        left_manual: manual_dir.join(manual_left_name),
        right_manual: manual_dir.join(manual_right_name),
    })
}

fn derive_manual_single_output_paths(
    workspace: &Path,
    manual_dir: &Path,
    source: &Path,
    kind: ManualImageKind,
) -> Result<ManualSingleOutputPaths, String> {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("invalid source filename: {}", source.display()))?;
    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value))
        .unwrap_or_else(String::new);

    let suffix = match kind {
        ManualImageKind::Cover => "cover",
        ManualImageKind::Spread => "spread",
        ManualImageKind::Content => {
            return Err(format!(
                "unsupported image kind for single output: {:?}",
                kind
            ))
        }
    };

    let root_name = format!("{}_{}{}", stem, suffix, ext);
    let manual_name = format!("{}_manual_{}{}", stem, suffix, ext);

    Ok(ManualSingleOutputPaths {
        root: workspace.join(root_name),
        manual: manual_dir.join(manual_name),
    })
}

fn backup_existing_file(
    path: &Path,
    backup_dir: &Path,
) -> Result<Option<PathBuf>, ManualSplitError> {
    if !path.exists() {
        return Ok(None);
    }

    if let Some(file_name) = path.file_name() {
        let backup_path = backup_dir.join(file_name);
        fs::copy(path, &backup_path)
            .map_err(|err| ManualSplitError::OutputWrite(err.to_string()))?;
        Ok(Some(backup_path))
    } else {
        Ok(None)
    }
}

fn compute_recommended_lines(split_x: Option<u32>, width: u32) -> Option<[f32; 4]> {
    let width_f = width.max(1) as f32;
    let center_ratio = split_x
        .map(|value| (value as f32 / width_f).clamp(0.0, 1.0))
        .unwrap_or(0.5);

    let gutter = 0.02;
    let left_trim = 0.02;
    let mut left_page_end = (center_ratio - gutter / 2.0).clamp(0.05, 0.95);
    let mut right_page_start = (center_ratio + gutter / 2.0).clamp(0.05, 0.95);

    if right_page_start - left_page_end < gutter {
        let adjust = (gutter - (right_page_start - left_page_end)) / 2.0;
        left_page_end = (left_page_end - adjust).clamp(0.05, 0.95);
        right_page_start = (right_page_start + adjust).clamp(0.05, 0.95);
    }

    let right_trim = 0.98;

    Some([left_trim, left_page_end, right_page_start, right_trim])
}

fn normalize_lines(lines: [f32; 4], width: u32) -> (u32, u32, u32, u32) {
    let clamp = |value: f32| value.clamp(0.0, 1.0);
    let [a, b, c, d] = lines;
    let left_trim = clamp(a);
    let mut left_end = clamp(b).max(left_trim);
    let mut right_start = clamp(c).max(left_end);
    let mut right_trim = clamp(d).max(right_start);

    if left_trim >= 1.0 {
        return (width, width, width, width);
    }

    if left_end <= left_trim {
        left_end = (left_trim + 0.02).min(1.0);
    }

    if right_start <= left_end {
        right_start = (left_end + 0.02).min(1.0);
    }

    if right_trim <= right_start {
        right_trim = (right_start + 0.02).min(1.0);
    }

    let to_px = |value: f32| ((value * width as f32).round() as u32).min(width);

    (
        to_px(left_trim),
        to_px(left_end),
        to_px(right_start),
        to_px(right_trim),
    )
}

fn crop_and_save(
    image: &DynamicImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    previews_dir: &Path,
    base_name: &str,
    digest: u64,
    suffix: &str,
    target_width: Option<u32>,
) -> Result<Option<PathBuf>, ManualSplitError> {
    if width == 0 || height == 0 {
        return Ok(None);
    }

    let view = image.crop_imm(x, y, width, height);
    let mut buffer = view.to_rgba8();

    if let Some(target) = target_width {
        if target > 0 && width > target {
            let new_height =
                std::cmp::max(1, (target as u64 * height as u64 / width as u64) as u32);
            buffer = resize(
                &buffer,
                target,
                new_height,
                image::imageops::FilterType::Triangle,
            );
        }
    }

    let path = previews_dir.join(format!("{}-{:x}-{}.png", base_name, digest, suffix));
    DynamicImage::ImageRgba8(buffer)
        .save_with_format(&path, ImageFormat::Png)
        .map_err(|err| ManualSplitError::PreviewWrite(err.to_string()))?;
    Ok(Some(path))
}

fn preview_digest(source: &Path, lines: &[f32; 4]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    for value in lines {
        hasher.write_u64(value.to_bits() as u64);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> MutexGuard<'static, ()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvVarGuard {
        key: &'static str,
    }

    impl EnvVarGuard {
        fn new(key: &'static str, value: &str) -> Self {
            std::env::set_var(key, value);
            Self { key }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.key);
        }
    }

    #[test]
    fn prepare_manual_split_workspace_initializes_context() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("images");
        fs::create_dir_all(&source_dir).unwrap();

        let first_image = source_dir.join("page_001.png");
        let second_image = source_dir.join("page_002.png");
        write_mock_image(&first_image, 2048, 1536);
        write_mock_image(&second_image, 1980, 1400);

        let response = prepare_manual_split_workspace(PrepareManualSplitWorkspaceRequest {
            source_directory: source_dir.clone(),
            workspace_root: None,
            overwrite: true,
        })
        .unwrap();

        let workspace = response.workspace.clone();
        assert!(workspace.as_path().exists());
        assert_eq!(response.entries.len(), 2);
        assert!(response.manual_split_report_path.is_none());
        assert!(response.manual_split_report_summary.is_none());
        assert!(!response.has_revert_history);

        let report_path = workspace.join("split-report.json");
        assert!(report_path.exists());
        let overrides_path = workspace
            .join("manual-overrides")
            .join("manual_overrides.json");
        assert!(overrides_path.exists());

        let context = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace.clone(),
        })
        .unwrap();
        assert_eq!(context.entries.len(), 2);
        assert!(context.manual_split_report_path.is_none());
        assert!(context.manual_split_report_summary.is_none());
        assert!(!context.has_revert_history);

        let reuse = prepare_manual_split_workspace(PrepareManualSplitWorkspaceRequest {
            source_directory: source_dir,
            workspace_root: None,
            overwrite: false,
        })
        .unwrap();
        assert_eq!(reuse.workspace, workspace);
        assert_eq!(reuse.entries.len(), 2);
        assert!(reuse.manual_split_report_path.is_none());
        assert!(reuse.manual_split_report_summary.is_none());
        assert!(!reuse.has_revert_history);
    }

    #[test]
    fn load_manual_split_context_reads_report() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_dir = temp.path().join("images");
        fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("page_001.png");
        write_mock_image(&source_path, 2000, 1400);

        let report = serde_json::json!({
            "generatedAt": "2025-10-06T10:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "split",
                    "split_x": 950,
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let overrides_dir = workspace.join("manual-overrides");
        fs::create_dir_all(&overrides_dir).unwrap();
        let overrides = serde_json::json!({
            "entries": [
                {
                    "source": source_path,
                    "width": 2000,
                    "height": 1400,
                    "lines": [0.05, 0.45, 0.55, 0.95],
                    "locked": true,
                    "lastAppliedAt": "2025-10-06T12:00:00Z"
                }
            ]
        });
        fs::write(
            overrides_dir.join("manual_overrides.json"),
            serde_json::to_string_pretty(&overrides).unwrap(),
        )
        .unwrap();

        let context = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace.clone(),
        })
        .unwrap();

        assert_eq!(context.entries.len(), 1);
        assert!(context.manual_split_report_path.is_none());
        assert!(context.manual_split_report_summary.is_none());
        assert!(!context.has_revert_history);
        let entry = &context.entries[0];
        assert_eq!(entry.source_path, source_path);
        assert_eq!(entry.width, 2000);
        assert_eq!(entry.height, 1400);
        assert!(entry.recommended_lines.is_some());
        assert!(entry.existing_lines.is_some());
        assert!(entry.locked);
        assert_eq!(
            entry.last_applied_at.as_deref(),
            Some("2025-10-06T12:00:00Z")
        );
        assert_eq!(entry.image_kind, Some(ManualImageKind::Content));
        assert_eq!(entry.rotate90, Some(false));
    }

    #[test]
    fn load_manual_split_context_includes_summary_when_report_exists() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_001.png");
        write_mock_image(&source_path, 1800, 1200);

        let split_report = serde_json::json!({
            "generatedAt": "2025-10-07T02:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "manual",
                    "splitX": 900
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&split_report).unwrap(),
        )
        .unwrap();

        let overrides_dir = workspace.join("manual-overrides");
        fs::create_dir_all(&overrides_dir).unwrap();
        let overrides = serde_json::json!({
            "entries": [
                {
                    "source": source_path,
                    "width": 1800,
                    "height": 1200,
                    "lines": [0.05, 0.45, 0.55, 0.95],
                    "locked": false,
                    "lastAppliedAt": "2025-10-07T03:00:00Z"
                }
            ]
        });
        fs::write(
            overrides_dir.join("manual_overrides.json"),
            serde_json::to_string_pretty(&overrides).unwrap(),
        )
        .unwrap();

        let manual_report = serde_json::json!({
            "version": 2,
            "generatedAt": "2025-10-07T03:05:00Z",
            "total": 1,
            "applied": 1,
            "skipped": 0,
            "entries": [
                {
                    "source": source_path,
                    "outputs": [workspace.join("page_001_R.png"), workspace.join("page_001_L.png")],
                    "lines": [0.05, 0.45, 0.55, 0.95],
                    "pixels": [90, 810, 990, 1710],
                    "gutterRatio": 0.1,
                    "imageKind": "content",
                    "rotate90": false,
                    "accelerator": "cpu",
                    "width": 1800,
                    "height": 1200,
                    "appliedAt": "2025-10-07T03:05:00Z"
                }
            ]
        });
        fs::write(
            workspace.join("manual_split_report.json"),
            serde_json::to_string_pretty(&manual_report).unwrap(),
        )
        .unwrap();

        let context = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace.clone(),
        })
        .unwrap();

        let summary = context
            .manual_split_report_summary
            .expect("expected manual split summary");
        assert_eq!(summary.total, 1);
        assert_eq!(summary.applied, 1);
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.generated_at, "2025-10-07T03:05:00Z");
        let report_path = context
            .manual_split_report_path
            .expect("expected manual report path");
        assert_eq!(
            report_path.file_name().and_then(|value| value.to_str()),
            Some("manual_split_report.json")
        );
        let entry = context
            .entries
            .first()
            .expect("expected manual split context entry");
        assert_eq!(entry.image_kind, Some(ManualImageKind::Content));
        assert_eq!(entry.rotate90, Some(false));
        assert!(!context.has_revert_history);
    }

    #[test]
    fn render_manual_split_preview_writes_outputs() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_010.png");
        write_mock_image(&source_path, 1200, 800);

        let response = render_manual_split_preview(ManualSplitPreviewRequest {
            workspace: workspace.clone(),
            source_path: source_path.clone(),
            lines: [0.05, 0.48, 0.52, 0.96],
            target_width: Some(600),
        })
        .unwrap();

        assert_eq!(response.source_path, source_path);
        assert!(response.left_preview_path.is_some());
        assert!(response.right_preview_path.is_some());
        assert!(response.gutter_preview_path.is_some());

        if let Some(path) = response.left_preview_path {
            assert!(path.exists());
        }
    }

    #[test]
    fn apply_manual_splits_creates_outputs_and_updates_report() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_001.png");
        write_mock_image(&source_path, 2000, 1400);

        let report = serde_json::json!({
            "generatedAt": "2025-10-06T10:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "split",
                    "splitX": 980,
                    "confidence": 0.92,
                    "contentWidthRatio": 0.8,
                    "outputs": [
                        workspace.join("page_001_R.png"),
                        workspace.join("page_001_L.png")
                    ],
                    "metadata": {}
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let request = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.05,
                left_page_end: 0.48,
                right_page_start: 0.52,
                right_trim: 0.95,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Content,
                rotate90: false,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let mut progress_events: Vec<ManualSplitProgress> = Vec::new();
        let mut progress_handler = |payload: ManualSplitProgress| {
            progress_events.push(payload);
        };

        let response = apply_manual_splits(request, Some(&mut progress_handler)).unwrap();

        assert!(
            !response.applied.is_empty(),
            "expected at least one applied entry"
        );
        assert!(response.can_revert);
        let manual_report_path = response
            .manual_split_report_path
            .expect("expected manual split report path");
        assert!(manual_report_path.exists());
        let report_summary = response
            .manual_split_report_summary
            .expect("expected manual report summary");
        assert_eq!(report_summary.total, 1);
        assert_eq!(report_summary.applied, 1);
        assert_eq!(report_summary.skipped, 0);
        let applied_entry = &response.applied[0];
        let canonical_source = fs::canonicalize(&source_path).unwrap();
        assert_eq!(applied_entry.source_path, canonical_source);
        assert_eq!(applied_entry.outputs.len(), 2);
        assert_eq!(applied_entry.image_kind, ManualImageKind::Content);
        assert!(!applied_entry.rotate90);
        assert_eq!(
            applied_entry.accelerator,
            EdgeTextureAccelerator::Cpu,
            "expected CPU accelerator fallback when GPU not requested"
        );

        let left_output = workspace.join("page_001_R.png");
        let right_output = workspace.join("page_001_L.png");
        assert!(left_output.exists());
        assert!(right_output.exists());

        let overrides_path = response.manual_overrides_path.unwrap();
        let overrides_data = fs::read_to_string(&overrides_path).unwrap();
        let overrides: ManualOverridesFile = serde_json::from_str(&overrides_data).unwrap();
        let override_entry = overrides
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected override entry for source");
        assert!(override_entry.lines[0] < override_entry.lines[3]);
        assert!(override_entry.pixels.is_some());
        assert_eq!(override_entry.image_kind, ManualImageKind::Content);
        assert!(!override_entry.rotate90);

        let report_path = response.split_report_path.unwrap();
        let report_data = fs::read_to_string(&report_path).unwrap();
        let parsed: SplitReportFull = serde_json::from_str(&report_data).unwrap();
        let updated_item = parsed
            .items
            .iter()
            .find(|item| item.source == canonical_source)
            .expect("expected split report entry for source");
        assert_eq!(updated_item.mode, SplitMode::Manual);
        assert_eq!(updated_item.metadata.manual_source.as_deref(), Some("user"));
        assert!(updated_item.metadata.manual_lines.is_some());
        assert_eq!(
            updated_item.metadata.manual_image_kind,
            Some(ManualImageKind::Content)
        );
        assert_eq!(updated_item.metadata.manual_rotate90, Some(false));

        let manual_report_data = fs::read_to_string(manual_report_path).unwrap();
        let parsed_manual: ManualSplitReportFile =
            serde_json::from_str(&manual_report_data).unwrap();
        assert_eq!(parsed_manual.version, 2);
        assert_eq!(parsed_manual.total, 1);
        let report_entry = parsed_manual
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected manual report entry");
        assert_eq!(report_entry.accelerator, "cpu");
        assert_eq!(report_entry.image_kind, ManualImageKind::Content);
        assert!(!report_entry.rotate90);

        assert!(!progress_events.is_empty());
        assert!(progress_events
            .iter()
            .any(|event| event.completed == response.applied.len()));
    }

    #[test]
    fn apply_manual_splits_records_gpu_accelerator_when_mock_gpu_enabled() {
        let _guard = env_lock();
        let _env = EnvVarGuard::new("EDGE_TEXTURE_ACCELERATOR", "mock-gpu");

        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_002.png");
        write_mock_image(&source_path, 2200, 1480);

        let report = serde_json::json!({
            "generatedAt": "2025-10-06T10:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "split",
                    "splitX": 1000,
                    "confidence": 0.92,
                    "contentWidthRatio": 0.8,
                    "outputs": [
                        workspace.join("page_002_R.png"),
                        workspace.join("page_002_L.png")
                    ],
                    "metadata": {}
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let request = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.05,
                left_page_end: 0.45,
                right_page_start: 0.55,
                right_trim: 0.95,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Content,
                rotate90: false,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let response = apply_manual_splits(request, None).unwrap();
        assert!(
            !response.applied.is_empty(),
            "expected at least one applied entry"
        );
        let applied_entry = &response.applied[0];
        assert_eq!(
            applied_entry.accelerator,
            EdgeTextureAccelerator::Gpu,
            "expected GPU accelerator to be recorded when mock GPU is requested"
        );
        assert_eq!(applied_entry.image_kind, ManualImageKind::Content);
        assert!(!applied_entry.rotate90);

        let manual_report_path = response
            .manual_split_report_path
            .expect("expected manual split report path");
        let manual_report_data = fs::read_to_string(manual_report_path).unwrap();
        let parsed_manual: ManualSplitReportFile =
            serde_json::from_str(&manual_report_data).unwrap();
        assert_eq!(parsed_manual.version, 2);
        let canonical_source = fs::canonicalize(&source_path).unwrap();
        let report_entry = parsed_manual
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected manual report entry");
        assert_eq!(report_entry.accelerator, "gpu");
        assert_eq!(report_entry.image_kind, ManualImageKind::Content);
        assert!(!report_entry.rotate90);
    }

    #[test]
    fn apply_manual_splits_emits_single_output_for_cover_kind() {
        let _guard = env_lock();

        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_003.png");
        write_mock_image(&source_path, 2000, 1400);

        let report = serde_json::json!({
            "generatedAt": "2025-10-07T04:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "manual",
                    "splitX": 900,
                    "confidence": 1.0,
                    "contentWidthRatio": 1.0,
                    "outputs": [],
                    "metadata": {}
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let request = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.1,
                left_page_end: 0.1,
                right_page_start: 0.9,
                right_trim: 0.9,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Cover,
                rotate90: false,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let response = apply_manual_splits(request, None).unwrap();
        assert_eq!(response.applied.len(), 1);
        let applied_entry = &response.applied[0];
        assert_eq!(applied_entry.outputs.len(), 1);
        assert_eq!(applied_entry.image_kind, ManualImageKind::Cover);
        assert!(!applied_entry.rotate90);

        let cover_output = workspace.join("page_003_cover.png");
        assert!(cover_output.exists(), "cover output should exist");
        let manual_cover = workspace.join("manual").join("page_003_manual_cover.png");
        assert!(
            manual_cover.exists(),
            "manual cover output should exist in manual directory"
        );

        let overrides_path = response.manual_overrides_path.expect("overrides path");
        let overrides_data = fs::read_to_string(&overrides_path).unwrap();
        let overrides: ManualOverridesFile = serde_json::from_str(&overrides_data).unwrap();
        let canonical_source = fs::canonicalize(&source_path).unwrap();
        let override_entry = overrides
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected override entry");
        let override_outputs = override_entry
            .outputs
            .as_ref()
            .map(|items| items.len())
            .unwrap_or(0);
        assert_eq!(override_outputs, 1);
        assert_eq!(override_entry.image_kind, ManualImageKind::Cover);
        assert!(!override_entry.rotate90);

        let split_report_path = response.split_report_path.expect("split report path");
        let split_report_data = fs::read_to_string(&split_report_path).unwrap();
        let parsed: SplitReportFull = serde_json::from_str(&split_report_data).unwrap();
        let item = parsed
            .items
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected split report item");
        assert!(item.split_x.is_none());
        assert_eq!(item.outputs.len(), 1);
        assert_eq!(
            item.metadata.manual_image_kind,
            Some(ManualImageKind::Cover)
        );
        assert_eq!(item.metadata.manual_rotate90, Some(false));

        let manual_report_path = response
            .manual_split_report_path
            .expect("expected manual report path");
        let manual_report_data = fs::read_to_string(&manual_report_path).unwrap();
        let manual_report: ManualSplitReportFile =
            serde_json::from_str(&manual_report_data).unwrap();
        assert_eq!(manual_report.version, 2);
        let report_entry = manual_report
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected manual report entry");
        assert_eq!(report_entry.outputs.len(), 1);
        assert_eq!(report_entry.image_kind, ManualImageKind::Cover);
        assert!(!report_entry.rotate90);
    }

    #[test]
    fn apply_manual_splits_emits_rotated_single_output_for_spread_kind() {
        let _guard = env_lock();

        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_004.png");
        write_mock_image(&source_path, 1600, 900);

        let report = serde_json::json!({
            "generatedAt": "2025-10-07T05:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "manual",
                    "splitX": 800,
                    "confidence": 1.0,
                    "contentWidthRatio": 1.0,
                    "outputs": [],
                    "metadata": {}
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let request = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.05,
                left_page_end: 0.05,
                right_page_start: 0.95,
                right_trim: 0.95,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Spread,
                rotate90: true,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let response = apply_manual_splits(request, None).unwrap();
        assert_eq!(response.applied.len(), 1);
        let applied_entry = &response.applied[0];
        assert_eq!(applied_entry.outputs.len(), 1);
        assert_eq!(applied_entry.image_kind, ManualImageKind::Spread);
        assert!(applied_entry.rotate90);

        let spread_output = workspace.join("page_004_spread.png");
        assert!(spread_output.exists(), "spread output should exist");
        let manual_spread = workspace.join("manual").join("page_004_manual_spread.png");
        assert!(
            manual_spread.exists(),
            "manual spread output should exist in manual directory"
        );

        let overrides_path = response.manual_overrides_path.expect("overrides path");
        let overrides_data = fs::read_to_string(&overrides_path).unwrap();
        let overrides: ManualOverridesFile = serde_json::from_str(&overrides_data).unwrap();
        let canonical_source = fs::canonicalize(&source_path).unwrap();
        let override_entry = overrides
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected override entry");
        assert_eq!(override_entry.image_kind, ManualImageKind::Spread);
        assert!(override_entry.rotate90);
        let override_outputs = override_entry
            .outputs
            .as_ref()
            .map(|items| items.len())
            .unwrap_or_default();
        assert_eq!(override_outputs, 1);
        let override_pixels = override_entry
            .pixels
            .expect("expected pixels recorded in override entry");

        let spread_image = image::open(&spread_output).expect("spread output should open");
        let (rotated_width, rotated_height) = spread_image.dimensions();
        assert_eq!(rotated_width, 900, "width should match original height");
        let expected_width = override_pixels[3].saturating_sub(override_pixels[0]);
        assert_eq!(
            rotated_height, expected_width,
            "height should match cropped width"
        );

        let split_report_path = response.split_report_path.expect("split report path");
        let split_report_data = fs::read_to_string(&split_report_path).unwrap();
        let parsed: SplitReportFull = serde_json::from_str(&split_report_data).unwrap();
        let item = parsed
            .items
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected split report entry");
        assert!(item.split_x.is_none());
        assert_eq!(item.outputs.len(), 1);
        assert_eq!(
            item.metadata.manual_image_kind,
            Some(ManualImageKind::Spread)
        );
        assert_eq!(item.metadata.manual_rotate90, Some(true));

        let manual_report_path = response
            .manual_split_report_path
            .expect("manual report path");
        let manual_report_data = fs::read_to_string(&manual_report_path).unwrap();
        let manual_report: ManualSplitReportFile =
            serde_json::from_str(&manual_report_data).unwrap();
        assert_eq!(manual_report.version, 2);
        let report_entry = manual_report
            .entries
            .iter()
            .find(|entry| entry.source == canonical_source)
            .expect("expected manual report entry");
        assert_eq!(report_entry.outputs.len(), 1);
        assert_eq!(report_entry.image_kind, ManualImageKind::Spread);
        assert!(report_entry.rotate90);
    }

    #[test]
    fn revert_manual_splits_restores_previous_outputs() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let source_path = workspace.join("page_050.png");
        write_mock_image(&source_path, 1600, 1200);

        let report = serde_json::json!({
            "generatedAt": "2025-10-07T06:00:00Z",
            "items": [
                {
                    "source": source_path,
                    "mode": "manual",
                    "splitX": 820,
                    "confidence": 0.9,
                    "contentWidthRatio": 0.8,
                    "outputs": [
                        workspace.join("page_050_R.png"),
                        workspace.join("page_050_L.png")
                    ],
                    "metadata": {}
                }
            ]
        });
        fs::write(
            workspace.join("split-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();

        let request_initial = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.05,
                left_page_end: 0.47,
                right_page_start: 0.53,
                right_trim: 0.96,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Content,
                rotate90: false,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let mut progress_initial = |_payload: ManualSplitProgress| {};
        let response_initial =
            apply_manual_splits(request_initial, Some(&mut progress_initial)).unwrap();
        assert!(response_initial.can_revert);
        let applied_initial = response_initial.applied[0].outputs.clone();
        let right_output = applied_initial[0].clone();
        let left_output = applied_initial[1].clone();
        let initial_left_bytes = fs::read(&left_output).unwrap();
        let initial_right_bytes = fs::read(&right_output).unwrap();

        let request_second = ManualSplitApplyRequest {
            workspace: workspace.clone(),
            overrides: vec![ManualSplitLine {
                source: source_path.clone(),
                left_trim: 0.08,
                left_page_end: 0.5,
                right_page_start: 0.58,
                right_trim: 0.98,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Content,
                rotate90: false,
            }],
            accelerator: EdgeTextureAcceleratorPreference::Auto,
            generate_preview: false,
        };

        let mut progress_second = |_payload: ManualSplitProgress| {};
        let response_second =
            apply_manual_splits(request_second, Some(&mut progress_second)).unwrap();
        assert!(response_second.can_revert);
        let second_left_bytes = fs::read(&left_output).unwrap();
        assert_ne!(initial_left_bytes, second_left_bytes);

        let context_after_second = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace.clone(),
        })
        .unwrap();
        assert!(context_after_second.has_revert_history);

        let revert_response = revert_manual_splits(ManualSplitRevertRequest {
            workspace: workspace.clone(),
        })
        .unwrap();
        assert!(revert_response.restored_outputs > 0);
        let reverted_left_bytes = fs::read(&left_output).unwrap();
        let reverted_right_bytes = fs::read(&right_output).unwrap();
        assert_eq!(reverted_left_bytes, initial_left_bytes);
        assert_eq!(reverted_right_bytes, initial_right_bytes);

        let manifest_path = workspace
            .join("manual-overrides")
            .join("backups")
            .join(MANUAL_REVERT_MANIFEST);
        assert!(!manifest_path.exists());
        assert!(revert_response.manual_split_report_summary.is_some());

        let context_after_revert = load_manual_split_context(ManualSplitContextRequest {
            workspace: workspace.clone(),
        })
        .unwrap();
        assert!(!context_after_revert.has_revert_history);
    }

    fn write_mock_image(path: &Path, width: u32, height: u32) {
        let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            let r = (x % 256) as u8;
            let g = (y % 256) as u8;
            let b = ((x + y) % 256) as u8;
            Rgba([r, g, b, 255])
        });
        DynamicImage::ImageRgba8(buffer)
            .save_with_format(path, ImageFormat::Png)
            .unwrap();
    }
}
