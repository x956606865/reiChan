use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::doublepage::{
    EdgeTextureAcceleratorPreference, ManualImageKind, ManualOverrideEntry, ManualOverridesFile,
    SplitDetectionSummary,
};
use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use hex;
use http::header::AUTHORIZATION;
use http::HeaderValue;
use natord::compare;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use sha2::{Digest, Sha256};
use tauri::{async_runtime, AppHandle, Emitter};
use tempfile::NamedTempFile;
use tokio::net::TcpStream;
use tokio::task::JoinError;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::CompressionMethod;
use zip::ZipArchive;
use zip::ZipWriter;

const SUPPORTED_IMAGE_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];
pub const JOB_EVENT_NAME: &str = "manga-job-event";
pub const UPLOAD_EVENT_NAME: &str = "manga-upload-progress";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenameEntry {
    pub original_name: String,
    pub renamed_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RenameOutcome {
    pub directory: PathBuf,
    pub manifest_path: Option<PathBuf>,
    pub entries: Vec<RenameEntry>,
    pub dry_run: bool,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub split_applied: bool,
    #[serde(default)]
    pub split_workspace: Option<PathBuf>,
    #[serde(default)]
    pub split_report_path: Option<PathBuf>,
    #[serde(default)]
    pub split_summary: Option<RenameSplitSummary>,
    #[serde(default)]
    pub source_directory: Option<PathBuf>,
    #[serde(default)]
    pub split_manual_overrides: bool,
    #[serde(default)]
    pub manual_entries: Option<Vec<ManifestManualEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RenameSplitSummary {
    pub analyzed_files: usize,
    pub emitted_files: usize,
    pub skipped_files: usize,
    pub split_pages: usize,
    pub cover_trims: usize,
    pub fallback_splits: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MangaSourceMode {
    SingleVolume,
    MultiVolume,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VolumeCandidate {
    pub directory: PathBuf,
    pub folder_name: String,
    pub image_count: usize,
    #[serde(default)]
    pub detected_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MangaSourceAnalysis {
    pub root: PathBuf,
    pub mode: MangaSourceMode,
    pub root_image_count: usize,
    pub total_images: usize,
    pub volume_candidates: Vec<VolumeCandidate>,
    pub skipped_entries: Vec<String>,
    #[serde(default)]
    pub split_detection: Option<SplitDetectionSummary>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOptions {
    pub directory: PathBuf,
    #[serde(default = "default_pad")]
    pub pad: usize,
    #[serde(default = "default_extension")]
    pub target_extension: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub split: RenameSplitOptions,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RenameSplitOptions {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub report_path: Option<PathBuf>,
    #[serde(default)]
    pub summary: Option<RenameSplitSummary>,
    #[serde(default)]
    pub warnings: Option<Vec<String>>,
}

fn default_pad() -> usize {
    4
}

fn default_extension() -> String {
    "jpg".to_string()
}

#[derive(Debug)]
pub enum RenameError {
    Io(std::io::Error),
    DirectoryNotFound(PathBuf),
    EmptyDirectory(PathBuf),
    NonUtf8Path(PathBuf),
    Serialization(serde_json::Error),
    SplitWorkspaceMissing(PathBuf),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenameError::Io(err) => write!(f, "I/O error: {}", err),
            RenameError::DirectoryNotFound(path) => {
                write!(f, "directory not found: {}", path.display())
            }
            RenameError::EmptyDirectory(path) => {
                write!(f, "no image files found in {}", path.display())
            }
            RenameError::NonUtf8Path(path) => {
                write!(f, "path is not valid UTF-8: {}", path.display())
            }
            RenameError::Serialization(err) => write!(f, "failed to write manifest: {}", err),
            RenameError::SplitWorkspaceMissing(path) => {
                write!(f, "split workspace not found: {}", path.display())
            }
        }
    }
}

impl std::error::Error for RenameError {}

impl From<std::io::Error> for RenameError {
    fn from(value: std::io::Error) -> Self {
        RenameError::Io(value)
    }
}

impl From<serde_json::Error> for RenameError {
    fn from(value: serde_json::Error) -> Self {
        RenameError::Serialization(value)
    }
}

pub fn analyze_manga_directory(directory: PathBuf) -> Result<MangaSourceAnalysis, RenameError> {
    if !directory.exists() || !directory.is_dir() {
        return Err(RenameError::DirectoryNotFound(directory));
    }

    let mut root_image_count = 0usize;
    let mut skipped_entries: Vec<String> = Vec::new();
    let mut volume_candidates: Vec<VolumeCandidate> = Vec::new();

    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        let file_name_os = entry.file_name();
        let file_name = file_name_os
            .to_str()
            .ok_or_else(|| RenameError::NonUtf8Path(path.clone()))?
            .to_string();

        if file_type.is_dir() {
            let (image_count, skipped) = scan_child_directory(&path)?;
            if image_count > 0 {
                let detected_number = detect_volume_number(&file_name);
                volume_candidates.push(VolumeCandidate {
                    directory: path,
                    folder_name: file_name.clone(),
                    image_count,
                    detected_number,
                });

                if !skipped.is_empty() {
                    for skipped_entry in skipped {
                        skipped_entries.push(format!("{}/{}", file_name, skipped_entry));
                    }
                }
            } else {
                skipped_entries.push(format!("{}/ (no supported images)", file_name));
            }

            continue;
        }

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|value| value.to_ascii_lowercase());

        match extension {
            Some(ref ext) if SUPPORTED_IMAGE_EXTENSIONS.contains(&ext.as_str()) => {
                root_image_count += 1;
            }
            _ => skipped_entries.push(file_name),
        }
    }

    volume_candidates.sort_by(|a, b| compare(&a.folder_name, &b.folder_name));

    let total_volume_images: usize = volume_candidates.iter().map(|item| item.image_count).sum();

    if root_image_count == 0 && total_volume_images == 0 {
        return Err(RenameError::EmptyDirectory(directory));
    }

    let mode = if !volume_candidates.is_empty() && root_image_count == 0 {
        MangaSourceMode::MultiVolume
    } else if volume_candidates.len() >= 2 {
        MangaSourceMode::MultiVolume
    } else {
        MangaSourceMode::SingleVolume
    };

    let total_images = if matches!(mode, MangaSourceMode::MultiVolume) {
        total_volume_images
    } else {
        root_image_count + total_volume_images
    };

    let split_detection = crate::doublepage::estimate_split_candidates(&directory).ok();

    Ok(MangaSourceAnalysis {
        root: directory,
        mode,
        root_image_count,
        total_images,
        volume_candidates,
        skipped_entries,
        split_detection,
    })
}

fn scan_child_directory(path: &Path) -> Result<(usize, Vec<String>), RenameError> {
    let mut image_count = 0usize;
    let mut skipped: Vec<String> = Vec::new();

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child_path = entry.path();
        let file_type = entry.file_type()?;
        let file_name_os = entry.file_name();
        let file_name = file_name_os
            .to_str()
            .ok_or_else(|| RenameError::NonUtf8Path(child_path.clone()))?
            .to_string();

        if file_type.is_dir() {
            skipped.push(format!("{}/", file_name));
            continue;
        }

        let extension = child_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|value| value.to_ascii_lowercase());

        match extension {
            Some(ref ext) if SUPPORTED_IMAGE_EXTENSIONS.contains(&ext.as_str()) => {
                image_count += 1;
            }
            _ => skipped.push(file_name),
        }
    }

    Ok((image_count, skipped))
}

fn detect_volume_number(name: &str) -> Option<u32> {
    let mut current = String::new();
    let mut detected: Option<u32> = None;

    for ch in name.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<u32>() {
                detected = Some(value);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        if let Ok(value) = current.parse::<u32>() {
            detected = Some(value);
        }
    }

    detected
}

#[derive(Serialize)]
struct ManifestFile {
    version: u32,
    created_at: String,
    pad: usize,
    target_extension: String,
    files: Vec<ManifestEntryData>,
    skipped: Vec<String>,
    split_applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    split: Option<ManifestSplitSection>,
    #[serde(default)]
    split_manual_overrides: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    manual_entries: Option<Vec<ManifestManualEntry>>,
}

#[derive(Serialize)]
struct ManifestEntryData {
    source: String,
    target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestManualEntry {
    pub source: String,
    pub outputs: Vec<String>,
    pub lines: [u32; 4],
    pub percentages: [f32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_kind: Option<String>,
    #[serde(default)]
    pub rotate90: bool,
}

struct FileCandidate {
    path: PathBuf,
    file_name: String,
    numeric_hint: Option<u64>,
}

#[derive(Serialize)]
struct ManifestSplitSection {
    workspace: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_path: Option<PathBuf>,
    summary: RenameSplitSummary,
}

fn load_manual_manifest_entries(
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> (bool, Option<Vec<ManifestManualEntry>>) {
    let overrides_path = workspace
        .join("manual-overrides")
        .join("manual_overrides.json");

    if !overrides_path.exists() {
        return (false, None);
    }

    let data = match fs::read_to_string(&overrides_path) {
        Ok(content) => content,
        Err(err) => {
            warnings.push(format!("failed to read manual overrides: {}", err));
            return (false, None);
        }
    };

    let overrides: ManualOverridesFile = match serde_json::from_str(&data) {
        Ok(parsed) => parsed,
        Err(err) => {
            warnings.push(format!("failed to parse manual overrides: {}", err));
            return (false, None);
        }
    };

    if overrides.entries.is_empty() {
        return (false, None);
    }

    let manual_report_path = workspace.join("manual_split_report.json");
    if !manual_report_path.exists() {
        warnings.push(
            "manual split report not found; resume/download may ignore manual outputs".to_string(),
        );
    }

    let entries = overrides
        .entries
        .iter()
        .map(build_manifest_manual_entry)
        .collect::<Vec<_>>();

    (true, Some(entries))
}

fn build_manifest_manual_entry(entry: &ManualOverrideEntry) -> ManifestManualEntry {
    let source_name = entry
        .source
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| entry.source.to_string_lossy().to_string());

    let width = entry.width.max(1) as f32;
    let pixels = entry.pixels.unwrap_or_else(|| {
        [
            (entry.lines[0].clamp(0.0, 1.0) * width).round() as u32,
            (entry.lines[1].clamp(0.0, 1.0) * width).round() as u32,
            (entry.lines[2].clamp(0.0, 1.0) * width).round() as u32,
            (entry.lines[3].clamp(0.0, 1.0) * width).round() as u32,
        ]
    });

    let outputs: Vec<String> = entry
        .outputs
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
                .map(|value| value.to_string())
                .collect()
        })
        .filter(|values: &Vec<String>| !values.is_empty())
        .unwrap_or_else(|| {
            let stem = entry
                .source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("page");
            let ext = entry
                .source
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!(".{}", value))
                .unwrap_or_else(String::new);
            vec![format!("{}_R{}", stem, ext), format!("{}_L{}", stem, ext)]
        });

    let accelerator = entry.accelerator.map(|pref| match pref {
        EdgeTextureAcceleratorPreference::Auto => "auto".to_string(),
        EdgeTextureAcceleratorPreference::Cpu => "cpu".to_string(),
        EdgeTextureAcceleratorPreference::Gpu => "gpu".to_string(),
    });

    let image_kind = Some(match entry.image_kind {
        ManualImageKind::Content => "content",
        ManualImageKind::Cover => "cover",
        ManualImageKind::Spread => "spread",
    }
    .to_string());

    ManifestManualEntry {
        source: source_name,
        outputs,
        lines: pixels,
        percentages: entry.lines,
        accelerator,
        applied_at: entry.last_applied_at.clone(),
        image_kind,
        rotate90: entry.rotate90,
    }
}

