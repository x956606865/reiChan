use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, State};

#[cfg(feature = "notion-http")]
use super::adapter::HttpNotionAdapter;
use super::adapter::{MockNotionAdapter, NotionAdapter};
use super::job_runner::{
    JobEventEmitter, JobLogEvent, JobLogLevel, JobRunner, JobSnapshot, JobState,
};
use super::mapping::build_property_entry;
use super::preview::{preview_file as notion_preview_file, PreviewRequest, PreviewResponse};
use super::scheduler::{Scheduler, SchedulerConfig, SchedulerDeps};
use super::storage::{
    ImportJobRecord, ImportJobStore, InMemoryJobStore, InMemoryTokenStore, NewImportJob,
    StateTransition, TokenStore,
};
#[cfg(feature = "notion-sqlite")]
use super::storage::{SqliteJobStore, SqliteTokenStore};
use super::transform::{TransformContext, TransformExecutor};
use super::types::{
    ConflictType, DatabaseBrief, DatabasePage, DatabaseProperty, DatabaseSchema, DryRunErrorKind,
    DryRunInput, DryRunReport, ExportFailedResult, FieldMapping, ImportDoneEvent, ImportJobHandle,
    ImportJobRequest, ImportJobSummary, ImportLogEvent, ImportLogLevel, ImportProgressEvent,
    ImportQueueSnapshot, ImportTemplate, RowError, RowErrorSummary, SaveTokenRequest, TokenRow,
    TransformEvalRequest, TransformEvalResult, WorkspaceInfo,
};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

fn apply_filter_empty_title(list: &mut Vec<DatabaseBrief>, include_empty_title: bool) {
    if !include_empty_title {
        list.retain(|d| !d.title.trim().is_empty());
    }
}

struct TauriJobEventEmitter {
    app: AppHandle,
    job_store: Arc<dyn ImportJobStore>,
}

impl TauriJobEventEmitter {
    fn new(app: AppHandle, job_store: Arc<dyn ImportJobStore>) -> Self {
        Self { app, job_store }
    }
}

impl JobEventEmitter for TauriJobEventEmitter {
    fn on_snapshot(&self, job_id: &str, snapshot: &JobSnapshot) {
        let record = self.job_store.load_job(job_id).ok().flatten();
        let rps = record.as_ref().and_then(|rec| rec.rps);
        let priority = record.as_ref().map(|rec| rec.priority);
        let lease_expires_at = record.as_ref().and_then(|rec| rec.lease_expires_at);
        let errors = self
            .job_store
            .list_recent_failures(job_id, 5)
            .unwrap_or_default()
            .into_iter()
            .map(|row| RowErrorSummary {
                row_index: row.row_index,
                error_code: row.error_code,
                error_message: row.error_message.unwrap_or_default(),
                conflict_type: row.conflict_type.as_deref().and_then(|kind| match kind {
                    "skip" => Some(ConflictType::Skip),
                    "overwrite" => Some(ConflictType::Overwrite),
                    "merge" => Some(ConflictType::Merge),
                    _ => Some(ConflictType::Unknown),
                }),
            })
            .collect::<Vec<_>>();
        let event = ImportProgressEvent {
            job_id: job_id.to_string(),
            state: snapshot.state.clone(),
            progress: snapshot.progress.clone(),
            rps,
            recent_errors: errors,
            priority,
            lease_expires_at,
            timestamp: Utc::now().timestamp_millis(),
        };
        let _ = self.app.emit("notion-import/progress", event);
    }

    fn on_log(&self, job_id: &str, event: &JobLogEvent) {
        let mapped = ImportLogEvent {
            job_id: job_id.to_string(),
            level: match event.level {
                JobLogLevel::Info => ImportLogLevel::Info,
                JobLogLevel::Warn => ImportLogLevel::Warn,
                JobLogLevel::Error => ImportLogLevel::Error,
            },
            message: event.message.clone(),
            timestamp: event.timestamp,
        };
        let _ = self.app.emit("notion-import/log", mapped);
    }

    fn on_done(&self, job_id: &str, snapshot: &JobSnapshot) {
        let record = self.job_store.load_job(job_id).ok().flatten();
        let (rps, last_error, finished_at) = if let Some(ref rec) = record {
            (
                rec.rps,
                rec.last_error.clone(),
                rec.ended_at
                    .unwrap_or_else(|| Utc::now().timestamp_millis()),
            )
        } else {
            (None, None, Utc::now().timestamp_millis())
        };
        let event = ImportDoneEvent {
            job_id: job_id.to_string(),
            state: snapshot.state.clone(),
            progress: snapshot.progress.clone(),
            rps,
            finished_at,
            last_error,
            priority: record.as_ref().map(|rec| rec.priority),
            conflict_total: snapshot.progress.conflict_total,
        };
        let _ = self.app.emit("notion-import/done", event);
    }
}

pub struct NotionState {
    pub store: Arc<dyn TokenStore>,
    pub adapter: Arc<dyn NotionAdapter>,
    pub db_path: Option<std::path::PathBuf>,
    // Fallback in-memory template store when SQLite path is unavailable (e.g., in tests).
    pub templates_mem: Arc<Mutex<Vec<ImportTemplate>>>,
    pub job_runner: Arc<JobRunner>,
    pub job_store: Arc<dyn ImportJobStore>,
    pub scheduler: Arc<Scheduler>,
}

impl NotionState {
    pub fn new(
        store: Arc<dyn TokenStore>,
        adapter: Arc<dyn NotionAdapter>,
        job_store: Arc<dyn ImportJobStore>,
        job_runner: Arc<JobRunner>,
    ) -> Self {
        let scheduler = Arc::new(Scheduler::spawn(
            SchedulerConfig::default(),
            SchedulerDeps {
                token_store: Arc::clone(&store),
                job_store: Arc::clone(&job_store),
                job_runner: Arc::clone(&job_runner),
                adapter: Arc::clone(&adapter),
            },
        ));
        Self {
            store,
            adapter,
            db_path: None,
            templates_mem: Arc::new(Mutex::new(Vec::new())),
            job_runner,
            job_store,
            scheduler,
        }
    }

