use std::sync::{Arc, Mutex};

use tauri::State;

use super::adapter::{MockNotionAdapter, NotionAdapter};
use super::mapping::build_property_entry;
use super::preview::{preview_file as notion_preview_file, PreviewRequest, PreviewResponse};
use super::storage::{InMemoryTokenStore, TokenStore};
use super::transform::{TransformContext, TransformExecutor};
#[cfg(feature = "notion-http")]
use super::adapter::HttpNotionAdapter;
#[cfg(feature = "notion-sqlite")]
use super::storage::SqliteTokenStore;
use std::path::PathBuf;
use super::types::{
    DatabaseBrief,
    DatabasePage,
    DatabaseSchema,
    DryRunInput,
    DryRunReport,
    ImportTemplate,
    RowError,
    SaveTokenRequest,
    TokenRow,
    TransformEvalRequest,
    TransformEvalResult,
    WorkspaceInfo,
};
use serde_json::Value;
use rusqlite::Connection;

fn apply_filter_empty_title(list: &mut Vec<DatabaseBrief>, include_empty_title: bool) {
    if !include_empty_title {
        list.retain(|d| !d.title.trim().is_empty());
    }
}

pub struct NotionState {
    pub store: Arc<dyn TokenStore>,
    pub adapter: Arc<dyn NotionAdapter>,
    pub db_path: Option<std::path::PathBuf>,
    // Fallback in-memory template store when SQLite path is unavailable (e.g., in tests).
    pub templates_mem: Arc<Mutex<Vec<ImportTemplate>>>,
}

impl NotionState {
    pub fn new(store: Arc<dyn TokenStore>, adapter: Arc<dyn NotionAdapter>) -> Self {
        Self { store, adapter, db_path: None, templates_mem: Arc::new(Mutex::new(Vec::new())) }
    }
}

pub fn create_default_state() -> NotionState {
    NotionState::new(
        Arc::new(InMemoryTokenStore::new()),
        Arc::new(MockNotionAdapter),
    )
}

#[cfg(feature = "notion-sqlite")]
pub fn create_state_with_sqlite(db_path: PathBuf) -> NotionState {
    let store: Arc<dyn TokenStore> = Arc::new(SqliteTokenStore::new(db_path.clone()));
    #[cfg(feature = "notion-http")]
    let adapter: Arc<dyn NotionAdapter> = Arc::new(HttpNotionAdapter);
    #[cfg(not(feature = "notion-http"))]
    let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter);
    let mut state = NotionState::new(store, adapter);
    state.db_path = Some(db_path);
    state
}

#[tauri::command]
pub async fn notion_save_token(state: State<'_, NotionState>, req: SaveTokenRequest) -> Result<TokenRow, String> {
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
        let ws = adapter
            .test_connection(&token)
            .unwrap_or(WorkspaceInfo { workspace_name: None, bot_name: None });
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
    if state.store.delete(&id) { Ok(()) } else { Err("Not found".into()) }
}

#[tauri::command]
pub async fn notion_test_connection(state: State<'_, NotionState>, token_id: String) -> Result<WorkspaceInfo, String> {
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
    let page = tauri::async_runtime::spawn_blocking(move || adapter.search_databases_page(&token, query, cursor, page_size))
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
pub async fn notion_get_database(state: State<'_, NotionState>, token_id: String, database_id: String) -> Result<DatabaseSchema, String> {
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

fn now_ms() -> i64 { chrono::Utc::now().timestamp_millis() }

#[tauri::command]
pub fn notion_template_save(state: State<NotionState>, tpl: ImportTemplate) -> Result<ImportTemplate, String> {
    // Basic validations
    if tpl.name.trim().is_empty() { return Err("Template name is required".into()); }
    if tpl.token_id.trim().is_empty() { return Err("tokenId is required".into()); }
    if tpl.database_id.trim().is_empty() { return Err("databaseId is required".into()); }
    let mapping_payload = MappingJsonPayload { version: 1, mappings: tpl.mappings.clone() };
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
                if affected == 0 { return Err("Template not found".into()); }
                Ok(ImportTemplate { id: Some(id), ..tpl })
            }
            None => {
                let mut stmt = conn.prepare(
                    "INSERT INTO notion_import_templates (id, name, token_id, database_id, mapping_json, defaults_json, created_at, updated_at)
                     VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id"
                ).map_err(|e| e.to_string())?;
                let new_id: String = stmt.query_row((tpl.name.as_str(), tpl.token_id.as_str(), tpl.database_id.as_str(), mapping_json.as_str(), defaults_json.as_deref(), now, now), |row| row.get(0)).map_err(|e| e.to_string())?;
                Ok(ImportTemplate { id: Some(new_id), ..tpl })
            }
        }
    } else {
        // In-memory fallback
        let mut guard = state.templates_mem.lock().map_err(|_| "poisoned".to_string())?;
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
pub fn notion_template_list(state: State<NotionState>, token_id: Option<String>) -> Result<Vec<ImportTemplate>, String> {
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
        let mut rows = if args.is_empty() { stmt.query([]).map_err(|e| e.to_string())? } else { stmt.query([&args[0]]).map_err(|e| e.to_string())? };
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let id: String = row.get(0).map_err(|e| e.to_string())?;
            let name: String = row.get(1).map_err(|e| e.to_string())?;
            let token_id: String = row.get(2).map_err(|e| e.to_string())?;
            let database_id: String = row.get(3).map_err(|e| e.to_string())?;
            let mapping_json: String = row.get(4).map_err(|e| e.to_string())?;
            let defaults_json_opt: Option<String> = row.get(5).map_err(|e| e.to_string())?;
            let payload: MappingJsonPayload = serde_json::from_str(&mapping_json).map_err(|e| e.to_string())?;
            let defaults = defaults_json_opt.and_then(|s| serde_json::from_str::<Value>(s.as_str()).ok());
            out.push(ImportTemplate { id: Some(id), name, token_id, database_id, mappings: payload.mappings, defaults });
        }
        Ok(out)
    } else {
        let guard = state.templates_mem.lock().map_err(|_| "poisoned".to_string())?;
        Ok(guard.clone())
    }
}

