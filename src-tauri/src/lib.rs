use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::Manager;

use rusqlite::{params, Connection};

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
fn collect_ports_unix() -> Result<Vec<PortUsage>, Box<dyn std::error::Error>> {
    let output = Command::new("lsof")
        .args(["-nP", "-i", "-FpctunP"])
        .output()?;

    if !output.status.success() {
        return Err(format!("lsof exited with status {}", output.status).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_lsof_output(&stdout)
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
            });
        }
    }

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data_dir)?;
            let db_path = app_data_dir.join("favorites.db");
            initialize_database(&db_path)?;

            app.manage(AppState { db_path });
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_ports,
            list_port_favorites,
            update_port_favorite
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn initialize_database(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