    pub fn resume_pending_jobs(&self) {
        let records = match self.job_store.list_pending_jobs() {
            Ok(list) => list,
            Err(_) => return,
        };
        for record in records {
            if self.job_runner.snapshot(&record.id).is_none() {
                self.job_runner.register_job(record.id.clone());
                self.job_runner
                    .update_progress(&record.id, record.progress.clone());
            }
            self.job_runner.set_state(&record.id, record.state.clone());

            if matches!(
                record.state,
                JobState::Pending | JobState::Queued | JobState::Running
            ) {
                let _ = self.job_store.touch_lease(&record.id, None);
                let _ = self.scheduler.enqueue(record.id.clone());
            }
        }
    }
}

pub fn create_default_state() -> NotionState {
    let state = NotionState::new(
        Arc::new(InMemoryTokenStore::new()),
        Arc::new(MockNotionAdapter::new()),
        Arc::new(InMemoryJobStore::new()),
        Arc::new(JobRunner::new()),
    );
    state.resume_pending_jobs();
    state
}

pub fn create_default_state_with_handle(app: AppHandle) -> NotionState {
    let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
    let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter::new());
    let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
    let emitter: Arc<dyn JobEventEmitter> =
        Arc::new(TauriJobEventEmitter::new(app, job_store.clone()));
    let job_runner = Arc::new(JobRunner::with_emitter(emitter));
    let state = NotionState::new(store, adapter, job_store, job_runner);
    state.resume_pending_jobs();
    state
}

#[cfg(feature = "notion-sqlite")]
pub fn create_state_with_sqlite(app: AppHandle, db_path: PathBuf) -> NotionState {
    let store: Arc<dyn TokenStore> = Arc::new(SqliteTokenStore::new(db_path.clone()));
    #[cfg(feature = "notion-http")]
    let adapter: Arc<dyn NotionAdapter> = Arc::new(HttpNotionAdapter);
    #[cfg(not(feature = "notion-http"))]
    let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter::new());
    #[cfg(feature = "notion-sqlite")]
    let job_store: Arc<dyn ImportJobStore> = Arc::new(SqliteJobStore::new(db_path.clone()));
    #[cfg(not(feature = "notion-sqlite"))]
    let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
    let emitter: Arc<dyn JobEventEmitter> =
        Arc::new(TauriJobEventEmitter::new(app, job_store.clone()));
    let job_runner = Arc::new(JobRunner::with_emitter(emitter));
    let mut state = NotionState::new(store, adapter, job_store, job_runner);
    state.db_path = Some(db_path);
    state.resume_pending_jobs();
    state
}

#[tauri::command]
pub async fn notion_save_token(
    state: State<'_, NotionState>,
    req: SaveTokenRequest,
) -> Result<TokenRow, String> {
    // Validate minimal input and avoid logging secrets
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err("Token name is required".into());
    }
    let token = req.token.trim().to_string();
    if token.is_empty() {
        return Err("Token string is required".into());
    }

    let adapter = state.adapter.clone();
    let store = state.store.clone();

    // Offload blocking network + storage work to a blocking thread to avoid UI jank.
    tauri::async_runtime::spawn_blocking(move || {
        let ws = adapter.test_connection(&token).unwrap_or(WorkspaceInfo {
            workspace_name: None,
            bot_name: None,
        });
        let row = store.save(&name, &token, ws.workspace_name.clone());
        Ok::<TokenRow, String>(row)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|res| res)
}

#[tauri::command]
pub fn notion_list_tokens(state: State<NotionState>) -> Result<Vec<TokenRow>, String> {
    Ok(state.store.list())
}

#[tauri::command]
pub fn notion_delete_token(state: State<NotionState>, id: String) -> Result<(), String> {
    if state.store.delete(&id) {
        Ok(())
    } else {
        Err("Not found".into())
    }
}

#[tauri::command]
pub async fn notion_test_connection(
    state: State<'_, NotionState>,
    token_id: String,
) -> Result<WorkspaceInfo, String> {
    let token = state
        .store
        .get_token(&token_id)
        .ok_or_else(|| "Token not found".to_string())?;
    let adapter = state.adapter.clone();
    tauri::async_runtime::spawn_blocking(move || adapter.test_connection(&token))
        .await
        .map_err(|e| e.to_string())
        .and_then(|res| res)
}

#[tauri::command]
pub async fn notion_search_databases(
    state: State<'_, NotionState>,
    token_id: String,
    query: Option<String>,
    include_empty_title: Option<bool>,
) -> Result<Vec<DatabaseBrief>, String> {
    let token = state
        .store
        .get_token(&token_id)
        .ok_or_else(|| "Token not found".to_string())?;
    let adapter = state.adapter.clone();
    let include_empty = include_empty_title.unwrap_or(false);
    let res = tauri::async_runtime::spawn_blocking(move || adapter.search_databases(&token, query))
        .await
        .map_err(|e| e.to_string())?;
    let mut res = res?;
    apply_filter_empty_title(&mut res, include_empty);
    Ok(res)
}