#[tauri::command]
pub fn notion_template_delete(state: State<NotionState>, id: String) -> Result<(), String> {
    if let Some(path) = &state.db_path {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let affected = conn.execute("DELETE FROM notion_import_templates WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
        if affected == 0 { return Err("Template not found".into()); }
        Ok(())
    } else {
        let mut guard = state.templates_mem.lock().map_err(|_| "poisoned".to_string())?;
        let before = guard.len();
        guard.retain(|t| t.id.as_deref() != Some(id.as_str()));
        if guard.len() == before { return Err("Template not found".into()); }
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

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<RowError> = Vec::new();
    let mut executor: Option<TransformExecutor> = None;
    let has_title_prop = input.schema.properties.iter().any(|p| p.type_ == "title");

    for (idx, rec) in input.records.iter().enumerate() {
        let obj = match rec.as_object() {
            Some(map) => map.clone(),
            None => {
                failed += 1;
                errors.push(RowError { row_index: idx, message: "record is not an object".into() });
                continue;
            }
        };

        let mut props = serde_json::Map::new();
        let mut record_failed = false;

        for mapping in input.mappings.iter().filter(|m| m.include) {
            let src_val = obj
                .get(&mapping.source_field)
                .cloned()
                .unwrap_or(Value::Null);
            let effective_val = if let Some(code) = mapping.transform_code.as_ref().filter(|c| !c.trim().is_empty()) {
                if executor.is_none() {
                    executor = Some(TransformExecutor::new().map_err(|err| err.to_string())?);
                }
                let ctx = TransformContext { row_index: idx, record: obj.clone() };
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
                        });
                        record_failed = true;
                        break;
                    }
                }
            } else {
                src_val
            };

            match build_property_entry(mapping, &effective_val) {
                Ok(entry) => {
                    props.insert(mapping.target_property.clone(), entry);
                }
                Err(err) => {
                    failed += 1;
                    errors.push(RowError {
                        row_index: idx,
                        message: format!("mapping error ({} -> {}): {}", mapping.source_field, mapping.target_property, err),
                    });
                    record_failed = true;
                    break;
                }
            }
        }

        if record_failed {
            continue;
        }

        if has_title_prop && !props.values().any(|v| v.get("title").is_some()) {
            failed += 1;
            errors.push(RowError { row_index: idx, message: "Missing title property mapping".into() });
            continue;
        }

        ok += 1;
    }

    Ok(DryRunReport { total: input.records.len(), ok, failed, errors })
}

// -----------------------------
// M2: Preview & Transform helpers
// -----------------------------

#[tauri::command]
pub fn notion_import_preview_file(req: PreviewRequest) -> Result<PreviewResponse, String> {
    notion_preview_file(&req)
}

#[tauri::command]
pub fn notion_transform_eval_sample(req: TransformEvalRequest) -> Result<TransformEvalResult, String> {
    let record_map = req
        .record
        .as_object()
        .cloned()
        .ok_or_else(|| "record must be an object".to_string())?;
    let executor = TransformExecutor::new().map_err(|err| err.to_string())?;
    let ctx = TransformContext { row_index: req.row_index, record: record_map };
    let result = executor
        .execute(&req.code, req.value, ctx)
        .map_err(|err| err.to_string())?;
    Ok(TransformEvalResult { result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::notion::types::{DatabaseProperty, FieldMapping};

    #[test]
    fn filter_removes_empty_titles_by_default() {
        let mut list = vec![
            DatabaseBrief { id: "1".into(), title: "".into(), icon: None },
            DatabaseBrief { id: "2".into(), title: "  ".into(), icon: None },
            DatabaseBrief { id: "3".into(), title: "A".into(), icon: None },
        ];
        apply_filter_empty_title(&mut list, false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "3");
    }

    #[test]
    fn filter_keeps_empty_when_included() {
        let mut list = vec![
            DatabaseBrief { id: "1".into(), title: "".into(), icon: None },
            DatabaseBrief { id: "2".into(), title: "B".into(), icon: None },
        ];
        apply_filter_empty_title(&mut list, true);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn dry_run_returns_error_when_no_records() {
        let schema = DatabaseSchema {
            id: "db".into(),
            title: "Test DB".into(),
            properties: vec![DatabaseProperty { name: "Name".into(), type_: "title".into(), required: Some(true), options: None }],
        };
        let input = DryRunInput { schema, mappings: vec![], records: vec![] };
        let result = notion_import_dry_run(input);
        assert!(result.is_err());
    }

    #[test]
    fn dry_run_reports_transform_error() {
        let schema = DatabaseSchema {
            id: "db".into(),
            title: "Test DB".into(),
            properties: vec![DatabaseProperty { name: "Name".into(), type_: "title".into(), required: Some(true), options: None }],
        };
        let mappings = vec![
            FieldMapping {
                include: true,
                source_field: "title".into(),
                target_property: "Name".into(),
                target_type: "title".into(),
                transform_code: Some("function transform(value) { throw new Error('oops'); }".into()),
            },
        ];
        let records = vec![json!({ "title": "hello" })];
        let input = DryRunInput { schema, mappings, records };
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
}