pub fn perform_rename(options: RenameOptions) -> Result<RenameOutcome, RenameError> {
    let RenameOptions {
        directory,
        pad,
        target_extension,
        dry_run,
        split,
    } = options;

    if !directory.exists() || !directory.is_dir() {
        return Err(RenameError::DirectoryNotFound(directory.clone()));
    }

    let mut working_directory = directory.clone();
    let mut split_applied = false;
    let mut split_workspace = None;
    let split_report_path = split.report_path.clone();
    let split_summary = split.summary.clone();
    let split_warnings = split.warnings.clone().unwrap_or_default();
    let mut source_directory = None;

    if split.enabled {
        let workspace_path = split
            .workspace
            .clone()
            .ok_or_else(|| RenameError::SplitWorkspaceMissing(directory.clone()))?;

        if !workspace_path.exists() || !workspace_path.is_dir() {
            return Err(RenameError::SplitWorkspaceMissing(workspace_path));
        }

        split_applied = true;
        working_directory = workspace_path.clone();
        split_workspace = Some(workspace_path);
        source_directory = Some(directory.clone());
    }

    let normalized_pad = pad.max(1);
    let normalized_extension = target_extension.to_ascii_lowercase();

    let mut candidates: Vec<FileCandidate> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for entry in fs::read_dir(&working_directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            skipped.push(format!("{}/", entry.file_name().to_string_lossy()));
            continue;
        }

        let file_name_os = entry.file_name();
        let file_name = file_name_os
            .to_str()
            .ok_or_else(|| RenameError::NonUtf8Path(path.clone()))?
            .to_string();

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|value| value.to_ascii_lowercase());

        match extension {
            Some(ref ext) if SUPPORTED_IMAGE_EXTENSIONS.contains(&ext.as_str()) => {
                let numeric_hint = extract_numeric_suffix(&file_name);
                candidates.push(FileCandidate {
                    path: path.clone(),
                    file_name,
                    numeric_hint,
                });
            }
            _ => skipped.push(file_name),
        }
    }

    if candidates.is_empty() {
        return Err(RenameError::EmptyDirectory(working_directory.clone()));
    }

    candidates.sort_by(|a, b| compare(&a.file_name, &b.file_name));

    let mut entries = Vec::with_capacity(candidates.len());
    let mut used_numbers: HashSet<u64> = HashSet::new();
    let mut next_sequence = 1u64;

    for candidate in candidates.iter() {
        let mut assigned_number = candidate.numeric_hint.and_then(|value| {
            if value == 0 || used_numbers.contains(&value) {
                None
            } else {
                Some(value)
            }
        });

        if assigned_number.is_none() {
            while used_numbers.contains(&next_sequence) {
                next_sequence += 1;
            }
            assigned_number = Some(next_sequence);
            next_sequence += 1;
        }

        let number = assigned_number.expect("assigned numbering");
        used_numbers.insert(number);

        let renamed = format!(
            "{:0width$}.{}",
            number,
            normalized_extension,
            width = normalized_pad
        );

        entries.push(RenameEntry {
            original_name: candidate.file_name.clone(),
            renamed_name: renamed,
        });
    }

    let mut warnings = build_warnings(&skipped);
    if !split_warnings.is_empty() {
        warnings.extend(split_warnings);
    }

    if dry_run {
        return Ok(RenameOutcome {
            directory: working_directory,
            manifest_path: None,
            entries,
            dry_run: true,
            warnings,
            split_applied,
            split_workspace,
            split_report_path,
            split_summary,
            source_directory,
            split_manual_overrides: false,
            manual_entries: None,
        });
    }

    let temp_prefix = format!(".rei_tmp_{}", std::process::id());
    let mut temp_paths = Vec::with_capacity(candidates.len());

    for (index, candidate) in candidates.iter().enumerate() {
        let temp_name = format!("{}_{:04}", temp_prefix, index);
        let temp_path = working_directory.join(&temp_name);
        fs::rename(&candidate.path, &temp_path)?;
        temp_paths.push(temp_path);
    }

    for (index, temp_path) in temp_paths.iter().enumerate() {
        let final_path = working_directory.join(&entries[index].renamed_name);
        fs::rename(temp_path, &final_path)?;
    }

    let (split_manual_overrides_flag, manual_manifest_entries) =
        load_manual_manifest_entries(&working_directory, &mut warnings);

    let manifest = ManifestFile {
        version: 1,
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        pad: normalized_pad,
        target_extension: normalized_extension.clone(),
        files: entries
            .iter()
            .map(|entry| ManifestEntryData {
                source: entry.original_name.clone(),
                target: entry.renamed_name.clone(),
            })
            .collect(),
        skipped: skipped.clone(),
        split_applied,
        split: if split_applied {
            split_summary.as_ref().map(|summary| ManifestSplitSection {
                workspace: working_directory.clone(),
                report_path: split_report_path.clone(),
                summary: summary.clone(),
            })
        } else {
            None
        },
        split_manual_overrides: split_manual_overrides_flag,
        manual_entries: manual_manifest_entries.clone(),
    };

    let manifest_path = working_directory.join("manifest.json");
    let mut manifest_file = File::create(&manifest_path)?;
    serde_json::to_writer_pretty(&mut manifest_file, &manifest)?;
    manifest_file.write_all(b"\n")?;

    Ok(RenameOutcome {
        directory: working_directory,
        manifest_path: Some(manifest_path),
        entries,
        dry_run: false,
        warnings,
        split_applied,
        split_workspace,
        split_report_path,
        split_summary,
        source_directory,
        split_manual_overrides: split_manual_overrides_flag,
        manual_entries: manual_manifest_entries,
    })
}

fn build_warnings(skipped: &[String]) -> Vec<String> {
    if skipped.is_empty() {
        Vec::new()
    } else {
        let preview: Vec<String> = skipped.iter().take(5).cloned().collect();
        let suffix = if skipped.len() > preview.len() {
            format!(" (+{} more)", skipped.len() - preview.len())
        } else {
            String::new()
        };
        vec![format!(
            "Skipped {} non-image entries: {}{}",
            skipped.len(),
            preview.join(", "),
            suffix
        )]
    }
}

