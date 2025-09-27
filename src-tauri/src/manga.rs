use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use http::header::AUTHORIZATION;
use http::HeaderValue;
use natord::compare;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json;
use tauri::{async_runtime, AppHandle, Emitter};
use tokio::net::TcpStream;
use tokio::task::JoinError;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;
use zip::write::FileOptions;
use zip::CompressionMethod;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenameOutcome {
    pub directory: PathBuf,
    pub manifest_path: Option<PathBuf>,
    pub entries: Vec<RenameEntry>,
    pub dry_run: bool,
    pub warnings: Vec<String>,
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

    Ok(MangaSourceAnalysis {
        root: directory,
        mode,
        root_image_count,
        total_images,
        volume_candidates,
        skipped_entries,
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
}

#[derive(Serialize)]
struct ManifestEntryData {
    source: String,
    target: String,
}

pub fn perform_rename(options: RenameOptions) -> Result<RenameOutcome, RenameError> {
    let RenameOptions {
        directory,
        pad,
        target_extension,
        dry_run,
    } = options;

    if !directory.exists() || !directory.is_dir() {
        return Err(RenameError::DirectoryNotFound(directory));
    }

    let normalized_pad = pad.max(1);
    let normalized_extension = target_extension.to_ascii_lowercase();

    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for entry in fs::read_dir(&directory)? {
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
                candidates.push((path.clone(), file_name));
            }
            _ => skipped.push(file_name),
        }
    }

    if candidates.is_empty() {
        return Err(RenameError::EmptyDirectory(directory));
    }

    candidates.sort_by(|a, b| compare(&a.1, &b.1));

    let mut entries = Vec::with_capacity(candidates.len());
    for (index, (_, original)) in candidates.iter().enumerate() {
        let renamed = format!(
            "{:0width$}.{}",
            index + 1,
            normalized_extension,
            width = normalized_pad
        );
        entries.push(RenameEntry {
            original_name: original.clone(),
            renamed_name: renamed,
        });
    }

    let warnings = build_warnings(&skipped);

    if dry_run {
        return Ok(RenameOutcome {
            directory,
            manifest_path: None,
            entries,
            dry_run: true,
            warnings,
        });
    }

    let temp_prefix = format!(".rei_tmp_{}", std::process::id());
    let mut temp_paths = Vec::with_capacity(candidates.len());

    for (index, (original_path, _)) in candidates.iter().enumerate() {
        let temp_name = format!("{}_{:04}", temp_prefix, index);
        let temp_path = directory.join(&temp_name);
        fs::rename(original_path, &temp_path)?;
        temp_paths.push(temp_path);
    }

    for (index, temp_path) in temp_paths.iter().enumerate() {
        let final_path = directory.join(&entries[index].renamed_name);
        fs::rename(temp_path, &final_path)?;
    }

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
    };

    let manifest_path = directory.join("manifest.json");
    let mut manifest_file = File::create(&manifest_path)?;
    serde_json::to_writer_pretty(&mut manifest_file, &manifest)?;
    manifest_file.write_all(b"\n")?;

    Ok(RenameOutcome {
        directory,
        manifest_path: Some(manifest_path),
        entries,
        dry_run: false,
        warnings,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobParamsPayload {
    #[serde(default = "default_scale")]
    pub scale: u32,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_denoise")]
    pub denoise: String,
}

impl Default for JobParamsPayload {
    fn default() -> Self {
        Self {
            scale: default_scale(),
            model: default_model(),
            denoise: default_denoise(),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobSubmission {
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
    pub job_id: String,
    pub status: String,
    pub processed: u32,
    pub total: u32,
    #[serde(default)]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
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
        })
        .expect("rename result");

        assert_eq!(result.entries.len(), 3);
        assert!(result.dry_run);
        assert!(result.manifest_path.is_none());
        assert_eq!(result.entries[0].renamed_name, "0001.jpg");
        assert_eq!(result.entries[1].renamed_name, "0002.jpg");
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
        })
        .expect("rename result");

        assert!(!result.dry_run);
        let manifest_path = result.manifest_path.expect("manifest path");
        assert!(manifest_path.exists());
        assert!(temp.path().join("0001.jpg").exists());
        assert!(temp.path().join("0002.jpg").exists());
        assert!(!temp.path().join("p1.png").exists());

        let manifest_text = fs::read_to_string(manifest_path).expect("read manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_text).expect("parse json");
        assert_eq!(manifest_json["files"].as_array().unwrap().len(), 2);
        assert_eq!(manifest_json["files"][0]["target"], "0001.jpg");
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