#[tauri::command]
pub async fn notion_search_databases_page(
    state: State<'_, NotionState>,
    token_id: String,
    query: Option<String>,
    cursor: Option<String>,
    page_size: Option<u32>,
    include_empty_title: Option<bool>,
) -> Result<DatabasePage, String> {
    let token = state
        .store
        .get_token(&token_id)
        .ok_or_else(|| "Token not found".to_string())?;
    let adapter = state.adapter.clone();
    let include_empty = include_empty_title.unwrap_or(false);
    let page = tauri::async_runtime::spawn_blocking(move || {
        adapter.search_databases_page(&token, query, cursor, page_size)
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut page = page?;
    apply_filter_empty_title(&mut page.results, include_empty);
    Ok(page)
}

// -----------------------------
// M2: Database schema & Templates
// -----------------------------

#[tauri::command]
pub async fn notion_get_database(
    state: State<'_, NotionState>,
    token_id: String,
    database_id: String,
) -> Result<DatabaseSchema, String> {
    let token = state
        .store
        .get_token(&token_id)
        .ok_or_else(|| "Token not found".to_string())?;
    let adapter = state.adapter.clone();
    tauri::async_runtime::spawn_blocking(move || adapter.get_database_schema(&token, &database_id))
        .await
        .map_err(|e| e.to_string())
        .and_then(|res| res)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MappingJsonPayload {
    version: u32,
    mappings: Vec<super::types::FieldMapping>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn snapshot_to_summary(job_id: String, snapshot: JobSnapshot) -> ImportJobSummary {
    ImportJobSummary {
        job_id,
        state: snapshot.state,
        progress: snapshot.progress,
        priority: None,
        lease_expires_at: None,
        token_id: None,
        database_id: None,
        created_at: None,
        started_at: None,
        ended_at: None,
        last_error: None,
        rps: None,
    }
}

fn record_to_summary(record: ImportJobRecord) -> ImportJobSummary {
    ImportJobSummary {
        job_id: record.id,
        state: record.state,
        progress: record.progress,
        priority: Some(record.priority),
        lease_expires_at: record.lease_expires_at,
        token_id: Some(record.token_id),
        database_id: Some(record.database_id),
        created_at: Some(record.created_at),
        started_at: record.started_at,
        ended_at: record.ended_at,
        last_error: record.last_error,
        rps: record.rps,
    }
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportHistoryRequest {
    page: Option<usize>,
    page_size: Option<usize>,
    states: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportHistoryPage {
    items: Vec<ImportJobSummary>,
    total: usize,
    page: usize,
    page_size: usize,
    has_more: bool,
}

fn parse_job_state_label(value: &str) -> Option<JobState> {
    match value {
        "Pending" => Some(JobState::Pending),
        "Queued" => Some(JobState::Queued),
        "Running" => Some(JobState::Running),
        "Paused" => Some(JobState::Paused),
        "Completed" => Some(JobState::Completed),
        "Failed" => Some(JobState::Failed),
        "Canceled" => Some(JobState::Canceled),
        _ => None,
    }
}

fn sort_by_priority(entries: &mut Vec<ImportJobSummary>) {
    entries.sort_by(|a, b| {
        let pa = a.priority.unwrap_or(0);
        let pb = b.priority.unwrap_or(0);
        pb.cmp(&pa)
            .then_with(|| {
                let ca = a.created_at.unwrap_or(i64::MAX);
                let cb = b.created_at.unwrap_or(i64::MAX);
                ca.cmp(&cb)
            })
            .then_with(|| a.job_id.cmp(&b.job_id))
    });
}

fn sort_running(entries: &mut Vec<ImportJobSummary>) {
    entries.sort_by(|a, b| {
        let pa = a.priority.unwrap_or(0);
        let pb = b.priority.unwrap_or(0);
        pb.cmp(&pa)
            .then_with(|| {
                let sa = a.started_at.unwrap_or(i64::MAX);
                let sb = b.started_at.unwrap_or(i64::MAX);
                sa.cmp(&sb)
            })
            .then_with(|| a.job_id.cmp(&b.job_id))
    });
}

#[tauri::command]
pub fn notion_template_save(
    state: State<NotionState>,
    tpl: ImportTemplate,
) -> Result<ImportTemplate, String> {
    // Basic validations
    if tpl.name.trim().is_empty() {
        return Err("Template name is required".into());
    }
    if tpl.token_id.trim().is_empty() {
        return Err("tokenId is required".into());
    }
    if tpl.database_id.trim().is_empty() {
        return Err("databaseId is required".into());
    }
    let mapping_payload = MappingJsonPayload {
        version: 1,
        mappings: tpl.mappings.clone(),
    };
    let mapping_json = serde_json::to_string(&mapping_payload).map_err(|e| e.to_string())?;
    let defaults_json: Option<String> = tpl.defaults.as_ref().map(|v| v.to_string());

    if let Some(path) = &state.db_path {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let now = now_ms();
        match tpl.id {
            Some(id) => {
                let affected = conn.execute(
                    "UPDATE notion_import_templates SET name=?2, token_id=?3, database_id=?4, mapping_json=?5, defaults_json=?6, updated_at=?7 WHERE id=?1",
                    (id.as_str(), tpl.name.as_str(), tpl.token_id.as_str(), tpl.database_id.as_str(), mapping_json.as_str(), defaults_json.as_deref(), now),
                ).map_err(|e| e.to_string())?;
                if affected == 0 {
                    return Err("Template not found".into());
                }
                Ok(ImportTemplate {
                    id: Some(id),
                    ..tpl
                })
            }
            None => {
                let mut stmt = conn.prepare(
                    "INSERT INTO notion_import_templates (id, name, token_id, database_id, mapping_json, defaults_json, created_at, updated_at)
                     VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id"
                ).map_err(|e| e.to_string())?;
                let new_id: String = stmt
                    .query_row(
                        (
                            tpl.name.as_str(),
                            tpl.token_id.as_str(),
                            tpl.database_id.as_str(),
                            mapping_json.as_str(),
                            defaults_json.as_deref(),
                            now,
                            now,
                        ),
                        |row| row.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(ImportTemplate {
                    id: Some(new_id),
                    ..tpl
                })
            }
        }
    } else {
        // In-memory fallback
        let mut guard = state
            .templates_mem
            .lock()
            .map_err(|_| "poisoned".to_string())?;
        let mut tpl = tpl;
        if tpl.id.is_none() {
            tpl.id = Some(format!("tpl-{}", now_ms()));
        }
        if let Some(pos) = guard.iter().position(|x| x.id == tpl.id) {
            guard[pos] = tpl.clone();
        } else {
            guard.push(tpl.clone());
        }
        Ok(tpl)
    }
}

#[tauri::command]
pub fn notion_template_list(
    state: State<NotionState>,
    token_id: Option<String>,
) -> Result<Vec<ImportTemplate>, String> {
    if let Some(path) = &state.db_path {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let mut sql = String::from("SELECT id, name, token_id, database_id, mapping_json, defaults_json FROM notion_import_templates");
        let mut args: Vec<String> = Vec::new();
        if let Some(tok) = token_id.as_ref() {
            sql.push_str(" WHERE token_id = ?1");
            args.push(tok.clone());
        }
        sql.push_str(" ORDER BY updated_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = if args.is_empty() {
            stmt.query([]).map_err(|e| e.to_string())?
        } else {
            stmt.query([&args[0]]).map_err(|e| e.to_string())?
        };
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let id: String = row.get(0).map_err(|e| e.to_string())?;
            let name: String = row.get(1).map_err(|e| e.to_string())?;
            let token_id: String = row.get(2).map_err(|e| e.to_string())?;
            let database_id: String = row.get(3).map_err(|e| e.to_string())?;
            let mapping_json: String = row.get(4).map_err(|e| e.to_string())?;
            let defaults_json_opt: Option<String> = row.get(5).map_err(|e| e.to_string())?;
            let payload: MappingJsonPayload =
                serde_json::from_str(&mapping_json).map_err(|e| e.to_string())?;
            let defaults =
                defaults_json_opt.and_then(|s| serde_json::from_str::<Value>(s.as_str()).ok());
            out.push(ImportTemplate {
                id: Some(id),
                name,
                token_id,
                database_id,
                mappings: payload.mappings,
                defaults,
            });
        }
        Ok(out)
    } else {
        let guard = state
            .templates_mem
            .lock()
            .map_err(|_| "poisoned".to_string())?;
        Ok(guard.clone())
    }
}

#[tauri::command]
pub fn notion_template_delete(state: State<NotionState>, id: String) -> Result<(), String> {
    if let Some(path) = &state.db_path {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let affected = conn
            .execute("DELETE FROM notion_import_templates WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        if affected == 0 {
            return Err("Template not found".into());
        }
        Ok(())
    } else {
        let mut guard = state
            .templates_mem
            .lock()
            .map_err(|_| "poisoned".to_string())?;
        let before = guard.len();
        guard.retain(|t| t.id.as_deref() != Some(id.as_str()));
        if guard.len() == before {
            return Err("Template not found".into());
        }
        Ok(())
    }
}

// -----------------------------
// M2: Dry-run (占位实现：构造 properties + 基础类型检查；不执行 JS transform)
// -----------------------------
#[tauri::command]
pub fn notion_import_dry_run(input: DryRunInput) -> Result<DryRunReport, String> {
    if input.records.is_empty() {
        return Err("Dry-run requires at least one sample record".into());
    }

    let DryRunInput {
        schema,
        mappings,
        records,
        defaults,
    } = input;

    let defaults_obj: Map<String, Value> = match defaults {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        other => {
            return Err(format!(
                "defaults must be an object, got {}",
                json_type(&other)
            ))
        }
    };

    let mut property_map: HashMap<String, super::types::DatabaseProperty> = HashMap::new();
    for prop in &schema.properties {
        property_map.insert(prop.name.clone(), prop.clone());
    }

    let title_props: HashSet<String> = schema
        .properties
        .iter()
        .filter(|p| p.type_ == "title")
        .map(|p| p.name.clone())
        .collect();
    let has_title_prop = !title_props.is_empty();

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<RowError> = Vec::new();
    let mut executor: Option<TransformExecutor> = None;

    for (idx, rec) in records.iter().enumerate() {
        let obj = match rec.as_object() {
            Some(map) => map.clone(),
            None => {
                failed += 1;
                errors.push(RowError {
                    row_index: idx,
                    message: "record is not an object".into(),
                    kind: DryRunErrorKind::Validation,
                });
                continue;
            }
        };

        let mut props = Map::new();
        let mut record_failed = false;

        for mapping in mappings.iter().filter(|m| m.include) {
            let target_name = mapping.target_property.trim();
            if target_name.is_empty() {
                failed += 1;
                errors.push(RowError {
                    row_index: idx,
                    message: "targetProperty is required".into(),
                    kind: DryRunErrorKind::Validation,
                });
                record_failed = true;
                break;
            }

            let property = match property_map.get(target_name) {
                Some(prop) => prop,
                None => {
                    failed += 1;
                    errors.push(RowError {
                        row_index: idx,
                        message: format!("property '{}' not found in schema", target_name),
                        kind: DryRunErrorKind::Validation,
                    });
                    record_failed = true;
                    break;
                }
            };

            let source_field = mapping.source_field.trim();
            let src_val = if source_field.is_empty() {
                Value::Null
            } else {
                obj.get(source_field).cloned().unwrap_or(Value::Null)
            };

            let effective_val = if let Some(code) = mapping
                .transform_code
                .as_ref()
                .filter(|c| !c.trim().is_empty())
            {
                if executor.is_none() {
                    executor = Some(TransformExecutor::new().map_err(|err| err.to_string())?);
                }
                let ctx = TransformContext {
                    row_index: idx,
                    record: obj.clone(),
                };
                match executor
                    .as_ref()
                    .unwrap()
                    .execute(code, src_val.clone(), ctx)
                {
                    Ok(val) => val,
                    Err(err) => {
                        failed += 1;
                        errors.push(RowError {
                            row_index: idx,
                            message: format!("transform error ({}): {}", mapping.source_field, err),
                            kind: DryRunErrorKind::Transform,
                        });
                        record_failed = true;
                        break;
                    }
                }
            } else {
                src_val
            };

            if let Err(msg) = validate_schema_constraints(property, &effective_val) {
                failed += 1;
                errors.push(RowError {
                    row_index: idx,
                    message: format!(
                        "validation error ({} -> {}): {}",
                        mapping.source_field, mapping.target_property, msg
                    ),
                    kind: DryRunErrorKind::Validation,
                });
                record_failed = true;
                break;
            }

            match build_property_entry(mapping, &effective_val) {
                Ok(entry) => {
                    if let Err(msg) = validate_option_values(property, &entry) {
                        failed += 1;
                        errors.push(RowError {
                            row_index: idx,
                            message: format!(
                                "validation error ({} -> {}): {}",
                                mapping.source_field, mapping.target_property, msg
                            ),
                            kind: DryRunErrorKind::Validation,
                        });
                        record_failed = true;
                        break;
                    }
                    props.insert(mapping.target_property.clone(), entry);
                }
                Err(err) => {
                    failed += 1;
                    errors.push(RowError {
                        row_index: idx,
                        message: format!(
                            "mapping error ({} -> {}): {}",
                            mapping.source_field, mapping.target_property, err
                        ),
                        kind: DryRunErrorKind::Mapping,
                    });
                    record_failed = true;
                    break;
                }
            }
        }

        if record_failed {
            continue;
        }

        if !defaults_obj.is_empty() {
            for (prop_name, default_value) in defaults_obj.iter() {
                let property = match property_map.get(prop_name) {
                    Some(prop) => prop,
                    None => {
                        failed += 1;
                        errors.push(RowError {
                            row_index: idx,
                            message: format!(
                                "defaults references unknown property '{}'",
                                prop_name
                            ),
                            kind: DryRunErrorKind::Validation,
                        });
                        record_failed = true;
                        break;
                    }
                };

                let existing_filled = props
                    .get(prop_name)
                    .map(|val| property_value_has_content(property, val))
                    .unwrap_or(false);

                if existing_filled {
                    continue;
                }

                if let Err(msg) = validate_schema_constraints(property, default_value) {
                    failed += 1;
                    errors.push(RowError {
                        row_index: idx,
                        message: format!("defaults for '{}' invalid: {}", prop_name, msg),
                        kind: DryRunErrorKind::Validation,
                    });
                    record_failed = true;
                    break;
                }

                let stub = FieldMapping {
                    include: true,
                    source_field: prop_name.clone(),
                    target_property: prop_name.clone(),
                    target_type: property.type_.clone(),
                    transform_code: None,
                };

                match build_property_entry(&stub, default_value) {
                    Ok(entry) => {
                        if let Err(msg) = validate_option_values(property, &entry) {
                            failed += 1;
                            errors.push(RowError {
                                row_index: idx,
                                message: format!("defaults for '{}' invalid: {}", prop_name, msg),
                                kind: DryRunErrorKind::Validation,
                            });
                            record_failed = true;
                            break;
                        }
                        props.insert(prop_name.clone(), entry);
                    }
                    Err(err) => {
                        failed += 1;
                        errors.push(RowError {
                            row_index: idx,
                            message: format!("defaults for '{}' mapping error: {}", prop_name, err),
                            kind: DryRunErrorKind::Mapping,
                        });
                        record_failed = true;
                        break;
                    }
                }
            }
        }

        if record_failed {
            continue;
        }

        if has_title_prop
            && !props
                .iter()
                .any(|(name, value)| title_props.contains(name) && title_entry_has_content(value))
        {
            failed += 1;
            errors.push(RowError {
                row_index: idx,
                message: "Missing title property mapping".into(),
                kind: DryRunErrorKind::Validation,
            });
            continue;
        }

        // Validate required properties flagged in schema
        let mut missing_required = Vec::new();
        for prop in schema
            .properties
            .iter()
            .filter(|p| p.required.unwrap_or(false))
        {
            if !props
                .get(&prop.name)
                .map(|value| property_value_has_content(prop, value))
                .unwrap_or(false)
            {
                missing_required.push(prop.name.clone());
            }
        }

        if !missing_required.is_empty() {
            failed += 1;
            errors.push(RowError {
                row_index: idx,
                message: format!(
                    "Missing required properties: {}",
                    missing_required.join(", ")
                ),
                kind: DryRunErrorKind::Validation,
            });
            continue;
        }

        ok += 1;
    }

    Ok(DryRunReport {
        total: records.len(),
        ok,
        failed,
        errors,
    })
}

// -----------------------------
// M2: Preview & Transform helpers
// -----------------------------

#[tauri::command]
pub fn notion_import_preview_file(req: PreviewRequest) -> Result<PreviewResponse, String> {
    notion_preview_file(&req)
}

#[tauri::command]
pub fn notion_transform_eval_sample(
    req: TransformEvalRequest,
) -> Result<TransformEvalResult, String> {
    let record_map = req
        .record
        .as_object()
        .cloned()
        .ok_or_else(|| "record must be an object".to_string())?;
    let executor = TransformExecutor::new().map_err(|err| err.to_string())?;
    let ctx = TransformContext {
        row_index: req.row_index,
        record: record_map,
    };
    let result = executor
        .execute(&req.code, req.value, ctx)
        .map_err(|err| err.to_string())?;
    Ok(TransformEvalResult { result })
}

fn next_job_id() -> String {
    format!("notion-import-{}", now_ms())
}

#[tauri::command]
pub fn notion_import_start(
    state: State<NotionState>,
    req: ImportJobRequest,
) -> Result<ImportJobHandle, String> {
    handle_import_start(&state, req)
}

#[tauri::command]
pub fn notion_import_pause(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_pause(&state, job_id)
}

#[tauri::command]
pub fn notion_import_resume(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_resume(&state, job_id)
}

#[tauri::command]
pub fn notion_import_cancel(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_cancel(&state, job_id)
}

#[tauri::command]
pub fn notion_import_get_job(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_get_job(&state, job_id)
}

#[tauri::command]
pub fn notion_import_list_jobs(state: State<NotionState>) -> Result<Vec<ImportJobSummary>, String> {
    handle_import_list_jobs(&state)
}

#[tauri::command]
pub fn notion_import_history(
    state: State<NotionState>,
    req: Option<ImportHistoryRequest>,
) -> Result<ImportHistoryPage, String> {
    handle_import_history(&state, req.unwrap_or_default())
}

#[tauri::command]
pub fn notion_import_queue(state: State<NotionState>) -> Result<ImportQueueSnapshot, String> {
    handle_import_queue_snapshot(&state)
}

#[tauri::command]
pub fn notion_import_promote(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_promote(&state, job_id)
}

#[tauri::command]
pub fn notion_import_requeue(
    state: State<NotionState>,
    job_id: String,
) -> Result<ImportJobSummary, String> {
    handle_import_requeue(&state, job_id)
}

#[tauri::command]
pub fn notion_import_set_priority(
    state: State<NotionState>,
    job_id: String,
    priority: i32,
) -> Result<ImportJobSummary, String> {
    handle_import_set_priority(&state, job_id, priority)
}

#[tauri::command]
pub fn notion_import_export_failed(
    state: State<NotionState>,
    job_id: String,
) -> Result<ExportFailedResult, String> {
    handle_export_failed(&state, job_id)
}

fn handle_import_start(
    state: &NotionState,
    req: ImportJobRequest,
) -> Result<ImportJobHandle, String> {
    let ImportJobRequest {
        job_id,
        token_id,
        database_id,
        source_file_path,
        file_type,
        mappings,
        defaults,
        rate_limit,
        batch_size,
        priority,
        upsert,
    } = req;

    if state.store.get_token(&token_id).is_none() {
        return Err("Token not found".to_string());
    }

    let job_id = job_id
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(next_job_id);

    let created_at = now_ms();
    let priority_value = priority.unwrap_or(0);
    let snapshot_value = serde_json::json!({
        "version": 1,
        "tokenId": token_id.clone(),
        "databaseId": database_id.clone(),
        "sourceFilePath": source_file_path.clone(),
        "fileType": file_type,
        "mappings": mappings,
        "defaults": defaults,
        "rateLimit": rate_limit,
        "batchSize": batch_size,
        "priority": priority_value,
        "upsert": upsert,
    });
    let config_snapshot_json = serde_json::to_string(&snapshot_value).map_err(|e| e.to_string())?;

    let new_job = NewImportJob {
        id: job_id.clone(),
        token_id: token_id,
        database_id: database_id,
        source_file_path: source_file_path,
        config_snapshot_json,
        total: None,
        created_at,
        priority: priority_value,
        lease_expires_at: None,
        conflict_total: Some(0),
    };

    let record = state.job_store.insert_job(new_job)?;

    if state.job_runner.snapshot(&job_id).is_none() {
        state.job_runner.register_job(job_id.clone());
        state
            .job_runner
            .update_progress(&job_id, record.progress.clone());
    }
    state.job_runner.set_state(&job_id, JobState::Queued);

    state.job_store.mark_state(
        &job_id,
        StateTransition {
            state: JobState::Queued,
            ..StateTransition::default()
        },
    )?;
    state.job_store.touch_lease(&job_id, None)?;

    state.scheduler.enqueue(job_id.clone())?;

    Ok(ImportJobHandle {
        job_id: job_id.clone(),
        state: JobState::Queued,
    })
}

fn handle_import_pause(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    state.job_runner.pause(&job_id);
    state.job_store.mark_state(
        &job_id,
        StateTransition {
            state: JobState::Paused,
            ..StateTransition::default()
        },
    )?;
    let record = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    Ok(record_to_summary(record))
}

fn handle_import_resume(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    state.job_runner.resume(&job_id);
    state.job_store.mark_state(
        &job_id,
        StateTransition {
            state: JobState::Running,
            ..StateTransition::default()
        },
    )?;
    let record = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    Ok(record_to_summary(record))
}

fn handle_import_cancel(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    state.job_runner.cancel(&job_id);
    state.job_store.mark_state(
        &job_id,
        StateTransition {
            state: JobState::Canceled,
            ended_at: Some(now_ms()),
            ..StateTransition::default()
        },
    )?;
    let record = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    Ok(record_to_summary(record))
}

fn handle_import_queue_snapshot(state: &NotionState) -> Result<ImportQueueSnapshot, String> {
    let mut running = Vec::new();
    let mut waiting = Vec::new();
    let mut paused = Vec::new();
    let records = state.job_store.list_pending_jobs()?;
    for record in records {
        let job_state = record.state.clone();
        let summary = record_to_summary(record);
        match job_state {
            JobState::Running => running.push(summary),
            JobState::Paused => paused.push(summary),
            JobState::Pending | JobState::Queued => waiting.push(summary),
            _ => {}
        }
    }
    sort_running(&mut running);
    sort_by_priority(&mut waiting);
    sort_by_priority(&mut paused);
    Ok(ImportQueueSnapshot {
        running,
        waiting,
        paused,
        timestamp: now_ms(),
    })
}

fn handle_import_promote(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    let record = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    let previous_priority = record.priority;
    state.scheduler.promote(job_id.clone())?;
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        if let Some(updated) = state.job_store.load_job(&job_id)? {
            if updated.priority > previous_priority {
                return Ok(record_to_summary(updated));
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle_import_get_job(state, job_id)
}

fn handle_import_requeue(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    state.scheduler.requeue(job_id.clone())?;
    handle_import_get_job(state, job_id)
}

fn handle_import_set_priority(
    state: &NotionState,
    job_id: String,
    priority: i32,
) -> Result<ImportJobSummary, String> {
    state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    state.job_store.set_priority(&job_id, priority)?;
    state.scheduler.set_priority(job_id.clone(), priority)?;
    let record = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    Ok(record_to_summary(record))
}

fn handle_import_get_job(state: &NotionState, job_id: String) -> Result<ImportJobSummary, String> {
    if let Some(record) = state.job_store.load_job(&job_id)? {
        Ok(record_to_summary(record))
    } else {
        state
            .job_runner
            .snapshot(&job_id)
            .map(|snapshot| snapshot_to_summary(job_id, snapshot))
            .ok_or_else(|| "Job not found".to_string())
    }
}

fn handle_import_list_jobs(state: &NotionState) -> Result<Vec<ImportJobSummary>, String> {
    let records = state.job_store.list_pending_jobs()?;
    if records.is_empty() {
        Ok(state
            .job_runner
            .list()
            .into_iter()
            .map(|(job_id, snapshot)| snapshot_to_summary(job_id, snapshot))
            .collect())
    } else {
        Ok(records.into_iter().map(record_to_summary).collect())
    }
}

fn handle_import_history(
    state: &NotionState,
    req: ImportHistoryRequest,
) -> Result<ImportHistoryPage, String> {
    let page_size = req.page_size.unwrap_or(20).clamp(1, 100);
    let page = req.page.unwrap_or(0);
    let offset = page.saturating_mul(page_size);
    let parsed_states: Option<Vec<JobState>> = match req.states {
        Some(values) => {
            if values.is_empty() {
                None
            } else {
                let mut parsed = Vec::with_capacity(values.len());
                for value in values {
                    let normalized = parse_job_state_label(&value)
                        .ok_or_else(|| format!("unknown job state '{}'", value))?;
                    parsed.push(normalized);
                }
                if parsed.is_empty() {
                    None
                } else {
                    Some(parsed)
                }
            }
        }
        None => None,
    };
    let filter_slice = parsed_states.as_ref().map(|vec| vec.as_slice());
    let records = state
        .job_store
        .list_history(offset, page_size, filter_slice)
        .map_err(|e| e)?;
    let total = state
        .job_store
        .count_history(filter_slice)
        .map_err(|e| e)?;
    let has_more = offset.saturating_add(records.len()) < total;
    let summaries = records.into_iter().map(record_to_summary).collect();
    Ok(ImportHistoryPage {
        items: summaries,
        total,
        page,
        page_size,
        has_more,
    })
}

fn handle_export_failed(state: &NotionState, job_id: String) -> Result<ExportFailedResult, String> {
    let job = state
        .job_store
        .load_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    let rows = state.job_store.list_failed_rows(&job_id)?;
    let mut sanitized = job_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        sanitized = "notion-import-job".into();
    }
    let file_name = format!("{}-failed-rows.csv", sanitized);
    let export_path = std::env::temp_dir().join(file_name);
    let mut writer = csv::Writer::from_path(&export_path).map_err(|e| e.to_string())?;
    writer
        .write_record([
            "row_index",
            "error_code",
            "error_message",
            "error_payload_json",
        ])
        .map_err(|e| e.to_string())?;
    for row in rows.iter() {
        writer
            .write_record([
                row.row_index.to_string(),
                row.error_code.clone().unwrap_or_default(),
                row.error_message.clone().unwrap_or_default(),
                row.error_payload_json.clone().unwrap_or_default(),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())?;
    let path_str = export_path.to_string_lossy().to_string();
    state.job_runner.emit_log(
        &job.id,
        JobLogLevel::Info,
        format!("exported {} failed rows to {}", rows.len(), path_str),
    );
    Ok(ExportFailedResult {
        job_id: job.id,
        path: path_str,
        total: rows.len(),
    })
}

fn validate_schema_constraints(property: &DatabaseProperty, value: &Value) -> Result<(), String> {
    match property.type_.as_str() {
        "relation" => {
            let count = match value {
                Value::Array(arr) => arr.len(),
                Value::Null => 0,
                _ => 1,
            };
            if count > 100 {
                Err("relation supports at most 100 linked pages".into())
            } else {
                Ok(())
            }
        }
        "multi_select" => {
            if let Value::Array(arr) = value {
                if arr.len() > 100 {
                    return Err("multi_select supports at most 100 options".into());
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_option_values(property: &DatabaseProperty, entry: &Value) -> Result<(), String> {
    let Some(options) = property.options.as_ref() else {
        return Ok(());
    };
    if options.is_empty() {
        return Ok(());
    }

    match property.type_.as_str() {
        "select" | "status" => {
            let key = property.type_.as_str();
            if let Some(name) = entry
                .get(key)
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
            {
                if !options.iter().any(|opt| opt == name) {
                    return Err(format!("option '{}' is not defined in schema", name));
                }
            }
            Ok(())
        }
        "multi_select" => {
            if let Some(arr) = entry.get("multi_select").and_then(|v| v.as_array()) {
                for item in arr {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if !options.iter().any(|opt| opt == name) {
                        return Err(format!("option '{}' is not defined in schema", name));
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn property_value_has_content(property: &DatabaseProperty, value: &Value) -> bool {
    match property.type_.as_str() {
        "title" => title_entry_has_content(value),
        "rich_text" => value
            .get("rich_text")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "number" => value.get("number").map(|v| !v.is_null()).unwrap_or(false),
        "select" | "status" => value
            .get(property.type_.as_str())
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "multi_select" => value
            .get("multi_select")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "checkbox" => value
            .get("checkbox")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "people" => value
            .get("people")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "relation" => value
            .get("relation")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "files" => value
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        _ => true,
    }
}

fn title_entry_has_content(value: &Value) -> bool {
    value
        .get("title")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().any(|fragment| {
                fragment
                    .get("text")
                    .and_then(|t| t.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notion::types::{DatabaseProperty, FieldMapping};
    use serde_json::json;
    use std::thread;
    use std::time::Duration;
    use tempfile::Builder;

    #[test]
    fn filter_removes_empty_titles_by_default() {
        let mut list = vec![
            DatabaseBrief {
                id: "1".into(),
                title: "".into(),
                icon: None,
            },
            DatabaseBrief {
                id: "2".into(),
                title: "  ".into(),
                icon: None,
            },
            DatabaseBrief {
                id: "3".into(),
                title: "A".into(),
                icon: None,
            },
        ];
        apply_filter_empty_title(&mut list, false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "3");
    }

    #[test]
    fn filter_keeps_empty_when_included() {
        let mut list = vec![
            DatabaseBrief {
                id: "1".into(),
                title: "".into(),
                icon: None,
            },
            DatabaseBrief {
                id: "2".into(),
                title: "B".into(),
                icon: None,
            },
        ];
        apply_filter_empty_title(&mut list, true);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn dry_run_returns_error_when_no_records() {
        let schema = DatabaseSchema {
            id: "db".into(),
            title: "Test DB".into(),
            properties: vec![DatabaseProperty {
                name: "Name".into(),
                type_: "title".into(),
                required: Some(true),
                options: None,
            }],
        };
        let input = DryRunInput {
            schema,
            mappings: vec![],
            records: vec![],
            defaults: Value::Null,
        };
        let result = notion_import_dry_run(input);
        assert!(result.is_err());
    }

    #[test]
    fn dry_run_reports_transform_error() {
        let schema = DatabaseSchema {
            id: "db".into(),
            title: "Test DB".into(),
            properties: vec![DatabaseProperty {
                name: "Name".into(),
                type_: "title".into(),
                required: Some(true),
                options: None,
            }],
        };
        let mappings = vec![FieldMapping {
            include: true,
            source_field: "title".into(),
            target_property: "Name".into(),
            target_type: "title".into(),
            transform_code: Some("function transform(value) { throw new Error('oops'); }".into()),
        }];
        let records = vec![json!({ "title": "hello" })];
        let input = DryRunInput {
            schema,
            mappings,
            records,
            defaults: Value::Null,
        };
        let report = notion_import_dry_run(input).expect("should succeed");
        assert_eq!(report.total, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].message.contains("oops"));
    }

    #[test]
    fn transform_eval_sample_runs_code() {
        let req = TransformEvalRequest {
            code: "function transform(value) { return value + '!'; }".into(),
            value: json!("hi"),
            record: json!({"title": "hi"}),
            row_index: 0,
        };
        let res = notion_transform_eval_sample(req).expect("eval");
        assert_eq!(res.result, json!("hi!"));
    }

    #[test]
    fn import_start_persists_job_in_store_and_runner() {
        let state = create_default_state();
        let token = state
            .store
            .save("demo", "secret-token", Some("Workspace".into()));

        let file = Builder::new()
            .suffix(".json")
            .tempfile()
            .expect("create temp file");
        let path = file.path().to_path_buf();
        let records = vec![json!({"title": "hello"}), json!({"title": "world"})];
        serde_json::to_writer(std::fs::File::create(&path).unwrap(), &records).expect("write json");

        let req = ImportJobRequest {
            job_id: None,
            token_id: token.id.clone(),
            database_id: "db-1".into(),
            source_file_path: path.to_string_lossy().to_string(),
            file_type: "json".into(),
            mappings: vec![FieldMapping {
                include: true,
                source_field: "title".into(),
                target_property: "Name".into(),
                target_type: "title".into(),
                transform_code: None,
            }],
            defaults: None,
            rate_limit: None,
            batch_size: None,
            priority: None,
            upsert: None,
        };

        let handle = handle_import_start(&state, req).expect("start job");

        for _ in 0..20 {
            let record = state
                .job_store
                .load_job(&handle.job_id)
                .expect("load job")
                .expect("job record");
            if record.state == JobState::Completed {
                assert_eq!(record.progress.done, records.len());
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let record = state
            .job_store
            .load_job(&handle.job_id)
            .expect("load job")
            .expect("job record");
        assert_eq!(record.id, handle.job_id);
        assert_eq!(record.state, JobState::Completed);

        let snapshot = state
            .job_runner
            .snapshot(&handle.job_id)
            .expect("job snapshot");
        assert_eq!(snapshot.state, JobState::Completed);
    }

    #[test]
    fn import_pause_resume_cancel_update_store_state() {
        let state = create_default_state();
        let job_id = "job-flow".to_string();
        state
            .job_store
            .insert_job(NewImportJob {
                id: job_id.clone(),
                token_id: "tok".into(),
                database_id: "db".into(),
                source_file_path: "/tmp/data.csv".into(),
                config_snapshot_json: "{}".into(),
                total: None,
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");
        state
            .job_store
            .mark_state(
                &job_id,
                StateTransition {
                    state: JobState::Running,
                    started_at: Some(now_ms()),
                    ..StateTransition::default()
                },
            )
            .expect("mark running");
        state.job_runner.register_job(job_id.clone());
        state.job_runner.mark_running(&job_id);

        let paused = handle_import_pause(&state, job_id.clone()).expect("pause");
        assert_eq!(paused.state, JobState::Paused);
        let paused_record = state
            .job_store
            .load_job(&job_id)
            .expect("load job")
            .expect("job record");
        assert_eq!(paused_record.state, JobState::Paused);

        let resumed = handle_import_resume(&state, job_id.clone()).expect("resume");
        assert_eq!(resumed.state, JobState::Running);
        let resumed_record = state
            .job_store
            .load_job(&job_id)
            .expect("load job")
            .expect("job record");
        assert_eq!(resumed_record.state, JobState::Running);

        let canceled = handle_import_cancel(&state, job_id.clone()).expect("cancel");
        assert_eq!(canceled.state, JobState::Canceled);
        let canceled_record = state
            .job_store
            .load_job(&job_id)
            .expect("load job")
            .expect("job record");
        assert_eq!(canceled_record.state, JobState::Canceled);
    }

    fn insert_history_job(
        state: &NotionState,
        id: &str,
        job_state: JobState,
        created_at: i64,
        ended_at: Option<i64>,
    ) {
        state
            .job_store
            .insert_job(NewImportJob {
                id: id.to_string(),
                token_id: "tok".into(),
                database_id: "db".into(),
                source_file_path: format!("/tmp/{id}.json"),
                config_snapshot_json: "{}".into(),
                total: Some(10),
                created_at,
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");
        state
            .job_store
            .mark_state(
                id,
                StateTransition {
                    state: job_state,
                    started_at: Some(created_at + 10),
                    ended_at,
                    last_error: None,
                },
            )
            .expect("mark state");
    }

    #[test]
    fn history_handler_returns_paginated_jobs() {
        let state = create_default_state();
        let base = now_ms();
        insert_history_job(
            &state,
            "job-h1",
            JobState::Completed,
            base,
            Some(base + 1_000),
        );
        insert_history_job(
            &state,
            "job-h2",
            JobState::Failed,
            base + 1_000,
            Some(base + 2_000),
        );
        insert_history_job(
            &state,
            "job-h3",
            JobState::Canceled,
            base + 2_000,
            Some(base + 3_000),
        );
        insert_history_job(
            &state,
            "job-running-ignore",
            JobState::Running,
            base + 3_000,
            None,
        );

        let page = handle_import_history(&state, ImportHistoryRequest::default()).expect("history");
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.items[0].job_id, "job-h3");
        assert_eq!(page.items[1].job_id, "job-h2");
        assert_eq!(page.items[2].job_id, "job-h1");
        assert!(!page.has_more);
    }

    #[test]
    fn history_handler_supports_filters_and_pagination() {
        let state = create_default_state();
        let base = now_ms();
        let mut offset: i64 = 0;
        for (id, job_state) in [
            ("hist-a", JobState::Completed),
            ("hist-b", JobState::Failed),
            ("hist-c", JobState::Canceled),
            ("hist-d", JobState::Completed),
        ] {
            insert_history_job(
                &state,
                id,
                job_state,
                base + offset,
                Some(base + offset + 500),
            );
            offset += 1_000;
        }

        let page_one = handle_import_history(
            &state,
            ImportHistoryRequest {
                page: Some(0),
                page_size: Some(2),
                states: None,
            },
        )
        .expect("page one");
        assert_eq!(page_one.items.len(), 2);
        assert!(page_one.has_more);
        assert_eq!(page_one.items[0].job_id, "hist-d");
        assert_eq!(page_one.items[1].job_id, "hist-c");

        let page_two = handle_import_history(
            &state,
            ImportHistoryRequest {
                page: Some(1),
                page_size: Some(2),
                states: None,
            },
        )
        .expect("page two");
        assert_eq!(page_two.items.len(), 2);
        assert!(!page_two.has_more);
        assert_eq!(page_two.items[0].job_id, "hist-b");
        assert_eq!(page_two.items[1].job_id, "hist-a");

        let failed_only = handle_import_history(
            &state,
            ImportHistoryRequest {
                page: Some(0),
                page_size: Some(5),
                states: Some(vec!["Failed".into()]),
            },
        )
        .expect("failed filter");
        assert_eq!(failed_only.items.len(), 1);
        assert_eq!(failed_only.total, 1);
        assert_eq!(failed_only.items[0].job_id, "hist-b");
    }
}