fn extract_numeric_suffix(name: &str) -> Option<u64> {
    let mut digits = String::new();
    for ch in name.chars().rev() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if digits.is_empty() {
            continue;
        } else {
            break;
        }
    }

    if digits.is_empty() {
        return None;
    }

    digits.chars().rev().collect::<String>().parse::<u64>().ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UploadMode {
    Folder,
    Zip,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    pub service_url: String,
    pub remote_path: String,
    pub local_path: PathBuf,
    pub mode: UploadMode,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub metadata: Option<UploadMetadata>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadMetadata {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadOutcome {
    pub remote_url: String,
    pub uploaded_bytes: u64,
    pub file_count: usize,
    pub mode: UploadMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgress {
    pub stage: UploadProgressStage,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub processed_files: usize,
    pub total_files: usize,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UploadProgressStage {
    Preparing,
    Uploading,
    Finalizing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobOptions {
    pub service_url: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
    pub payload: JobPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPayload {
    pub title: String,
    pub volume: String,
    pub input: JobInputPayload,
    #[serde(default)]
    pub params: JobParamsPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInputPayload {
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobParamsPayload {
    #[serde(default = "default_scale")]
    pub scale: u32,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_denoise")]
    pub denoise: String,
    #[serde(
        default = "default_output_format",
        skip_serializing_if = "is_default_output_format"
    )]
    pub output_format: String,
    #[serde(
        default = "default_jpeg_quality",
        skip_serializing_if = "is_default_jpeg_quality"
    )]
    pub jpeg_quality: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_pad: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    #[serde(default = "default_device", skip_serializing_if = "is_default_device")]
    pub device: String,
}

impl Default for JobParamsPayload {
    fn default() -> Self {
        Self {
            scale: default_scale(),
            model: default_model(),
            denoise: default_denoise(),
            output_format: default_output_format(),
            jpeg_quality: default_jpeg_quality(),
            tile_size: None,
            tile_pad: None,
            batch_size: None,
            device: default_device(),
        }
    }
}

fn default_scale() -> u32 {
    2
}

fn default_model() -> String {
    "RealESRGAN_x4plus_anime_6B".to_string()
}

fn default_denoise() -> String {
    "medium".to_string()
}

fn is_default_output_format(value: &String) -> bool {
    value == &default_output_format()
}

fn is_default_jpeg_quality(value: &u8) -> bool {
    *value == default_jpeg_quality()
}

fn is_default_device(value: &String) -> bool {
    value == &default_device()
}

fn default_output_format() -> String {
    "jpg".to_string()
}

fn default_jpeg_quality() -> u8 {
    95
}

fn default_device() -> String {
    "auto".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobSubmission {
    #[serde(alias = "job_id")]
    pub job_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatusRequest {
    pub service_url: String,
    pub job_id: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobStatusSnapshot {
    #[serde(alias = "job_id")]
    pub job_id: String,
    pub status: String,
    pub processed: u32,
    pub total: u32,
    #[serde(default)]
    #[serde(alias = "artifact_path")]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    #[serde(alias = "artifact_hash")]
    pub artifact_hash: Option<String>,
    #[serde(default)]
    pub params: Option<JobParamsPayload>,
    #[serde(default)]
    pub metadata: Option<JobMetadataSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobMetadataSnapshot {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobWatchRequest {
    pub service_url: String,
    pub job_id: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobControlRequest {
    pub service_url: String,
    pub job_id: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub input_path: Option<String>,
    #[serde(default)]
    pub input_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDownloadRequest {
    pub service_url: String,
    pub job_id: String,
    pub artifact_path: String,
    pub target_dir: PathBuf,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub metadata: Option<JobMetadataSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobEventTransport {
    Websocket,
    Polling,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobEventEnvelope {
    pub job_id: String,
    pub status: String,
    pub processed: u32,
    pub total: u32,
    pub artifact_path: Option<String>,
    pub message: Option<String>,
    pub transport: JobEventTransport,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub artifact_hash: Option<String>,
    #[serde(default)]
    pub params: Option<JobParamsPayload>,
    #[serde(default)]
    pub metadata: Option<JobMetadataSnapshot>,
}

impl JobEventEnvelope {
    fn from_snapshot(snapshot: JobStatusSnapshot, transport: JobEventTransport) -> Self {
        Self {
            job_id: snapshot.job_id,
            status: snapshot.status,
            processed: snapshot.processed,
            total: snapshot.total,
            artifact_path: snapshot.artifact_path,
            message: snapshot.message,
            transport,
            error: None,
            retries: snapshot.retries,
            last_error: snapshot.last_error,
            artifact_hash: snapshot.artifact_hash,
            params: snapshot.params,
            metadata: snapshot.metadata,
        }
    }

    pub(crate) fn system_error(job_id: String, message: String) -> Self {
        Self {
            job_id,
            status: "ERROR".to_string(),
            processed: 0,
            total: 0,
            artifact_path: None,
            message: Some(message.clone()),
            transport: JobEventTransport::System,
            error: Some(message),
            retries: 0,
            last_error: None,
            artifact_hash: None,
            params: None,
            metadata: None,
        }
    }
}

#[derive(Debug)]
pub enum JobError {
    Request(reqwest::Error),
    Url(url::ParseError),
    UnexpectedStatus(StatusCode),
    WebSocket(WsError),
    Emit(tauri::Error),
    Join(JoinError),
    InvalidResponse(String),
    InvalidServiceUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactValidationStatus {
    Matched,
    Missing,
    Extra,
    Mismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactValidationItem {
    pub filename: String,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub actual_hash: Option<String>,
    #[serde(default)]
    pub expected_bytes: Option<u64>,
    #[serde(default)]
    pub actual_bytes: Option<u64>,
    pub status: ArtifactValidationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReportSummary {
    pub matched: u32,
    pub missing: u32,
    pub extra: u32,
    pub mismatched: u32,
    pub total_manifest: u32,
    pub total_extracted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReport {
    pub job_id: String,
    pub artifact_path: PathBuf,
    pub extract_path: PathBuf,
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    pub hash: String,
    pub created_at: String,
    pub summary: ArtifactReportSummary,
    pub items: Vec<ArtifactValidationItem>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub report_path: Option<PathBuf>,
    #[serde(default)]
    pub archive_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDownloadSummary {
    pub job_id: String,
    pub archive_path: PathBuf,
    pub extract_path: PathBuf,
    pub hash: String,
    pub file_count: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ArtifactError {
    Request(reqwest::Error),
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    UnexpectedStatus(StatusCode),
    InvalidServiceUrl,
    ManifestMissing,
    ManifestRead(PathBuf, std::io::Error),
    ManifestParse(PathBuf, serde_json::Error),
    TargetDir(PathBuf, std::io::Error),
    ReportWrite(PathBuf, serde_json::Error),
    CachedReportRead(PathBuf, serde_json::Error),
    NotModifiedWithoutCache(PathBuf),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactError::Request(err) => write!(f, "request error: {}", err),
            ArtifactError::Io(err) => write!(f, "io error: {}", err),
            ArtifactError::Zip(err) => write!(f, "zip error: {}", err),
            ArtifactError::UnexpectedStatus(status) => write!(f, "unexpected status: {}", status),
            ArtifactError::InvalidServiceUrl => write!(f, "service url is empty"),
            ArtifactError::ManifestMissing => write!(f, "manifest path not provided"),
            ArtifactError::ManifestRead(path, err) => {
                write!(f, "failed to read manifest {}: {}", path.display(), err)
            }
            ArtifactError::ManifestParse(path, err) => {
                write!(f, "failed to parse manifest {}: {}", path.display(), err)
            }
            ArtifactError::TargetDir(path, err) => {
                write!(
                    f,
                    "unable to prepare target dir {}: {}",
                    path.display(),
                    err
                )
            }
            ArtifactError::ReportWrite(path, err) => {
                write!(f, "failed to write report {}: {}", path.display(), err)
            }
            ArtifactError::CachedReportRead(path, err) => {
                write!(
                    f,
                    "failed to read cached report {}: {}",
                    path.display(),
                    err
                )
            }
            ArtifactError::NotModifiedWithoutCache(path) => {
                write!(
                    f,
                    "remote returned 304 but no cached artifact at {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<reqwest::Error> for ArtifactError {
    fn from(value: reqwest::Error) -> Self {
        ArtifactError::Request(value)
    }
}

impl From<std::io::Error> for ArtifactError {
    fn from(value: std::io::Error) -> Self {
        ArtifactError::Io(value)
    }
}

impl From<zip::result::ZipError> for ArtifactError {
    fn from(value: zip::result::ZipError) -> Self {
        ArtifactError::Zip(value)
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Request(err) => write!(f, "request error: {}", err),
            JobError::Url(err) => write!(f, "invalid service url: {}", err),
            JobError::UnexpectedStatus(status) => {
                write!(f, "unexpected response status: {}", status)
            }
            JobError::WebSocket(err) => write!(f, "websocket error: {}", err),
            JobError::Emit(err) => write!(f, "event emit error: {}", err),
            JobError::Join(err) => write!(f, "task join error: {}", err),
            JobError::InvalidResponse(message) => write!(f, "invalid response: {}", message),
            JobError::InvalidServiceUrl => write!(f, "service url is empty"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<reqwest::Error> for JobError {
    fn from(value: reqwest::Error) -> Self {
        JobError::Request(value)
    }
}

impl From<url::ParseError> for JobError {
    fn from(value: url::ParseError) -> Self {
        JobError::Url(value)
    }
}

impl From<WsError> for JobError {
    fn from(value: WsError) -> Self {
        JobError::WebSocket(value)
    }
}

impl From<tauri::Error> for JobError {
    fn from(value: tauri::Error) -> Self {
        JobError::Emit(value)
    }
}

impl From<JoinError> for JobError {
    fn from(value: JoinError) -> Self {
        JobError::Join(value)
    }
}

#[derive(Debug)]
pub enum UploadError {
    Io(std::io::Error),
    Archive(zip::result::ZipError),
    Request(reqwest::Error),
    DirectoryNotFound(PathBuf),
    EmptyDirectory(PathBuf),
    NonUtf8Path(PathBuf),
    UnexpectedStatus(StatusCode),
    UnsupportedMode,
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadError::Io(err) => write!(f, "I/O error: {}", err),
            UploadError::Archive(err) => write!(f, "zip error: {}", err),
            UploadError::Request(err) => write!(f, "request error: {}", err),
            UploadError::DirectoryNotFound(path) => {
                write!(f, "directory not found: {}", path.display())
            }
            UploadError::EmptyDirectory(path) => {
                write!(f, "no files found to upload in {}", path.display())
            }
            UploadError::NonUtf8Path(path) => {
                write!(f, "path is not valid UTF-8: {}", path.display())
            }
            UploadError::UnexpectedStatus(status) => {
                write!(f, "unexpected response status: {}", status)
            }
            UploadError::UnsupportedMode => write!(f, "unsupported upload mode"),
        }
    }
}

impl std::error::Error for UploadError {}

impl From<std::io::Error> for UploadError {
    fn from(value: std::io::Error) -> Self {
        UploadError::Io(value)
    }
}

impl From<zip::result::ZipError> for UploadError {
    fn from(value: zip::result::ZipError) -> Self {
        UploadError::Archive(value)
    }
}

impl From<reqwest::Error> for UploadError {
    fn from(value: reqwest::Error) -> Self {
        UploadError::Request(value)
    }
}

pub fn perform_upload(
    app: Option<AppHandle>,
    request: UploadRequest,
) -> Result<UploadOutcome, UploadError> {
    let UploadRequest {
        service_url,
        remote_path,
        local_path,
        mode,
        bearer_token,
        metadata,
    } = request;

    if !local_path.exists() || !local_path.is_dir() {
        return Err(UploadError::DirectoryNotFound(local_path));
    }

    let files = collect_sorted_files(&local_path)?;
    if files.is_empty() {
        return Err(UploadError::EmptyDirectory(local_path));
    }

    let remote_url = build_remote_url(&service_url, &remote_path);

    match mode {
        UploadMode::Zip => upload_as_zip(
            app,
            &remote_url,
            &files,
            bearer_token.as_deref(),
            metadata.as_ref(),
        ),
        UploadMode::Folder => Err(UploadError::UnsupportedMode),
    }
}

pub fn create_remote_job(options: CreateJobOptions) -> Result<JobSubmission, JobError> {
    let CreateJobOptions {
        service_url,
        bearer_token,
        payload,
    } = options;

    let url = build_service_endpoint(&service_url, "jobs")?;
    let client = Client::new();
    let mut request = client.post(url).json(&payload);

    if let Some(token) = bearer_token.as_deref() {
        request = request.bearer_auth(token);
    }

    let response = request.send()?;
    if !response.status().is_success() {
        return Err(JobError::UnexpectedStatus(response.status()));
    }

    let submission = response.json::<JobSubmission>()?;
    Ok(submission)
}

pub fn resume_remote_job(request: JobControlRequest) -> Result<JobStatusSnapshot, JobError> {
    let url = build_service_endpoint(
        &request.service_url,
        &format!("jobs/{}/resume", request.job_id),
    )?;
    let client = Client::new();
    let mut http_request = client.post(url);

    if let Some(token) = request.bearer_token.as_deref() {
        http_request = http_request.bearer_auth(token);
    }

    let mut payload = serde_json::Map::new();
    if let Some(path) = request.input_path.as_ref() {
        payload.insert("inputPath".to_string(), Value::String(path.clone()));
    }
    if let Some(kind) = request.input_type.as_ref() {
        payload.insert("inputType".to_string(), Value::String(kind.clone()));
    }

    let response = if payload.is_empty() {
        http_request.send()?
    } else {
        http_request.json(&payload).send()?
    };

    if !response.status().is_success() {
        return Err(JobError::UnexpectedStatus(response.status()));
    }

    let snapshot = response.json::<JobStatusSnapshot>()?;
    Ok(snapshot)
}

pub fn cancel_remote_job(request: JobControlRequest) -> Result<JobStatusSnapshot, JobError> {
    let url = build_service_endpoint(
        &request.service_url,
        &format!("jobs/{}/cancel", request.job_id),
    )?;
    let client = Client::new();
    let mut http_request = client.post(url);

    if let Some(token) = request.bearer_token.as_deref() {
        http_request = http_request.bearer_auth(token);
    }

    let response = http_request.send()?;

    if !response.status().is_success() {
        return Err(JobError::UnexpectedStatus(response.status()));
    }

    let snapshot = response.json::<JobStatusSnapshot>()?;
    Ok(snapshot)
}

fn sanitize_filename_component(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    trimmed
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect()
}

fn parse_volume_number(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }

    digits
        .parse::<u32>()
        .ok()
        .map(|number| format!("{number:04}"))
}

fn build_archive_filename(
    metadata: Option<&JobMetadataSnapshot>,
    job_id: &str,
) -> (String, Vec<String>) {
    let mut warnings = Vec::new();

    if let Some(meta) = metadata {
        if let (Some(title), Some(volume)) = (meta.title.as_deref(), meta.volume.as_deref()) {
            let normalized_title = sanitize_filename_component(title);
            if normalized_title.is_empty() {
                warnings.push("作品名为空，使用 jobId 命名 zip。".to_string());
            } else if let Some(normalized_volume) = parse_volume_number(volume) {
                let filename = format!("{}_{}.zip", normalized_volume, normalized_title);
                return (filename, warnings);
            } else {
                warnings.push("卷号非数字，使用 jobId 命名 zip。".to_string());
            }
        } else {
            warnings.push("缺少作品名或卷号，使用 jobId 命名 zip。".to_string());
        }
    } else {
        warnings.push("缺少作业元数据，使用 jobId 命名 zip。".to_string());
    }

    let fallback = sanitize_filename_component(job_id);
    let final_name = if fallback.is_empty() {
        "artifact.zip".to_string()
    } else {
        format!("{}.zip", fallback)
    };
    (final_name, warnings)
}

struct PreparedArtifact {
    extract_root: PathBuf,
    archive_path: PathBuf,
    artifact_hash: String,
    warnings: Vec<String>,
    image_count: usize,
}

fn is_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            SUPPORTED_IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

fn normalize_zip_path(path: &Path) -> Option<String> {
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }

    Some(path.to_string_lossy().replace('\\', "/"))
}

fn finalize_artifact(
    temp_file: &NamedTempFile,
    request: &ArtifactDownloadRequest,
    archive_path: PathBuf,
    artifact_hash: String,
    mut warnings: Vec<String>,
    cleanup_extract: bool,
) -> Result<PreparedArtifact, ArtifactError> {
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| ArtifactError::TargetDir(parent.to_path_buf(), err))?;
    }

    if archive_path.exists() {
        fs::remove_file(&archive_path)
            .map_err(|err| ArtifactError::TargetDir(archive_path.clone(), err))?;
    }

    fs::create_dir_all(&request.target_dir)
        .map_err(|err| ArtifactError::TargetDir(request.target_dir.clone(), err))?;

    let extract_root = request.target_dir.join(&request.job_id);
    if extract_root.exists() {
        fs::remove_dir_all(&extract_root)
            .map_err(|err| ArtifactError::TargetDir(extract_root.clone(), err))?;
    }
    fs::create_dir_all(&extract_root)
        .map_err(|err| ArtifactError::TargetDir(extract_root.clone(), err))?;

    let mut archive_reader = File::open(temp_file.path())?;
    let mut archive = ZipArchive::new(&mut archive_reader)?;

    let mut image_entries: Vec<(PathBuf, String)> = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry.mangled_name();
        let out_path = extract_root.join(&relative);

        if entry.name().ends_with('/') {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut outfile = File::create(&out_path)?;
        io::copy(&mut entry, &mut outfile)?;

        if is_image_extension(&out_path) {
            if let Some(zip_name) = normalize_zip_path(&relative) {
                image_entries.push((out_path.clone(), zip_name));
            }
        }
    }

    let file = File::create(&archive_path)
        .map_err(|err| ArtifactError::TargetDir(archive_path.clone(), err))?;
    let mut writer = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Stored);

    for (path, zip_name) in &image_entries {
        writer.start_file(zip_name, options)?;
        let mut source = File::open(path)?;
        io::copy(&mut source, &mut writer)?;
    }
    writer.finish()?;

    if image_entries.is_empty() {
        warnings.push("压缩包中未找到图片文件。".to_string());
    }

    if cleanup_extract {
        match fs::remove_dir_all(&extract_root) {
            Ok(_) => warnings.push(format!("已清理临时目录 {}", extract_root.display())),
            Err(err) => warnings.push(format!("清理临时目录失败: {}", err)),
        }
    }

    Ok(PreparedArtifact {
        extract_root,
        archive_path,
        artifact_hash,
        warnings,
        image_count: image_entries.len(),
    })
}

pub fn download_artifact(
    request: ArtifactDownloadRequest,
) -> Result<ArtifactDownloadSummary, ArtifactError> {
    if request.service_url.trim().is_empty() {
        return Err(ArtifactError::InvalidServiceUrl);
    }

    let (archive_filename, warnings) =
        build_archive_filename(request.metadata.as_ref(), &request.job_id);
    let archive_path = request.target_dir.join(&archive_filename);
    let artifact_url = build_service_endpoint(
        &request.service_url,
        &format!("jobs/{}/artifact", request.job_id),
    )
    .map_err(|_| ArtifactError::InvalidServiceUrl)?;

    let client = Client::new();
    let mut http_request = client.get(artifact_url);
    if let Some(token) = request.bearer_token.as_deref() {
        http_request = http_request.bearer_auth(token);
    }

    let mut response = http_request.send()?;

    if !response.status().is_success() {
        return Err(ArtifactError::UnexpectedStatus(response.status()));
    }

    let mut temp_file = NamedTempFile::new()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        temp_file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }

    temp_file.flush()?;
    let artifact_hash = hex::encode(hasher.finalize());

    let prepared = finalize_artifact(
        &temp_file,
        &request,
        archive_path,
        artifact_hash,
        warnings,
        true,
    )?;

    Ok(ArtifactDownloadSummary {
        job_id: request.job_id,
        archive_path: prepared.archive_path,
        extract_path: prepared.extract_root,
        hash: prepared.artifact_hash,
        file_count: prepared.image_count,
        warnings: prepared.warnings,
    })
}

pub fn validate_artifact(
    request: ArtifactDownloadRequest,
) -> Result<ArtifactReport, ArtifactError> {
    if request.service_url.trim().is_empty() {
        return Err(ArtifactError::InvalidServiceUrl);
    }

    let extract_root = request.target_dir.join(&request.job_id);
    let (archive_filename, warnings) =
        build_archive_filename(request.metadata.as_ref(), &request.job_id);
    let archive_path = request.target_dir.join(&archive_filename);
    let cache_report_path = extract_root.join("artifact-report.json");
    let artifact_url = build_service_endpoint(
        &request.service_url,
        &format!("jobs/{}/artifact", request.job_id),
    )
    .map_err(|_| ArtifactError::InvalidServiceUrl)?;

    let client = Client::new();
    let mut http_request = client.get(artifact_url);
    if let Some(token) = request.bearer_token.as_deref() {
        http_request = http_request.bearer_auth(token);
    }
    let has_cached_report = cache_report_path.exists();
    if let Some(hash) = request.expected_hash.as_deref() {
        if has_cached_report {
            http_request = http_request.header("If-None-Match", hash);
        }
    }

    let mut response = http_request.send()?;

    if response.status() == StatusCode::NOT_MODIFIED {
        if has_cached_report {
            let file = File::open(&cache_report_path)?;
            let mut report: ArtifactReport = serde_json::from_reader(BufReader::new(file))
                .map_err(|err| ArtifactError::CachedReportRead(cache_report_path.clone(), err))?;
            report.report_path = Some(cache_report_path.clone());
            if !report
                .warnings
                .iter()
                .any(|warning| warning.contains("未变更"))
            {
                report
                    .warnings
                    .insert(0, "远端产物未变更，返回本地缓存结果。".to_string());
            }
            return Ok(report);
        }
        return Err(ArtifactError::NotModifiedWithoutCache(extract_root.clone()));
    }

    if !response.status().is_success() {
        return Err(ArtifactError::UnexpectedStatus(response.status()));
    }

    let mut temp_file = NamedTempFile::new()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        temp_file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }

    temp_file.flush()?;
    let artifact_hash = hex::encode(hasher.finalize());

    let prepared = finalize_artifact(
        &temp_file,
        &request,
        archive_path,
        artifact_hash,
        warnings,
        false,
    )?;

    let expected_map = if let Some(path) = request.manifest_path.as_ref() {
        Some(read_manifest_expectations(path)?)
    } else {
        None
    };

    let mut actual_map = collect_directory_digests(&prepared.extract_root)?;

    let mut warnings = prepared.warnings;
    if request.manifest_path.is_none() {
        warnings.push("未提供 manifest，按实际文件生成报告。".to_string());
    }

    let mut items: Vec<ArtifactValidationItem> = Vec::new();
    let mut matched = 0u32;
    let mut missing = 0u32;
    let mut extra = 0u32;
    let mut mismatched = 0u32;

    if let Some(mut expected) = expected_map {
        for (name, expected_digest) in expected.drain() {
            match actual_map.remove(&name) {
                Some(actual_digest) => {
                    if expected_digest.hash == actual_digest.hash {
                        matched += 1;
                        items.push(ArtifactValidationItem {
                            filename: name,
                            expected_hash: Some(expected_digest.hash),
                            actual_hash: Some(actual_digest.hash),
                            expected_bytes: Some(expected_digest.bytes),
                            actual_bytes: Some(actual_digest.bytes),
                            status: ArtifactValidationStatus::Matched,
                        });
                    } else {
                        mismatched += 1;
                        items.push(ArtifactValidationItem {
                            filename: name,
                            expected_hash: Some(expected_digest.hash),
                            actual_hash: Some(actual_digest.hash),
                            expected_bytes: Some(expected_digest.bytes),
                            actual_bytes: Some(actual_digest.bytes),
                            status: ArtifactValidationStatus::Mismatch,
                        });
                    }
                }
                None => {
                    missing += 1;
                    items.push(ArtifactValidationItem {
                        filename: name,
                        expected_hash: Some(expected_digest.hash),
                        actual_hash: None,
                        expected_bytes: Some(expected_digest.bytes),
                        actual_bytes: None,
                        status: ArtifactValidationStatus::Missing,
                    });
                }
            }
        }

        for (name, actual_digest) in actual_map.drain() {
            extra += 1;
            items.push(ArtifactValidationItem {
                filename: name,
                expected_hash: None,
                actual_hash: Some(actual_digest.hash),
                expected_bytes: None,
                actual_bytes: Some(actual_digest.bytes),
                status: ArtifactValidationStatus::Extra,
            });
        }
    } else {
        for (name, actual_digest) in actual_map.drain() {
            matched += 1;
            items.push(ArtifactValidationItem {
                filename: name,
                expected_hash: None,
                actual_hash: Some(actual_digest.hash),
                expected_bytes: None,
                actual_bytes: Some(actual_digest.bytes),
                status: ArtifactValidationStatus::Matched,
            });
        }
    }

    let summary = ArtifactReportSummary {
        matched,
        missing,
        extra,
        mismatched,
        total_manifest: (matched + missing + mismatched) as u32,
        total_extracted: (matched + mismatched + extra) as u32,
    };

    if missing > 0 {
        warnings.push(format!("缺少 {} 个文件", missing));
    }
    if extra > 0 {
        warnings.push(format!("存在 {} 个额外文件", extra));
    }
    if mismatched > 0 {
        warnings.push(format!("{} 个文件的哈希不一致", mismatched));
    }

    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    let report = ArtifactReport {
        job_id: request.job_id.clone(),
        artifact_path: PathBuf::from(request.artifact_path.clone()),
        extract_path: prepared.extract_root.clone(),
        manifest_path: request.manifest_path.clone(),
        hash: prepared.artifact_hash.clone(),
        created_at,
        summary,
        items: items.clone(),
        warnings: warnings.clone(),
        report_path: None,
        archive_path: Some(prepared.archive_path.clone()),
    };

    let mut file = File::create(&cache_report_path)?;
    serde_json::to_writer_pretty(&mut file, &report)
        .map_err(|err| ArtifactError::ReportWrite(cache_report_path.clone(), err))?;
    file.write_all(b"\n")?;

    let mut final_report = report;
    final_report.report_path = Some(cache_report_path);
    final_report.archive_path = Some(prepared.archive_path);

    Ok(final_report)
}

pub fn fetch_job_state(request: JobStatusRequest) -> Result<JobStatusSnapshot, JobError> {
    let url = build_service_endpoint(&request.service_url, &format!("jobs/{}", request.job_id))?;
    let client = Client::new();
    let mut req = client.get(url);

    if let Some(token) = request.bearer_token.as_deref() {
        req = req.bearer_auth(token);
    }

    let response = req.send()?;
    if !response.status().is_success() {
        return Err(JobError::UnexpectedStatus(response.status()));
    }

    let snapshot = response.json::<JobStatusSnapshot>()?;
    Ok(snapshot)
}

pub async fn watch_job_events(app: AppHandle, request: JobWatchRequest) -> Result<(), JobError> {
    match connect_job_websocket(&request).await {
        Ok(mut ws_stream) => {
            let mut last_snapshot: Option<JobStatusSnapshot> = None;

            while let Some(message) = ws_stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<JobStatusSnapshot>(&text) {
                            Ok(snapshot) => {
                                let envelope = JobEventEnvelope::from_snapshot(
                                    snapshot.clone(),
                                    JobEventTransport::Websocket,
                                );
                                app.emit(JOB_EVENT_NAME, &envelope)?;
                                last_snapshot = Some(snapshot.clone());
                                if is_terminal_status(&snapshot.status) {
                                    return Ok(());
                                }
                            }
                            Err(err) => {
                                let error = JobEventEnvelope::system_error(
                                    request.job_id.clone(),
                                    format!("无法解析 WebSocket 消息: {}", err),
                                );
                                app.emit(JOB_EVENT_NAME, &error)?;
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        ws_stream.send(Message::Pong(payload)).await?;
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Binary(_)) => continue,
                    Ok(Message::Pong(_)) => continue,
                    Ok(Message::Frame(_)) => continue,
                    Err(err) => {
                        let error = JobEventEnvelope::system_error(
                            request.job_id.clone(),
                            format!("WebSocket 连接中断: {}", err),
                        );
                        app.emit(JOB_EVENT_NAME, &error)?;
                        break;
                    }
                }
            }

            if let Some(snapshot) = last_snapshot {
                if is_terminal_status(&snapshot.status) {
                    return Ok(());
                }
            }

            poll_job_events(app, request).await
        }
        Err(err) => {
            let fallback_notice = JobEventEnvelope::system_error(
                request.job_id.clone(),
                format!("WebSocket 不可用，改用轮询：{}", err),
            );
            if let Err(emit_err) = app.emit(JOB_EVENT_NAME, &fallback_notice) {
                return Err(JobError::Emit(emit_err));
            }
            poll_job_events(app, request).await
        }
    }
}

async fn poll_job_events(app: AppHandle, request: JobWatchRequest) -> Result<(), JobError> {
    let interval_ms = request.poll_interval_ms.unwrap_or(1000).max(250);
    let interval = Duration::from_millis(interval_ms);
    let status_request = JobStatusRequest {
        service_url: request.service_url.clone(),
        job_id: request.job_id.clone(),
        bearer_token: request.bearer_token.clone(),
    };

    loop {
        let cloned = status_request.clone();
        let snapshot = async_runtime::spawn_blocking(move || fetch_job_state(cloned)).await??;
        let envelope =
            JobEventEnvelope::from_snapshot(snapshot.clone(), JobEventTransport::Polling);
        app.emit(JOB_EVENT_NAME, &envelope)?;

        if is_terminal_status(&snapshot.status) {
            break;
        }

        sleep(interval).await;
    }

    Ok(())
}

async fn connect_job_websocket(
    request: &JobWatchRequest,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, JobError> {
    let url = build_websocket_endpoint(&request.service_url, &request.job_id)?;
    let mut ws_request = url.into_client_request()?;

    if let Some(token) = request.bearer_token.as_deref() {
        let value = format!("Bearer {}", token);
        let header = HeaderValue::from_str(&value)
            .map_err(|err| JobError::InvalidResponse(format!("invalid bearer token: {}", err)))?;
        ws_request.headers_mut().insert(AUTHORIZATION, header);
    }

    let (stream, _response) = connect_async(ws_request).await?;
    Ok(stream)
}

fn build_service_endpoint(base: &str, path: &str) -> Result<Url, JobError> {
    if base.trim().is_empty() {
        return Err(JobError::InvalidServiceUrl);
    }

    let mut url = Url::parse(base.trim())?;
    ensure_trailing_slash(&mut url);
    let joined = url.join(path)?;
    Ok(joined)
}

fn build_websocket_endpoint(base: &str, job_id: &str) -> Result<Url, JobError> {
    if base.trim().is_empty() {
        return Err(JobError::InvalidServiceUrl);
    }

    let mut url = Url::parse(base.trim())?;
    if let Some(target) = match url.scheme() {
        "https" => Some("wss"),
        "http" => Some("ws"),
        _ => None,
    } {
        url.set_scheme(target)
            .map_err(|_| JobError::InvalidServiceUrl)?;
    }
    ensure_trailing_slash(&mut url);
    let endpoint = format!("ws/jobs/{}", job_id);
    let joined = url.join(&endpoint)?;
    Ok(joined)
}

fn ensure_trailing_slash(url: &mut Url) {
    if !url.path().ends_with('/') {
        let mut path = url.path().to_string();
        if !path.ends_with('/') {
            path.push('/');
        }
        url.set_path(&path);
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "SUCCESS" | "FAILED")
}

fn collect_sorted_files(directory: &Path) -> Result<Vec<(PathBuf, String)>, UploadError> {
    let mut files = Vec::new();

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }

        let path = entry.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            continue;
        }
        let file_name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| UploadError::NonUtf8Path(path.clone()))?
            .to_string();

        files.push((path, file_name));
    }

    files.sort_by(|a, b| compare(&a.1, &b.1));
    Ok(files)
}

#[derive(Debug, Clone)]
struct FileDigest {
    bytes: u64,
    hash: String,
}

fn read_manifest_expectations(path: &Path) -> Result<HashMap<String, FileDigest>, ArtifactError> {
    let manifest_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let file =
        File::open(path).map_err(|err| ArtifactError::ManifestRead(path.to_path_buf(), err))?;
    let reader = io::BufReader::new(file);

    #[derive(Deserialize)]
    struct ManifestEntry {
        target: String,
    }

    #[derive(Deserialize)]
    struct ManifestFileRead {
        files: Vec<ManifestEntry>,
    }

    let manifest: ManifestFileRead = serde_json::from_reader(reader)
        .map_err(|err| ArtifactError::ManifestParse(path.to_path_buf(), err))?;

    let mut expectations = HashMap::new();
    for entry in manifest.files {
        let target_path = manifest_dir.join(&entry.target);
        if !target_path.exists() {
            continue;
        }
        let digest = compute_file_digest(&target_path)?;
        expectations.insert(entry.target, digest);
    }

    Ok(expectations)
}

fn collect_directory_digests(root: &Path) -> Result<HashMap<String, FileDigest>, ArtifactError> {
    let mut digests = HashMap::new();

    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.into_path();
        if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
            let digest = compute_file_digest(&path)?;
            digests.insert(name.to_string(), digest);
        }
    }

    Ok(digests)
}

fn compute_file_digest(path: &Path) -> Result<FileDigest, ArtifactError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut total = 0u64;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }

    let hash = hex::encode(hasher.finalize());
    Ok(FileDigest { bytes: total, hash })
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use httpmock::MockServer;
    use serde_json::json;
    use std::io::Cursor;
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::FileOptions;

    #[test]
    fn job_params_payload_defaults() {
        let params = JobParamsPayload::default();
        assert_eq!(params.scale, 2);
        assert_eq!(params.model, "RealESRGAN_x4plus_anime_6B");
        assert_eq!(params.denoise, "medium");
        assert_eq!(params.output_format, "jpg");
        assert_eq!(params.jpeg_quality, 95);
        assert_eq!(params.tile_size, None);
        assert_eq!(params.tile_pad, None);
        assert_eq!(params.batch_size, None);
        assert_eq!(params.device, "auto");
    }

    fn build_zip_archive(files: Vec<(&str, &[u8])>) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);

        for (name, content) in files {
            writer.start_file(name, options).unwrap();
            writer.write_all(content).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn validate_artifact_matches_manifest() {
        let temp = tempdir().unwrap();
        let manifest_dir = temp.path().join("manifest");
        fs::create_dir_all(&manifest_dir).unwrap();

        let expected_path = manifest_dir.join("0001.jpg");
        fs::write(&expected_path, b"expected").unwrap();

        let manifest_path = manifest_dir.join("manifest.json");
        let manifest = json!({
            "files": [
                {"source": "0001.jpg", "target": "0001.jpg"}
            ],
            "skipped": []
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let zip_bytes = build_zip_archive(vec![("0001.jpg", b"expected")]);

        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(200)
                .header("content-type", "application/zip")
                .body(zip_bytes.clone());
        });

        let request = ArtifactDownloadRequest {
            service_url: server.base_url(),
            job_id: "test".to_string(),
            artifact_path: "artifacts/test.zip".to_string(),
            target_dir: temp.path().join("output"),
            bearer_token: None,
            manifest_path: Some(manifest_path.clone()),
            expected_hash: None,
            metadata: Some(JobMetadataSnapshot {
                title: Some("MyTitle".to_string()),
                volume: Some("1".to_string()),
            }),
        };

        let report = validate_artifact(request).expect("report");
        assert_eq!(report.summary.matched, 1);
        assert_eq!(report.summary.mismatched, 0);
        assert_eq!(report.summary.missing, 0);
        assert_eq!(report.summary.extra, 0);
        let expected_archive = temp.path().join("output").join("0001_MyTitle.zip");
        assert_eq!(report.archive_path.as_ref(), Some(&expected_archive));
        assert!(expected_archive.exists());
    }

    #[test]
    fn validate_artifact_detects_mismatch() {
        let temp = tempdir().unwrap();
        let manifest_dir = temp.path().join("manifest");
        fs::create_dir_all(&manifest_dir).unwrap();

        let expected_path = manifest_dir.join("0001.jpg");
        fs::write(&expected_path, b"expected").unwrap();

        let manifest_path = manifest_dir.join("manifest.json");
        let manifest = json!({
            "files": [
                {"source": "0001.jpg", "target": "0001.jpg"}
            ],
            "skipped": []
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let zip_bytes = build_zip_archive(vec![("0001.jpg", b"different")]);

        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(200)
                .header("content-type", "application/zip")
                .body(zip_bytes.clone());
        });

        let request = ArtifactDownloadRequest {
            service_url: server.base_url(),
            job_id: "test".to_string(),
            artifact_path: "artifacts/test.zip".to_string(),
            target_dir: temp.path().join("output"),
            bearer_token: None,
            manifest_path: Some(manifest_path.clone()),
            expected_hash: None,
            metadata: Some(JobMetadataSnapshot {
                title: Some("MyTitle".to_string()),
                volume: Some("1".to_string()),
            }),
        };

        let report = validate_artifact(request).expect("report");
        assert_eq!(report.summary.mismatched, 1);
        assert_eq!(report.summary.matched, 0);
        let expected_archive = temp.path().join("output").join("0001_MyTitle.zip");
        assert_eq!(report.archive_path.as_ref(), Some(&expected_archive));
        assert!(expected_archive.exists());
    }

    #[test]
    fn validate_artifact_ignores_etag_without_cache() {
        let temp = tempdir().unwrap();

        let zip_bytes = build_zip_archive(vec![("0001.jpg", b"payload")]);

        let server = MockServer::start();
        let etag_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact")
                .header("if-none-match", "abc123");
            then.status(304);
        });

        let success_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(200)
                .header("content-type", "application/zip")
                .body(zip_bytes.clone());
        });

        let request = ArtifactDownloadRequest {
            service_url: server.base_url(),
            job_id: "test".to_string(),
            artifact_path: "artifacts/test.zip".to_string(),
            target_dir: temp.path().join("output"),
            bearer_token: None,
            manifest_path: None,
            expected_hash: Some("abc123".to_string()),
            metadata: Some(JobMetadataSnapshot {
                title: Some("Sample".to_string()),
                volume: Some("2".to_string()),
            }),
        };

        let report = validate_artifact(request).expect("report");
        assert_eq!(report.summary.matched, 1);
        let expected_archive = temp.path().join("output").join("0002_Sample.zip");
        assert_eq!(report.archive_path.as_ref(), Some(&expected_archive));
        assert!(expected_archive.exists());
        assert_eq!(etag_mock.hits(), 0);
        success_mock.assert();
    }

    #[test]
    fn download_artifact_filters_non_images() {
        let temp = tempdir().unwrap();
        let output_dir = temp.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();

        let zip_bytes = build_zip_archive(vec![
            ("0001.jpg", b"image"),
            ("notes.txt", b"text"),
            ("nested/0002.png", b"image2"),
        ]);

        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(200)
                .header("content-type", "application/zip")
                .body(zip_bytes.clone());
        });

        let request = ArtifactDownloadRequest {
            service_url: server.base_url(),
            job_id: "test".to_string(),
            artifact_path: "artifacts/test.zip".to_string(),
            target_dir: output_dir.clone(),
            bearer_token: None,
            manifest_path: None,
            expected_hash: None,
            metadata: Some(JobMetadataSnapshot {
                title: Some("Title".to_string()),
                volume: Some("3".to_string()),
            }),
        };

        let summary = download_artifact(request).expect("summary");
        assert_eq!(summary.file_count, 2);
        let expected_archive = output_dir.join("0003_Title.zip");
        assert_eq!(summary.archive_path, expected_archive);
        assert!(expected_archive.exists());
        assert!(!summary.extract_path.exists());
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("已清理临时目录")));

        let file = File::open(&summary.archive_path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index).unwrap();
            if entry.name().ends_with('/') {
                continue;
            }
            names.push(entry.name().to_string());
        }
        names.sort();
        assert_eq!(
            names,
            vec!["0001.jpg".to_string(), "nested/0002.png".to_string()]
        );
    }

    #[test]
    fn validate_artifact_uses_cache_on_not_modified() {
        let temp = tempdir().unwrap();
        let manifest_dir = temp.path().join("manifest");
        fs::create_dir_all(&manifest_dir).unwrap();

        let expected_path = manifest_dir.join("0001.jpg");
        fs::write(&expected_path, b"expected").unwrap();

        let manifest_path = manifest_dir.join("manifest.json");
        let manifest = json!({
            "files": [
                {"source": "0001.jpg", "target": "0001.jpg"}
            ],
            "skipped": []
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let zip_bytes = build_zip_archive(vec![("0001.jpg", b"expected")]);

        let server = MockServer::start();
        let first_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(200)
                .header("content-type", "application/zip")
                .body(zip_bytes.clone());
        });

        let target_dir = temp.path().join("output");

        let request = ArtifactDownloadRequest {
            service_url: server.base_url(),
            job_id: "test".to_string(),
            artifact_path: "artifacts/test.zip".to_string(),
            target_dir: target_dir.clone(),
            bearer_token: None,
            manifest_path: Some(manifest_path.clone()),
            expected_hash: None,
            metadata: Some(JobMetadataSnapshot {
                title: Some("Another".to_string()),
                volume: Some("12".to_string()),
            }),
        };

        let first_report = validate_artifact(request.clone()).expect("first report");
        assert_eq!(first_report.summary.matched, 1);
        first_mock.assert();
        let expected_archive = target_dir.join("0012_Another.zip");
        assert_eq!(first_report.archive_path.as_ref(), Some(&expected_archive));
        assert!(expected_archive.exists());

        let second_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/jobs/test/artifact");
            then.status(304);
        });

        let mut cached_request = request.clone();
        cached_request.expected_hash = Some(first_report.hash.clone());

        let cached_report = validate_artifact(cached_request).expect("cached report");
        assert_eq!(cached_report.summary.matched, 1);
        assert!(cached_report
            .warnings
            .iter()
            .any(|warning| warning.contains("未变更")));
        let expected_report_path = target_dir.join("test").join("artifact-report.json");
        assert_eq!(
            cached_report.report_path.as_ref(),
            Some(&expected_report_path)
        );
        assert!(cached_report.extract_path.exists());
        assert_eq!(cached_report.archive_path.as_ref(), Some(&expected_archive));
        second_mock.assert();
    }
    #[test]
    fn resume_remote_job_posts_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/jobs/xyz/resume")
                .header("authorization", "Bearer secret")
                .json_body(json!({
                    "inputPath": "staging/vol1.zip",
                    "inputType": "zip"
                }));
            then.status(200).json_body(json!({
                "job_id": "xyz",
                "status": "PENDING",
                "processed": 0,
                "total": 0
            }));
        });

        let snapshot = resume_remote_job(JobControlRequest {
            service_url: server.url("/api"),
            job_id: "xyz".to_string(),
            bearer_token: Some("secret".to_string()),
            input_path: Some("staging/vol1.zip".to_string()),
            input_type: Some("zip".to_string()),
        })
        .expect("resume snapshot");

        assert_eq!(snapshot.job_id, "xyz");
        assert_eq!(snapshot.status, "PENDING");
        mock.assert();
    }

    #[test]
    fn cancel_remote_job_posts_without_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/jobs/xyz/cancel");
            then.status(200).json_body(json!({
                "job_id": "xyz",
                "status": "FAILED",
                "processed": 0,
                "total": 0,
                "message": "Cancelled"
            }));
        });

        let snapshot = cancel_remote_job(JobControlRequest {
            service_url: server.url("/api"),
            job_id: "xyz".to_string(),
            bearer_token: None,
            input_path: None,
            input_type: None,
        })
        .expect("cancel snapshot");

        assert_eq!(snapshot.status, "FAILED");
        assert_eq!(snapshot.job_id, "xyz");
        mock.assert();
    }
}

