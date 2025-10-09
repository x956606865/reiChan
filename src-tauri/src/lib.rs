mod doublepage;
mod manga;
mod notion;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{async_runtime, Emitter, Manager};

use rusqlite::{params, Connection};

#[cfg(feature = "notion-sqlite")]
use crate::notion::storage::SqliteTokenStore;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortUsage {
    protocol: String,
    local_address: String,
    local_port: Option<u16>,
    remote_address: Option<String>,
    remote_port: Option<u16>,
    pid: Option<u32>,
    process_name: Option<String>,
    parent_pid: Option<u32>,
    parent_process_name: Option<String>,
    ancestors: Vec<ProcessLink>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcessLink {
    pid: u32,
    process_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FavoriteRecord {
    protocol: String,
    local_address: String,
    local_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FavoritePayload {
    protocol: String,
    local_address: String,
    local_port: Option<u16>,
}

#[derive(Debug)]
struct AppState {
    db_path: PathBuf,
}

#[tauri::command]
fn list_ports() -> Result<Vec<PortUsage>, String> {
    collect_ports().map_err(|err| err.to_string())
}

#[tauri::command]
fn kill_port_process(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("Invalid PID".to_string());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        return kill_process_unix(pid).map_err(|err| err.to_string());
    }

    #[cfg(target_os = "windows")]
    {
        return kill_process_windows(pid).map_err(|err| err.to_string());
    }

    #[allow(unreachable_code)]
    Err("Unsupported platform".to_string())
}

#[tauri::command]
fn list_port_favorites(state: tauri::State<AppState>) -> Result<Vec<FavoriteRecord>, String> {
    with_connection(&state.db_path, |conn| {
        let mut stmt = conn.prepare(
            "SELECT protocol, local_address, local_port FROM port_favorites ORDER BY protocol, local_address",
        )?;
        let rows = stmt.query_map([], |row| {
            let port: Option<i64> = row.get(2)?;
            Ok(FavoriteRecord {
                protocol: row.get::<_, String>(0)?.to_uppercase(),
                local_address: row.get(1)?,
                local_port: port.map(|value| value as u16),
            })
        })?;

        let mut favorites = Vec::new();
        for entry in rows {
            favorites.push(entry?);
        }

        Ok(favorites)
    })
    .map_err(|err| err.to_string())
}

#[tauri::command]
fn update_port_favorite(
    state: tauri::State<AppState>,
    favorite: FavoritePayload,
    is_favorite: bool,
) -> Result<(), String> {
    with_connection(&state.db_path, |conn| {
        let protocol = favorite.protocol.to_uppercase();
        let local_port = favorite.local_port.map(|value| value as i64);

        if is_favorite {
            conn.execute(
                "INSERT OR IGNORE INTO port_favorites (protocol, local_address, local_port) VALUES (?1, ?2, ?3)",
                params![protocol, favorite.local_address, local_port],
            )?;
        } else {
            conn.execute(
                "DELETE FROM port_favorites WHERE protocol = ?1 AND local_address = ?2 AND ((local_port IS NULL AND ?3 IS NULL) OR local_port = ?3)",
                params![protocol, favorite.local_address, local_port],
            )?;
        }

        Ok(())
    })
    .map_err(|err| err.to_string())
}

#[tauri::command]
fn rename_manga_sequence(options: manga::RenameOptions) -> Result<manga::RenameOutcome, String> {
    manga::perform_rename(options).map_err(|err| err.to_string())
}

#[tauri::command]
async fn prepare_doublepage_split(
    app: tauri::AppHandle,
    options: doublepage::SplitCommandOptions,
) -> Result<doublepage::SplitCommandOutcome, String> {
    let handle = app.clone();

    async_runtime::spawn_blocking(move || {
        let mut progress = move |payload: doublepage::SplitProgress| {
            let _ = handle.emit(doublepage::SPLIT_PROGRESS_EVENT, payload);
        };

        doublepage::prepare_split(options, Some(&mut progress))
    })
    .await
    .map_err(|err| err.to_string())?
    .map_err(|err| err.to_string())
}

#[tauri::command]
async fn preview_edge_texture_trim(
    app: tauri::AppHandle,
    request: doublepage::EdgePreviewRequest,
) -> Result<doublepage::EdgePreviewResponse, String> {
    let cache_root = app.path().app_cache_dir().map_err(|err| err.to_string())?;

    async_runtime::spawn_blocking(move || {
        doublepage::preview_edge_texture_trim(&cache_root, request)
    })
    .await
    .map_err(|err| err.to_string())?
    .map_err(|err| err.to_string())
}

#[tauri::command]
async fn load_manual_split_context(
    request: doublepage::ManualSplitContextRequest,
) -> Result<doublepage::ManualSplitContext, String> {
    async_runtime::spawn_blocking(move || doublepage::load_manual_split_context(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn render_manual_split_preview(
    request: doublepage::ManualSplitPreviewRequest,
) -> Result<doublepage::ManualSplitPreviewResponse, String> {
    async_runtime::spawn_blocking(move || doublepage::render_manual_split_preview(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn prepare_manual_split_workspace(
    request: doublepage::PrepareManualSplitWorkspaceRequest,
) -> Result<doublepage::PrepareManualSplitWorkspaceResponse, String> {
    async_runtime::spawn_blocking(move || doublepage::prepare_manual_split_workspace(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn apply_manual_splits(
    app: tauri::AppHandle,
    request: doublepage::ManualSplitApplyRequest,
) -> Result<doublepage::ManualSplitApplyResponse, String> {
    let workspace = request.workspace.clone();
    let total = request.overrides.len();

    let _ = app.emit(
        doublepage::MANUAL_SPLIT_APPLY_STARTED_EVENT,
        doublepage::ManualSplitApplyStarted {
            workspace: workspace.clone(),
            total,
        },
    );

    let event_app = app.clone();
    let result = async_runtime::spawn_blocking(move || {
        let mut progress_callback = |payload: doublepage::ManualSplitProgress| {
            let _ = event_app.emit(doublepage::MANUAL_SPLIT_APPLY_PROGRESS_EVENT, payload);
        };
        doublepage::apply_manual_splits(request, Some(&mut progress_callback))
    })
    .await
    .map_err(|err| err.to_string())?;

    match result {
        Ok(response) => {
            let _ = app.emit(doublepage::MANUAL_SPLIT_APPLY_SUCCEEDED_EVENT, &response);
            Ok(response)
        }
        Err(err) => {
            let message = err.to_string();
            let _ = app.emit(
                doublepage::MANUAL_SPLIT_APPLY_FAILED_EVENT,
                doublepage::ManualSplitApplyFailed {
                    workspace,
                    message: message.clone(),
                },
            );
            Err(message)
        }
    }
}

#[tauri::command]
async fn revert_manual_splits(
    request: doublepage::ManualSplitRevertRequest,
) -> Result<doublepage::ManualSplitRevertResponse, String> {
    async_runtime::spawn_blocking(move || doublepage::revert_manual_splits(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn export_manual_split_template(
    request: doublepage::ManualSplitTemplateExportRequest,
) -> Result<doublepage::ManualSplitTemplateExportResponse, String> {
    async_runtime::spawn_blocking(move || doublepage::export_manual_split_template(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn track_manual_split_event(
    request: doublepage::ManualSplitTelemetryRequest,
) -> Result<(), String> {
    async_runtime::spawn_blocking(move || doublepage::track_manual_split_event(request))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn list_edge_preview_candidates(
    directory: PathBuf,
) -> Result<Vec<doublepage::EdgePreviewCandidate>, String> {
    doublepage::list_edge_preview_candidates(&directory).map_err(|err| err.to_string())
}

#[tauri::command]
fn analyze_manga_directory(directory: PathBuf) -> Result<manga::MangaSourceAnalysis, String> {
    manga::analyze_manga_directory(directory).map_err(|err| err.to_string())
}

#[tauri::command]
fn upload_copyparty(
    app: tauri::AppHandle,
    request: manga::UploadRequest,
) -> Result<manga::UploadOutcome, String> {
    manga::perform_upload(Some(app.clone()), request).map_err(|err| err.to_string())
}

#[tauri::command]
fn create_manga_job(options: manga::CreateJobOptions) -> Result<manga::JobSubmission, String> {
    manga::create_remote_job(options).map_err(|err| err.to_string())
}

#[tauri::command]
fn fetch_manga_job_status(
    request: manga::JobStatusRequest,
) -> Result<manga::JobStatusSnapshot, String> {
    manga::fetch_job_state(request).map_err(|err| err.to_string())
}

#[tauri::command]
async fn watch_manga_job(
    app: tauri::AppHandle,
    request: manga::JobWatchRequest,
) -> Result<(), String> {
    let job_id = request.job_id.clone();
    async_runtime::spawn({
        let app_handle = app.clone();
        async move {
            if let Err(err) = manga::watch_job_events(app_handle.clone(), request).await {
                let fallback = manga::JobEventEnvelope::system_error(job_id, err.to_string());
                let _ = app_handle.emit(manga::JOB_EVENT_NAME, &fallback);
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn resume_manga_job(request: manga::JobControlRequest) -> Result<manga::JobStatusSnapshot, String> {
    manga::resume_remote_job(request).map_err(|err| err.to_string())
}

#[tauri::command]
fn cancel_manga_job(request: manga::JobControlRequest) -> Result<manga::JobStatusSnapshot, String> {
    manga::cancel_remote_job(request).map_err(|err| err.to_string())
}

#[tauri::command]
fn download_manga_artifact(
    request: manga::ArtifactDownloadRequest,
) -> Result<manga::ArtifactDownloadSummary, String> {
    manga::download_artifact(request).map_err(|err| err.to_string())
}

#[tauri::command]
fn validate_manga_artifact(
    request: manga::ArtifactDownloadRequest,
) -> Result<manga::ArtifactReport, String> {
    manga::validate_artifact(request).map_err(|err| err.to_string())
}

#[tauri::command]
fn read_template_file(path: String) -> Result<String, String> {
    let resolved = PathBuf::from(&path);
    fs::read_to_string(&resolved)
        .map_err(|err| format!("无法读取模板文件 {}: {}", resolved.display(), err))
}

fn collect_ports() -> Result<Vec<PortUsage>, Box<dyn std::error::Error>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        return collect_ports_unix();
    }

    #[cfg(target_os = "windows")]
    {
        return collect_ports_windows();
    }

    #[allow(unreachable_code)]
    Err("Unsupported platform".into())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn kill_process_unix(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("kill").arg(pid.to_string()).output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No such process") {
        return Ok(());
    }

    Err(format!("kill failed: {}", stderr.trim()).into())
}

#[cfg(target_os = "windows")]
fn kill_process_windows(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not found") {
        return Ok(());
    }

    Err(format!("taskkill failed: {}", stderr.trim()).into())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn collect_ports_unix() -> Result<Vec<PortUsage>, Box<dyn std::error::Error>> {
    let output = Command::new("lsof")
        .args(["-nP", "-i", "-FpctunP"])
        .output()?;

    if !output.status.success() {
        return Err(format!("lsof exited with status {}", output.status).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ports = parse_lsof_output(&stdout)?;
    attach_process_tree_unix(&mut ports)?;
    Ok(ports)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_lsof_output(stdout: &str) -> Result<Vec<PortUsage>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    let mut current_pid: Option<u32> = None;
    let mut current_process: Option<String> = None;
    let mut current_protocol: Option<String> = None;

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }

        let (tag, value) = line.split_at(1);

        match tag {
            "p" => {
                current_pid = value.parse().ok();
                current_process = None;
            }
            "c" => {
                current_process = Some(value.to_string());
            }
            "f" => {
                current_protocol = None;
            }
            "P" => {
                current_protocol = Some(value.to_uppercase());
            }
            "n" => {
                let pid = current_pid;
                let protocol = current_protocol
                    .clone()
                    .unwrap_or_else(|| "UNKNOWN".to_string());
                let (local, remote) = parse_endpoint(value);
                let remote_address = remote.as_ref().map(|r| r.address.clone());
                let remote_port = remote.as_ref().and_then(|r| r.port);

                let key = format!(
                    "{pid:?}|{protocol}|{local_host}|{local_port:?}|{remote_host:?}|{remote_port:?}",
                    pid = pid,
                    protocol = protocol,
                    local_host = &local.address,
                    local_port = local.port,
                    remote_host = remote_address.clone().unwrap_or_default(),
                    remote_port = remote_port
                );

                if seen.insert(key) {
                    results.push(PortUsage {
                        protocol,
                        local_address: local.address,
                        local_port: local.port,
                        remote_address: remote_address,
                        remote_port,
                        pid,
                        process_name: current_process.clone(),
                        parent_pid: None,
                        parent_process_name: None,
                        ancestors: Vec::new(),
                    });
                }
            }
            _ => {}
        }
    }

    Ok(results)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone)]
struct Endpoint {
    address: String,
    port: Option<u16>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_endpoint(input: &str) -> (Endpoint, Option<Endpoint>) {
    if let Some((left, right)) = input.split_once("->") {
        (
            parse_single_endpoint(left),
            Some(parse_single_endpoint(right)),
        )
    } else {
        (parse_single_endpoint(input), None)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_single_endpoint(raw: &str) -> Endpoint {
    let value = raw.trim();
    if value.starts_with('[') && value.contains(']') {
        if let Some(end) = value.find(']') {
            let host = &value[1..end];
            let port = value[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse().ok());
            return Endpoint {
                address: host.to_string(),
                port,
            };
        }
    }

    if let Some(idx) = value.rfind(':') {
        let (host, port_part) = value.split_at(idx);
        let host = if host.is_empty() { "*" } else { host };
        let port = port_part[1..].parse().ok();
        Endpoint {
            address: host.trim().to_string(),
            port,
        }
    } else {
        Endpoint {
            address: value.to_string(),
            port: None,
        }
    }
}

#[cfg(target_os = "windows")]
fn collect_ports_windows() -> Result<Vec<PortUsage>, Box<dyn std::error::Error>> {
    use std::collections::HashMap;

    let netstat = Command::new("netstat").args(["-a", "-n", "-o"]).output()?;

    if !netstat.status.success() {
        return Err(format!("netstat exited with status {}", netstat.status).into());
    }

    let stdout = String::from_utf8_lossy(&netstat.stdout);
    let mut pid_to_name: HashMap<u32, String> = HashMap::new();

    let tasklist = Command::new("tasklist")
        .args(["/fo", "csv", "/nh"])
        .output()?;
    if tasklist.status.success() {
        let csv = String::from_utf8_lossy(&tasklist.stdout);
        for line in csv.lines() {
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() > 1 {
                let name = fields[0].trim_matches('"').to_string();
                if let Ok(pid) = fields[1].trim_matches('"').parse::<u32>() {
                    pid_to_name.insert(pid, name);
                }
            }
        }
    }

    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for line in stdout.lines().skip(4) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let (protocol_raw, local, foreign, _state, pid_str) = if parts.len() == 5 {
            (parts[0], parts[1], parts[2], parts[3], parts[4])
        } else {
            (parts[0], parts[1], parts[2], "", parts.last().unwrap())
        };

        let protocol = protocol_raw.to_uppercase();
        let pid = pid_str.parse::<u32>().ok();
        let local_ep = parse_windows_endpoint(local);
        let remote_ep = parse_windows_endpoint(foreign);

        let local_address = local_ep.address.clone();
        let local_port = local_ep.port;
        let remote_address_value = remote_ep.address.clone();
        let remote_port = remote_ep.port;
        let remote_address = if remote_address_value.is_empty() || remote_address_value == "*" {
            None
        } else {
            Some(remote_address_value)
        };

        let key = format!(
            "{pid:?}|{protocol}|{local_host}|{local_port:?}|{remote_host:?}|{remote_port:?}",
            pid = pid,
            protocol = protocol,
            local_host = &local_address,
            local_port = local_port,
            remote_host = remote_address.clone().unwrap_or_default(),
            remote_port = remote_port
        );

        if seen.insert(key) {
            results.push(PortUsage {
                protocol,
                local_address,
                local_port,
                remote_address,
                remote_port,
                pid,
                process_name: pid.and_then(|p| pid_to_name.get(&p).cloned()),
                parent_pid: None,
                parent_process_name: None,
                ancestors: Vec::new(),
            });
        }
    }

    attach_process_tree_windows(&mut results)?;
    Ok(results)
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct WindowsEndpoint {
    address: String,
    port: Option<u16>,
}

#[cfg(target_os = "windows")]
fn parse_windows_endpoint(value: &str) -> WindowsEndpoint {
    let trimmed = value.trim();

    if trimmed.starts_with('[') && trimmed.contains(']') {
        if let Some(end) = trimmed.find(']') {
            let host = &trimmed[1..end];
            let port = trimmed[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse().ok());
            return WindowsEndpoint {
                address: host.to_string(),
                port,
            };
        }
    }

    if let Some(idx) = trimmed.rfind(':') {
        let (host, port_part) = trimmed.split_at(idx);
        let port = port_part[1..].parse().ok();
        WindowsEndpoint {
            address: host.to_string(),
            port,
        }
    } else {
        WindowsEndpoint {
            address: trimmed.to_string(),
            port: None,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn attach_process_tree_unix(ports: &mut [PortUsage]) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::{HashMap, HashSet};

    let output = Command::new("ps")
        .args(["-eo", "pid=,ppid=,comm="])
        .output()?;

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut process_map: HashMap<u32, (Option<u32>, String)> = HashMap::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let pid_str = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let ppid_str = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let remainder = parts.collect::<Vec<_>>().join(" ");

        let pid = match pid_str.parse::<u32>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let ppid_raw = match ppid_str.parse::<u32>() {
            Ok(value) => value,
            Err(_) => 0,
        };

        let parent = if ppid_raw == 0 { None } else { Some(ppid_raw) };
        let name = if remainder.is_empty() {
            String::from("(unknown)")
        } else {
            remainder
        };

        process_map.insert(pid, (parent, name));
    }

    for port in ports.iter_mut() {
        let pid = match port.pid {
            Some(value) => value,
            None => continue,
        };

        if let Some((parent_pid, _)) = process_map.get(&pid).cloned() {
            port.parent_pid = parent_pid;
            if let Some(ppid) = parent_pid {
                if let Some((_, parent_name)) = process_map.get(&ppid) {
                    let normalized_name = parent_name.trim();
                    if normalized_name != "launchd" && !normalized_name.ends_with("/launchd") {
                        port.parent_process_name = Some(normalized_name.to_string());
                    }
                }
            }

            let mut lineage = Vec::new();
            let mut current = parent_pid;
            let mut visited = HashSet::new();

            while let Some(current_pid) = current {
                if current_pid <= 1 {
                    break;
                }

                if !visited.insert(current_pid) {
                    break;
                }

                if let Some((next_parent, name)) = process_map.get(&current_pid) {
                    let normalized_name = name.trim();
                    if normalized_name == "launchd" || normalized_name.ends_with("/launchd") {
                        break;
                    }

                    lineage.push(ProcessLink {
                        pid: current_pid,
                        process_name: Some(normalized_name.to_string()),
                    });

                    if let Some(parent_pid) = *next_parent {
                        if parent_pid <= 1 {
                            break;
                        }
                    }

                    current = *next_parent;
                } else {
                    break;
                }
            }

            lineage.reverse();
            port.ancestors = lineage;
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn attach_process_tree_windows(ports: &mut [PortUsage]) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::{HashMap, HashSet};

    let output = Command::new("wmic")
        .args([
            "process",
            "get",
            "ProcessId,ParentProcessId,Name",
            "/FORMAT:CSV",
        ])
        .output();

    let output = match output {
        Ok(val) => val,
        Err(_) => return Ok(()),
    };

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut process_map: HashMap<u32, (Option<u32>, String)> = HashMap::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Node,") {
            continue;
        }

        let parts: Vec<&str> = trimmed.split(',').collect();
        if parts.len() < 4 {
            continue;
        }

        let parent_pid = parts[1].trim().parse::<u32>().ok();
        let pid = match parts[2].trim().parse::<u32>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let parent = parent_pid.and_then(|value| if value == 0 { None } else { Some(value) });
        let name = parts[3].trim();

        process_map.insert(pid, (parent, name.to_string()));
    }

    for port in ports.iter_mut() {
        let pid = match port.pid {
            Some(value) => value,
            None => continue,
        };

        if let Some((parent_pid, _)) = process_map.get(&pid).cloned() {
            port.parent_pid = parent_pid;
            if let Some(ppid) = parent_pid {
                if let Some((_, parent_name)) = process_map.get(&ppid) {
                    if parent_name.trim() != "System" {
                        port.parent_process_name = Some(parent_name.clone());
                    }
                }
            }

            let mut lineage = Vec::new();
            let mut current = parent_pid;
            let mut visited = HashSet::new();

            while let Some(current_pid) = current {
                if current_pid == 0 || current_pid == 4 {
                    break;
                }

                if !visited.insert(current_pid) {
                    break;
                }

                if let Some((next_parent, name)) = process_map.get(&current_pid) {
                    lineage.push(ProcessLink {
                        pid: current_pid,
                        process_name: Some(name.clone()),
                    });

                    if let Some(parent_pid) = *next_parent {
                        if parent_pid == 0 || parent_pid == 4 {
                            break;
                        }
                    }

                    current = *next_parent;
                } else {
                    break;
                }
            }

            lineage.reverse();
            port.ancestors = lineage;
        }
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data_dir)?;
            let db_path = app_data_dir.join("app.db");
            initialize_database(&db_path)?;

            app.manage(AppState {
                db_path: db_path.clone(),
            });
            // Notion: use SQLite-backed store and HTTP adapter when enabled.
            #[cfg(feature = "notion-sqlite")]
            {
                let handle = app.handle().clone();
                app.manage(notion::commands::create_state_with_sqlite(
                    handle,
                    db_path.clone(),
                ));
            }
            #[cfg(not(feature = "notion-sqlite"))]
            {
                let handle = app.handle().clone();
                app.manage(notion::commands::create_default_state_with_handle(handle));
            }
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_ports,
            kill_port_process,
            list_port_favorites,
            update_port_favorite,
            analyze_manga_directory,
            prepare_doublepage_split,
            preview_edge_texture_trim,
            load_manual_split_context,
            render_manual_split_preview,
            prepare_manual_split_workspace,
            apply_manual_splits,
            revert_manual_splits,
            export_manual_split_template,
            track_manual_split_event,
            list_edge_preview_candidates,
            rename_manga_sequence,
            upload_copyparty,
            create_manga_job,
            fetch_manga_job_status,
            watch_manga_job,
            resume_manga_job,
            cancel_manga_job,
            download_manga_artifact,
            validate_manga_artifact,
            read_template_file,
            // Notion Import M1 (skeleton)
            notion::commands::notion_start_oauth_session,
            notion::commands::notion_exchange_oauth_code,
            notion::commands::notion_save_token,
            notion::commands::notion_list_tokens,
            notion::commands::notion_get_token_secret,
            notion::commands::notion_get_oauth_settings,
            notion::commands::notion_delete_token,
            notion::commands::notion_refresh_oauth_token,
            notion::commands::notion_update_oauth_settings,
            notion::commands::notion_test_connection,
            notion::commands::notion_search_databases,
            notion::commands::notion_search_databases_page,
            // Notion Import M2
            notion::commands::notion_get_database,
            notion::commands::notion_template_save,
            notion::commands::notion_template_list,
            notion::commands::notion_template_delete,
            notion::commands::notion_import_preview_file,
            notion::commands::notion_import_dry_run,
            notion::commands::notion_transform_eval_sample,
            // Notion Import M3 skeleton
            notion::commands::notion_import_start,
            notion::commands::notion_import_pause,
            notion::commands::notion_import_resume,
            notion::commands::notion_import_cancel,
            notion::commands::notion_import_get_job,
            notion::commands::notion_import_list_jobs,
            notion::commands::notion_import_export_failed
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn initialize_database(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(path)?;
    // port_favorites (existing)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS port_favorites (
            protocol TEXT NOT NULL,
            local_address TEXT NOT NULL,
            local_port INTEGER,
            PRIMARY KEY (protocol, local_address, local_port)
        )",
        [],
    )?;
    // notion_* tables (M1)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notion_tokens (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'manual',
            token_cipher BLOB NOT NULL,
            workspace_name TEXT NULL,
            workspace_icon TEXT NULL,
            workspace_id TEXT NULL,
            created_at INTEGER NOT NULL,
            last_used_at INTEGER NULL,
            expires_at INTEGER NULL,
            refresh_token TEXT NULL,
            last_refresh_error TEXT NULL,
            encryption_salt BLOB NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notion_import_templates (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            token_id TEXT NOT NULL,
            database_id TEXT NOT NULL,
            mapping_json TEXT NOT NULL,
            defaults_json TEXT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notion_import_jobs (
            id TEXT PRIMARY KEY,
            token_id TEXT NOT NULL,
            database_id TEXT NOT NULL,
            source_file_path TEXT NOT NULL,
            status TEXT NOT NULL,
            total INTEGER NULL,
            done INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0,
            skipped INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NULL,
            ended_at INTEGER NULL,
            config_snapshot_json TEXT NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notion_import_job_rows (
            job_id TEXT NOT NULL,
            row_index INTEGER NOT NULL,
            status TEXT NOT NULL,
            error_code TEXT NULL,
            error_message TEXT NULL,
            PRIMARY KEY (job_id, row_index)
        )",
        [],
    )?;
    #[cfg(feature = "notion-sqlite")]
    if let Err(err) = SqliteTokenStore::ensure_schema(path) {
        eprintln!("[notion] failed to ensure notion_tokens schema: {}", err);
    }
    Ok(())
}

fn with_connection<T, F>(path: &Path, action: F) -> rusqlite::Result<T>
where
    F: FnOnce(&Connection) -> rusqlite::Result<T>,
{
    let conn = Connection::open(path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS port_favorites (
            protocol TEXT NOT NULL,
            local_address TEXT NOT NULL,
            local_port INTEGER,
            PRIMARY KEY (protocol, local_address, local_port)
        )",
        [],
    )?;
    action(&conn)
}