fn upload_as_zip(
    app: Option<AppHandle>,
    remote_url: &str,
    files: &[(PathBuf, String)],
    bearer_token: Option<&str>,
    metadata: Option<&UploadMetadata>,
) -> Result<UploadOutcome, UploadError> {
    let file_count = files.len();
    emit_upload_event(
        app.as_ref(),
        UploadProgress {
            stage: UploadProgressStage::Preparing,
            transferred_bytes: 0,
            total_bytes: 0,
            processed_files: 0,
            total_files: file_count,
            message: Some("开始整理文件".to_string()),
        },
    );

    let (zip_path, zipped_bytes) = create_zip_archive_with_progress(app.as_ref(), files)?;
    let total_bytes = fs::metadata(&zip_path)?.len();
    let file = File::open(&zip_path)?;
    let reader = ProgressReader::new(app.clone(), file, total_bytes, file_count);

    let client = Client::builder().build()?;
    let mut request = client
        .put(remote_url)
        .header("Content-Type", "application/zip");

    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }

    if let Some(meta) = metadata {
        if let Some(title) = meta.title.as_deref() {
            request = request.header("X-Reichan-Title", title);
        }
        if let Some(volume) = meta.volume.as_deref() {
            request = request.header("X-Reichan-Volume", volume);
        }
    }

    let response = request.body(reqwest::blocking::Body::new(reader)).send();
    match response {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                emit_upload_event(
                    app.as_ref(),
                    UploadProgress {
                        stage: UploadProgressStage::Failed,
                        transferred_bytes: total_bytes,
                        total_bytes,
                        processed_files: file_count,
                        total_files: file_count,
                        message: Some(format!("上传失败: {}", status)),
                    },
                );
                fs::remove_file(&zip_path).ok();
                return Err(UploadError::UnexpectedStatus(status));
            }
        }
        Err(err) => {
            emit_upload_event(
                app.as_ref(),
                UploadProgress {
                    stage: UploadProgressStage::Failed,
                    transferred_bytes: 0,
                    total_bytes,
                    processed_files: file_count,
                    total_files: file_count,
                    message: Some(format!("上传失败: {}", err)),
                },
            );
            fs::remove_file(&zip_path).ok();
            return Err(UploadError::Request(err));
        }
    }

    emit_upload_event(
        app.as_ref(),
        UploadProgress {
            stage: UploadProgressStage::Finalizing,
            transferred_bytes: total_bytes,
            total_bytes,
            processed_files: file_count,
            total_files: file_count,
            message: Some("服务器已接收，处理中".to_string()),
        },
    );

    fs::remove_file(&zip_path).ok();

    emit_upload_event(
        app.as_ref(),
        UploadProgress {
            stage: UploadProgressStage::Completed,
            transferred_bytes: total_bytes,
            total_bytes,
            processed_files: file_count,
            total_files: file_count,
            message: Some("上传完成".to_string()),
        },
    );

    Ok(UploadOutcome {
        remote_url: remote_url.to_string(),
        uploaded_bytes: zipped_bytes,
        file_count,
        mode: UploadMode::Zip,
    })
}

fn create_zip_archive_with_progress(
    app: Option<&AppHandle>,
    files: &[(PathBuf, String)],
) -> Result<(PathBuf, u64), UploadError> {
    let timestamp = Utc::now().timestamp_millis();
    let file_name = format!("rei-manga-{}-{}.zip", std::process::id(), timestamp);
    let temp_path = std::env::temp_dir().join(file_name);

    let file = File::create(&temp_path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let total_files = files.len();
    for (index, (path, name)) in files.iter().enumerate() {
        writer.start_file(name, options)?;
        let mut source = File::open(path)?;
        io::copy(&mut source, &mut writer)?;
        emit_upload_event(
            app,
            UploadProgress {
                stage: UploadProgressStage::Preparing,
                transferred_bytes: 0,
                total_bytes: 0,
                processed_files: index + 1,
                total_files,
                message: Some(format!("已打包 {}/{}", index + 1, total_files)),
            },
        );
    }

    let file = writer.finish()?;
    let size = file.metadata()?.len();
    drop(file);

    Ok((temp_path, size))
}

fn build_remote_url(base: &str, path: &str) -> String {
    let trimmed_base = base.trim_end_matches('/');
    let trimmed_path = path.trim_start_matches('/');

    if trimmed_base.is_empty() {
        if trimmed_path.is_empty() {
            String::new()
        } else {
            format!("/{}", trimmed_path)
        }
    } else if trimmed_path.is_empty() {
        trimmed_base.to_string()
    } else {
        format!("{}/{}", trimmed_base, trimmed_path)
    }
}

fn emit_upload_event(app: Option<&AppHandle>, progress: UploadProgress) {
    if let Some(handle) = app {
        let _ = handle.emit(UPLOAD_EVENT_NAME, progress);
    }
}

struct ProgressReader<R> {
    inner: R,
    emitted: u64,
    total: u64,
    app: Option<AppHandle>,
    total_files: usize,
}

impl<R> ProgressReader<R> {
    fn new(app: Option<AppHandle>, inner: R, total: u64, total_files: usize) -> Self {
        emit_upload_event(
            app.as_ref(),
            UploadProgress {
                stage: UploadProgressStage::Uploading,
                transferred_bytes: 0,
                total_bytes: total,
                processed_files: total_files,
                total_files,
                message: Some("开始上传".to_string()),
            },
        );

        Self {
            inner,
            emitted: 0,
            total,
            app,
            total_files,
        }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        if read > 0 {
            self.emitted += read as u64;
            emit_upload_event(
                self.app.as_ref(),
                UploadProgress {
                    stage: UploadProgressStage::Uploading,
                    transferred_bytes: self.emitted,
                    total_bytes: self.total,
                    processed_files: self.total_files,
                    total_files: self.total_files,
                    message: None,
                },
            );
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doublepage::{
        apply_manual_splits, prepare_manual_split_workspace, ManualSplitApplyRequest,
        ManualSplitLine, PrepareManualSplitWorkspaceRequest,
    };
    use httpmock::prelude::*;
    use serde_json::json;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str) {
        let path = dir.join(name);
        let mut file = File::create(path).expect("create file");
        file.write_all(b"test").expect("write file");
    }

    #[test]
    fn analyze_directory_single_volume_detects_root_images() {
        let temp = TempDir::new().expect("temp dir");
        write_file(temp.path(), "001.png");
        write_file(temp.path(), "002.jpeg");
        write_file(temp.path(), "notes.txt");

        let analysis = analyze_manga_directory(temp.path().to_path_buf()).expect("analysis");

        assert_eq!(analysis.mode, MangaSourceMode::SingleVolume);
        assert_eq!(analysis.root_image_count, 2);
        assert_eq!(analysis.total_images, 2);
        assert!(analysis.volume_candidates.is_empty());
        assert!(analysis
            .skipped_entries
            .iter()
            .any(|item| item.contains("notes.txt")));
    }

    #[test]
    fn analyze_directory_multi_volume_collects_candidates() {
        let temp = TempDir::new().expect("temp dir");
        let volume_a = temp.path().join("Vol_01");
        let volume_b = temp.path().join("Special 3");
        fs::create_dir_all(&volume_a).expect("dir a");
        fs::create_dir_all(&volume_b).expect("dir b");

        write_file(&volume_a, "p1.jpg");
        write_file(&volume_a, "p2.jpg");
        write_file(&volume_b, "scan.png");

        let analysis = analyze_manga_directory(temp.path().to_path_buf()).expect("analysis");

        assert_eq!(analysis.mode, MangaSourceMode::MultiVolume);
        assert_eq!(analysis.root_image_count, 0);
        assert_eq!(analysis.total_images, 3);
        assert_eq!(analysis.volume_candidates.len(), 2);

        let mut numbers: Vec<Option<u32>> = analysis
            .volume_candidates
            .iter()
            .map(|item| item.detected_number)
            .collect();
        numbers.sort();
        assert_eq!(numbers, vec![Some(1), Some(3)]);
    }

    #[test]
    fn rename_dry_run_collects_preview_without_touching_files() {
        let temp = TempDir::new().expect("temp dir");
        write_file(temp.path(), "page10.png");
        write_file(temp.path(), "page1.jpg");
        write_file(temp.path(), "page02.jpeg");

        let result = perform_rename(RenameOptions {
            directory: temp.path().to_path_buf(),
            pad: 4,
            target_extension: "jpg".to_string(),
            dry_run: true,
            split: RenameSplitOptions::default(),
        })
        .expect("rename result");

        assert_eq!(result.entries.len(), 3);
        assert!(result.dry_run);
        assert!(result.manifest_path.is_none());
        let mut renamed: Vec<&str> = result
            .entries
            .iter()
            .map(|entry| entry.renamed_name.as_str())
            .collect();
        renamed.sort();
        assert_eq!(renamed, ["0001.jpg", "0002.jpg", "0010.jpg"]);
        assert!(!result.split_applied);
        assert!(result.split_workspace.is_none());
        assert!(temp.path().join("page10.png").exists());
        assert!(temp.path().join("page1.jpg").exists());
        assert!(temp.path().join("page02.jpeg").exists());
    }

    #[test]
    fn rename_persists_manifest_and_new_filenames() {
        let temp = TempDir::new().expect("temp dir");
        write_file(temp.path(), "p1.png");
        write_file(temp.path(), "p2.png");

        let result = perform_rename(RenameOptions {
            directory: temp.path().to_path_buf(),
            pad: 4,
            target_extension: "jpg".to_string(),
            dry_run: false,
            split: RenameSplitOptions::default(),
        })
        .expect("rename result");

        assert!(!result.dry_run);
        let manifest_path = result.manifest_path.expect("manifest path");
        assert!(manifest_path.exists());
        assert!(temp.path().join("0001.jpg").exists());
        assert!(temp.path().join("0002.jpg").exists());
        assert!(!temp.path().join("p1.png").exists());
        assert!(!result.split_applied);
        assert!(result.split_workspace.is_none());

        let manifest_text = fs::read_to_string(manifest_path).expect("read manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_text).expect("parse json");
        assert_eq!(manifest_json["files"].as_array().unwrap().len(), 2);
        assert_eq!(manifest_json["files"][0]["target"], "0001.jpg");
    }

    #[test]
    fn rename_prefers_numeric_suffix_when_available() {
        let temp = TempDir::new().expect("temp dir");
        write_file(temp.path(), "Dl-Raw.net-01.jpg");
        write_file(temp.path(), "Dl-Raw.net-010.jpg");
        write_file(temp.path(), "Dl-Raw.net-0100.jpg");

        let result = perform_rename(RenameOptions {
            directory: temp.path().to_path_buf(),
            pad: 4,
            target_extension: "jpg".to_string(),
            dry_run: true,
            split: RenameSplitOptions::default(),
        })
        .expect("rename result");

        let mut targets: Vec<String> = result
            .entries
            .iter()
            .map(|entry| entry.renamed_name.clone())
            .collect();
        targets.sort();
        assert_eq!(targets, vec!["0001.jpg", "0010.jpg", "0100.jpg"]);
    }

    #[test]
    fn rename_attaches_manual_overrides_when_only_manual_applied() {
        use image::{ImageBuffer, Rgba};

        fn write_image(path: &Path) {
            let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_fn(1200, 800, |_, _| Rgba([200, 208, 220, 255]));
            buffer
                .save_with_format(path, image::ImageFormat::Png)
                .expect("save image");
        }

        let temp = TempDir::new().expect("temp dir");
        let source_dir = temp.path().join("source");
        fs::create_dir_all(&source_dir).expect("create source");
        write_image(&source_dir.join("page_001.png"));
        write_image(&source_dir.join("page_002.png"));

        let setup = prepare_manual_split_workspace(PrepareManualSplitWorkspaceRequest {
            source_directory: source_dir.clone(),
            workspace_root: None,
            overwrite: true,
        })
        .expect("prepare manual workspace");

        let overrides: Vec<ManualSplitLine> = setup
            .entries
            .iter()
            .map(|entry| ManualSplitLine {
                source: entry.source_path.clone(),
                left_trim: 0.05,
                left_page_end: 0.48,
                right_page_start: 0.52,
                right_trim: 0.95,
                gutter_ratio: None,
                locked: false,
                image_kind: ManualImageKind::Content,
                rotate90: false,
            })
            .collect();

        apply_manual_splits(
            ManualSplitApplyRequest {
                workspace: setup.workspace.clone(),
                overrides,
                accelerator: EdgeTextureAcceleratorPreference::Cpu,
                generate_preview: false,
            },
            None,
        )
        .expect("apply manual splits");

        let result = perform_rename(RenameOptions {
            directory: source_dir.clone(),
            pad: 4,
            target_extension: "jpg".to_string(),
            dry_run: false,
            split: RenameSplitOptions {
                enabled: true,
                workspace: Some(setup.workspace.clone()),
                report_path: None,
                summary: None,
                warnings: None,
            },
        })
        .expect("rename with manual workspace");

        assert!(result.split_applied);
        assert_eq!(result.split_workspace, Some(setup.workspace.clone()));
        assert!(result.split_manual_overrides);
        let manual_entries = result.manual_entries.expect("manual entries");
        assert_eq!(manual_entries.len(), 2);
        assert!(manual_entries.iter().all(|entry| entry.outputs.len() == 2));
        assert!(result
            .warnings
            .iter()
            .all(|warning| !warning.contains("manual split report not found")));
        assert!(setup.workspace.join("0001.jpg").exists());
        assert!(setup.workspace.join("0002.jpg").exists());
    }

    #[test]
    fn upload_directory_as_zip_hits_remote_endpoint() {
        let temp = TempDir::new().expect("temp dir");
        write_file(temp.path(), "a.jpg");
        write_file(temp.path(), "b.jpg");

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/incoming/title-volume.zip")
                .header("content-type", "application/zip");
            then.status(201).body("ok");
        });

        let result = perform_upload(
            None,
            UploadRequest {
                service_url: server.url(""),
                remote_path: "/incoming/title-volume.zip".to_string(),
                local_path: temp.path().to_path_buf(),
                mode: UploadMode::Zip,
                bearer_token: None,
                metadata: Some(UploadMetadata {
                    title: Some("Title".to_string()),
                    volume: Some("Volume".to_string()),
                }),
            },
        )
        .expect("upload result");

        mock.assert();
        assert_eq!(
            result.remote_url,
            format!("{}/incoming/title-volume.zip", server.url(""))
        );
        assert_eq!(result.file_count, 2);
        assert_eq!(result.mode, UploadMode::Zip);
        assert!(result.uploaded_bytes > 0);
    }

    #[test]
    fn create_remote_job_posts_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/jobs")
                .header("authorization", "Bearer token")
                .json_body(json!({
                    "title": "Title",
                    "volume": "Vol",
                    "input": {"type": "zip", "path": "incoming/file.zip"},
                    "params": {
                        "scale": 2,
                        "model": "RealESRGAN_x4plus_anime_6B",
                        "denoise": "medium"
                    }
                }));
            then.status(202).json_body(json!({"job_id": "abc"}));
        });

        let submission = create_remote_job(CreateJobOptions {
            service_url: server.url("/api"),
            bearer_token: Some("token".to_string()),
            payload: JobPayload {
                title: "Title".to_string(),
                volume: "Vol".to_string(),
                input: JobInputPayload {
                    kind: "zip".to_string(),
                    path: "incoming/file.zip".to_string(),
                },
                params: JobParamsPayload::default(),
            },
        })
        .expect("submission");

        assert_eq!(submission.job_id, "abc");
        mock.assert();
    }

    #[test]
    fn fetch_job_state_parses_snapshot() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/jobs/xyz");
            then.status(200).json_body(json!({
                "job_id": "xyz",
                "status": "RUNNING",
                "processed": 5,
                "total": 48,
                "artifact_path": null,
                "message": "processing"
            }));
        });

        let snapshot = fetch_job_state(JobStatusRequest {
            service_url: server.url("/"),
            job_id: "xyz".to_string(),
            bearer_token: None,
        })
        .expect("snapshot");

        assert_eq!(snapshot.job_id, "xyz");
        assert_eq!(snapshot.status, "RUNNING");
        assert_eq!(snapshot.processed, 5);
        assert_eq!(snapshot.total, 48);
        assert_eq!(snapshot.message.as_deref(), Some("processing"));
        assert!(snapshot.artifact_path.is_none());
        mock.assert();
    }
}
