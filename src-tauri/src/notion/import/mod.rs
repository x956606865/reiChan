use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::notion::adapter::{
    CreatePageRequest, LookupProperty, NotionAdapter, NotionApiError, NotionApiErrorKind,
    PageSnapshot,
};
use crate::notion::io::{RecordStream, StreamPosition};
use crate::notion::job_runner::{
    JobCommand, JobController, JobLogLevel, JobProgress, JobRunner, JobState,
};
use crate::notion::mapping::build_property_entry;
use crate::notion::storage::{
    CheckpointRecord, ImportJobRecord, ImportJobRowRecord, ImportJobRowStatus, ImportJobStore,
    ProgressUpdate, StateTransition,
};
use crate::notion::transform::{TransformContext, TransformExecutor};
use crate::notion::types::{FieldMapping, ImportUpsertConfig, UpsertStrategy};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Clone)]
pub struct ImportEngine {
    adapter: Arc<dyn NotionAdapter>,
    job_store: Arc<dyn ImportJobStore>,
    job_runner: Arc<JobRunner>,
}

impl ImportEngine {
    pub fn new(
        adapter: Arc<dyn NotionAdapter>,
        job_store: Arc<dyn ImportJobStore>,
        job_runner: Arc<JobRunner>,
    ) -> Self {
        Self {
            adapter,
            job_store,
            job_runner,
        }
    }

    pub fn spawn_job(&self, ctx: StartContext) -> Result<JobWorkerHandle, String> {
        let record = self
            .job_store
            .load_job(&ctx.job_id)?
            .ok_or_else(|| "job not found".to_string())?;
        let snapshot: JobConfigSnapshot = serde_json::from_str(&record.config_snapshot_json)
            .map_err(|err| format!("invalid job snapshot: {}", err))?;

        let (tx, rx) = unbounded_channel();
        self.job_runner
            .attach_controller(&ctx.job_id, JobController::new(tx.clone()))?;

        let worker_ctx = WorkerContext {
            job_id: ctx.job_id.clone(),
            token: ctx.token,
            record,
            config: snapshot,
            job_store: Arc::clone(&self.job_store),
            job_runner: Arc::clone(&self.job_runner),
            adapter: Arc::clone(&self.adapter),
            command_rx: rx,
        };

        let handle = thread::spawn(move || run_worker(worker_ctx));
        Ok(JobWorkerHandle {
            join_handle: handle,
        })
    }
}

pub struct StartContext {
    pub job_id: String,
    pub token: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub struct JobWorkerHandle {
    join_handle: thread::JoinHandle<()>,
}

impl JobWorkerHandle {
    #[cfg(test)]
    pub fn join(self) {
        if self.join_handle.join().is_err() {
            panic!("worker panicked");
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobConfigSnapshot {
    #[allow(unused)]
    version: u32,
    #[allow(unused)]
    token_id: String,
    #[allow(unused)]
    database_id: String,
    source_file_path: String,
    #[allow(unused)]
    file_type: String,
    #[allow(unused)]
    mappings: Vec<FieldMapping>,
    #[allow(unused)]
    defaults: Option<Value>,
    #[allow(unused)]
    rate_limit: Option<u32>,
    batch_size: Option<usize>,
    #[allow(unused)]
    upsert: Option<ImportUpsertConfig>,
}

struct LookupCache {
    adapter: Arc<dyn NotionAdapter>,
    token: String,
    database_id: String,
    entries: HashMap<String, PageSnapshot>,
}

impl LookupCache {
    fn new(adapter: Arc<dyn NotionAdapter>, token: String, database_id: String) -> Self {
        Self {
            adapter,
            token,
            database_id,
            entries: HashMap::new(),
        }
    }

    fn lookup(
        &mut self,
        properties: &[LookupProperty],
    ) -> Result<Option<PageSnapshot>, NotionApiError> {
        let key = lookup_cache_key(properties);
        if let Some(snapshot) = self.entries.get(&key) {
            return Ok(Some(snapshot.clone()));
        }
        let result = self
            .adapter
            .lookup_page(&self.token, &self.database_id, properties)?;
        if let Some(ref snapshot) = result {
            self.entries.insert(key, snapshot.clone());
        }
        Ok(result)
    }

    fn put(&mut self, properties: &[LookupProperty], snapshot: PageSnapshot) {
        let key = lookup_cache_key(properties);
        self.entries.insert(key, snapshot);
    }
}

fn lookup_cache_key(properties: &[LookupProperty]) -> String {
    let mut fragments: Vec<String> = properties
        .iter()
        .map(|prop| {
            let serialized = serde_json::to_string(&prop.property).unwrap_or_default();
            format!("{}={}", prop.name, serialized)
        })
        .collect();
    fragments.sort();
    fragments.join("|")
}

enum HandleRowOutcome {
    Created,
    Updated {
        previous: Map<String, Value>,
        strategy: UpsertStrategy,
    },
    Skipped {
        previous: Map<String, Value>,
        strategy: UpsertStrategy,
    },
}

struct RowFailure {
    code: Option<String>,
    message: String,
    payload: Option<String>,
}

struct WorkerContext {
    job_id: String,
    #[allow(dead_code)]
    token: Option<String>,
    record: ImportJobRecord,
    config: JobConfigSnapshot,
    job_store: Arc<dyn ImportJobStore>,
    job_runner: Arc<JobRunner>,
    #[allow(dead_code)]
    adapter: Arc<dyn NotionAdapter>,
    command_rx: UnboundedReceiver<JobCommand>,
}

fn run_worker(mut ctx: WorkerContext) {
    let Some(token) = ctx.token.clone() else {
        mark_failed(&ctx, "token unavailable for job".into());
        return;
    };

    let batch_size = ctx.config.batch_size.unwrap_or(25).max(1);
    let mut position = StreamPosition {
        byte_offset: 0,
        record_index: ctx.record.next_offset,
    };

    if ctx.record.next_offset == 0 {
        let _ = ctx.job_store.clear_checkpoints(&ctx.job_id);
    } else if let Ok(checkpoints) = ctx.job_store.recent_checkpoints(&ctx.job_id, 5) {
        for checkpoint in checkpoints {
            if checkpoint.row_index <= position.record_index {
                position.record_index = checkpoint.row_index;
                position.byte_offset = checkpoint.file_offset;
                break;
            }
        }
    }

    let open_result = RecordStream::open(&ctx.config.source_file_path, position.clone());
    let (mut stream, mut stream_pos) = match open_result {
        Ok(pair) => pair,
        Err(err) => {
            let message = format!(
                "failed to open source {}: {}",
                ctx.config.source_file_path, err
            );
            mark_failed(&ctx, message);
            return;
        }
    };

    if position.byte_offset > 0 || position.record_index < ctx.record.next_offset {
        ctx.job_runner.emit_log(
            &ctx.job_id,
            JobLogLevel::Info,
            format!(
                "resumed from checkpoint at row {} (byte offset {})",
                stream_pos.record_index, stream_pos.byte_offset
            ),
        );
    }

    let defaults_map = ctx
        .config
        .defaults
        .as_ref()
        .and_then(|v| v.as_object())
        .cloned();
    ctx.job_runner.emit_log(
        &ctx.job_id,
        JobLogLevel::Info,
        format!(
            "starting import from {} at row {} (batch size {})",
            ctx.config.source_file_path, stream_pos.record_index, batch_size
        ),
    );
    let mut transform_executor: Option<TransformExecutor> = None;

    let upsert_config = ctx.config.upsert.clone();
    let mut lookup_cache = upsert_config
        .as_ref()
        .and_then(|cfg| cfg.dedupe_key.as_ref())
        .map(|_| {
            LookupCache::new(
                Arc::clone(&ctx.adapter),
                token.clone(),
                ctx.config.database_id.clone(),
            )
        });

    let mut paused = matches!(ctx.record.state, JobState::Paused);
    let mut cancelled = matches!(ctx.record.state, JobState::Canceled);

    let mut total_processed = ctx.record.progress.done + ctx.record.progress.failed;
    let mut last_error: Option<String> = ctx.record.last_error.clone();
    let started_at = ctx.record.started_at.unwrap_or_else(now_ms);
    let timer = Instant::now();

    let conflict_columns: &[String] = upsert_config
        .as_ref()
        .map(|cfg| cfg.conflict_columns.as_slice())
        .unwrap_or(&[]);

    while !cancelled {
        poll_commands(&mut ctx.command_rx, &mut paused, &mut cancelled);
        if cancelled {
            break;
        }
        if paused {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        let batch_start_index = stream_pos.record_index;
        match stream.next_batch(batch_size, &mut stream_pos) {
            Ok(Some(batch)) => {
                if batch.is_empty() {
                    continue;
                }

                let checkpoint_hash = compute_batch_hash(&batch);
                let mut batch_rows: Vec<ImportJobRowRecord> = Vec::new();
                let mut success_count = 0usize;
                let mut failure_count = 0usize;
                let mut skipped_count = 0usize;
                let mut conflict_count = 0usize;

                for (offset, raw) in batch.into_iter().enumerate() {
                    let row_index = batch_start_index + offset;
                    match build_properties_for_record(
                        row_index,
                        raw,
                        &ctx.config.mappings,
                        defaults_map.as_ref(),
                        &mut transform_executor,
                    ) {
                        Ok(properties) => match handle_row(
                            ctx.adapter.as_ref(),
                            &token,
                            &ctx.config.database_id,
                            &properties,
                            upsert_config.as_ref(),
                            lookup_cache.as_mut(),
                        ) {
                            Ok(HandleRowOutcome::Created) => {
                                success_count += 1;
                            }
                            Ok(HandleRowOutcome::Updated { previous, strategy }) => {
                                success_count += 1;
                                conflict_count += 1;
                                batch_rows.push(build_conflict_row(
                                    &ctx.job_id,
                                    row_index,
                                    ImportJobRowStatus::Ok,
                                    strategy,
                                    &previous,
                                    conflict_columns,
                                ));
                            }
                            Ok(HandleRowOutcome::Skipped { previous, strategy }) => {
                                skipped_count += 1;
                                conflict_count += 1;
                                batch_rows.push(build_conflict_row(
                                    &ctx.job_id,
                                    row_index,
                                    ImportJobRowStatus::Skipped,
                                    strategy,
                                    &previous,
                                    conflict_columns,
                                ));
                            }
                            Err(err) => {
                                failure_count += 1;
                                last_error = Some(err.message.clone());
                                batch_rows.push(build_failure_row(
                                    &ctx.job_id,
                                    row_index,
                                    err.code,
                                    Some(err.message),
                                    err.payload,
                                ));
                            }
                        },
                        Err(message) => {
                            failure_count += 1;
                            last_error = Some(message.clone());
                            batch_rows.push(build_failure_row(
                                &ctx.job_id,
                                row_index,
                                Some("mapping_error".into()),
                                Some(message),
                                None,
                            ));
                        }
                    }
                }

                if !batch_rows.is_empty() {
                    if let Err(err) = ctx.job_store.append_row_results(batch_rows) {
                        mark_failed(&ctx, format!("failed to persist row results: {}", err));
                        return;
                    }
                }

                if success_count + failure_count + skipped_count > 0 {
                    total_processed += success_count + failure_count + skipped_count;
                    let rps = if timer.elapsed().as_secs_f64() > 0.0 {
                        Some((total_processed as f64) / timer.elapsed().as_secs_f64())
                    } else {
                        None
                    };

                    if let Err(err) = persist_progress(
                        &ctx,
                        BatchProgress {
                            success: success_count,
                            failed: failure_count,
                            skipped: skipped_count,
                            conflicts: conflict_count,
                            next_offset: stream_pos.record_index,
                            rps,
                            last_error: last_error.clone(),
                        },
                    ) {
                        let msg = format!("failed to persist progress: {}", err);
                        mark_failed(&ctx, msg);
                        return;
                    }

                    if let Some(ref hash) = checkpoint_hash {
                        let checkpoint = CheckpointRecord {
                            job_id: ctx.job_id.clone(),
                            row_index: stream_pos.record_index,
                            file_offset: stream_pos.byte_offset,
                            data_hash: hash.clone(),
                        };
                        if let Err(err) = ctx.job_store.write_checkpoint(checkpoint) {
                            ctx.job_runner.emit_log(
                                &ctx.job_id,
                                JobLogLevel::Warn,
                                format!("failed to persist checkpoint: {}", err),
                            );
                        }
                    }

                    let end_index = stream_pos.record_index.saturating_sub(1);
                    let mut message = format!(
                        "processed rows {}-{}: ok={}, failed={}, skipped={}, conflicts={}, total_processed={}",
                        batch_start_index,
                        end_index,
                        success_count,
                        failure_count,
                        skipped_count,
                        conflict_count,
                        total_processed
                    );
                    if failure_count > 0 {
                        if let Some(err_text) = last_error.as_ref() {
                            message.push_str(&format!(" | last_error={}", err_text));
                        }
                    }
                    let level = if failure_count > 0 {
                        JobLogLevel::Warn
                    } else {
                        JobLogLevel::Info
                    };
                    ctx.job_runner.emit_log(&ctx.job_id, level, message);
                }
            }
            Ok(None) => {
                finalize_success(&ctx, total_processed);
                return;
            }
            Err(err) => {
                let message = format!("failed to read batch: {}", err);
                mark_failed(&ctx, message);
                return;
            }
        }

        thread::yield_now();
    }

    if cancelled {
        ctx.job_runner
            .emit_log(&ctx.job_id, JobLogLevel::Warn, "import canceled by user");
        let _ = ctx.job_store.mark_state(
            &ctx.job_id,
            StateTransition {
                state: JobState::Canceled,
                started_at: Some(started_at),
                ended_at: Some(now_ms()),
                last_error,
            },
        );
        ctx.job_runner.set_state(&ctx.job_id, JobState::Canceled);
    }
}

struct BatchProgress {
    success: usize,
    failed: usize,
    skipped: usize,
    conflicts: usize,
    next_offset: usize,
    rps: Option<f64>,
    last_error: Option<String>,
}

fn poll_commands(rx: &mut UnboundedReceiver<JobCommand>, paused: &mut bool, cancelled: &mut bool) {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            JobCommand::Pause => *paused = true,
            JobCommand::Resume => *paused = false,
            JobCommand::Cancel | JobCommand::Shutdown => *cancelled = true,
            _ => {}
        }
    }
}

fn invoke_create_page(
    adapter: &dyn NotionAdapter,
    token: &str,
    database_id: &str,
    properties: &Map<String, Value>,
) -> Result<(), NotionApiError> {
    let mut attempts = 0usize;
    let mut backoff_ms = 100u64;
    loop {
        attempts += 1;
        let request = CreatePageRequest {
            database_id: database_id.to_string(),
            properties: properties.clone(),
        };
        match adapter.create_page(token, request) {
            Ok(_) => return Ok(()),
            Err(err) => {
                if err.kind.is_retryable() && attempts < 5 {
                    let sleep_ms = err.retry_after_ms.unwrap_or(backoff_ms);
                    thread::sleep(Duration::from_millis(sleep_ms));
                    if err.retry_after_ms.is_none() {
                        backoff_ms = (backoff_ms.saturating_mul(2)).min(1600);
                    }
                    continue;
                }
                return Err(err);
            }
        }
    }
}

fn build_properties_for_record(
    row_index: usize,
    raw: Value,
    mappings: &[FieldMapping],
    defaults: Option<&Map<String, Value>>,
    transform_executor: &mut Option<TransformExecutor>,
) -> Result<Map<String, Value>, String> {
    let obj = raw
        .as_object()
        .cloned()
        .ok_or_else(|| format!("row {} is not an object", row_index))?;

    let mut props = Map::new();

    for mapping in mappings.iter().filter(|m| m.include) {
        let source_val = obj
            .get(&mapping.source_field)
            .cloned()
            .unwrap_or(Value::Null);

        let effective_val = if let Some(code) = mapping
            .transform_code
            .as_ref()
            .filter(|c| !c.trim().is_empty())
        {
            let executor = ensure_transform_executor(transform_executor)
                .map_err(|err| format!("transform init failed: {}", err))?;
            match executor.execute(
                code,
                source_val.clone(),
                TransformContext {
                    row_index,
                    record: obj.clone(),
                },
            ) {
                Ok(val) => val,
                Err(err) => {
                    return Err(format!(
                        "transform error ({} -> {}): {}",
                        mapping.source_field, mapping.target_property, err
                    ))
                }
            }
        } else {
            source_val
        };

        let entry = build_property_entry(mapping, &effective_val).map_err(|err| {
            format!(
                "mapping error ({} -> {}): {}",
                mapping.source_field, mapping.target_property, err
            )
        })?;
        props.insert(mapping.target_property.clone(), entry);
    }

    if let Some(defaults_map) = defaults {
        apply_defaults(defaults_map, mappings, &mut props)?;
    }

    Ok(props)
}

fn ensure_transform_executor(
    executor: &mut Option<TransformExecutor>,
) -> Result<&TransformExecutor, String> {
    if executor.is_none() {
        *executor = Some(
            TransformExecutor::new()
                .map_err(|err| format!("transform executor init error: {}", err))?,
        );
    }
    Ok(executor.as_ref().expect("transform executor initialized"))
}

fn apply_defaults(
    defaults: &Map<String, Value>,
    mappings: &[FieldMapping],
    props: &mut Map<String, Value>,
) -> Result<(), String> {
    for (prop_name, default_value) in defaults {
        if props.contains_key(prop_name) {
            continue;
        }

        let (target_type, payload) = extract_default_payload(prop_name, default_value, mappings)?;

        let stub = FieldMapping {
            include: true,
            source_field: prop_name.clone(),
            target_property: prop_name.clone(),
            target_type,
            transform_code: None,
        };
        let entry = build_property_entry(&stub, &payload)
            .map_err(|err| format!("defaults for '{}' mapping error: {}", prop_name, err))?;
        props.insert(prop_name.clone(), entry);
    }
    Ok(())
}

fn extract_default_payload(
    prop_name: &str,
    raw: &Value,
    mappings: &[FieldMapping],
) -> Result<(String, Value), String> {
    if let Some(obj) = raw.as_object() {
        let is_flagged = obj
            .get("__reiDefault")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false);
        if is_flagged {
            let target_type = obj
                .get("targetType")
                .and_then(|val| val.as_str())
                .ok_or_else(|| format!("defaults for '{}' missing targetType", prop_name))?;
            let payload = obj
                .get("value")
                .cloned()
                .unwrap_or(Value::Null);
            return Ok((target_type.to_string(), payload));
        }
    }

    let target_type = mappings
        .iter()
        .find(|m| m.target_property == prop_name)
        .map(|m| m.target_type.clone())
        .ok_or_else(|| format!("defaults references unknown property '{}'", prop_name))?;

    Ok((target_type, raw.clone()))
}

fn build_lookup_properties(
    props: &Map<String, Value>,
    dedupe_key: &str,
) -> Result<Vec<LookupProperty>, String> {
    match props.get(dedupe_key) {
        Some(value) => Ok(vec![LookupProperty {
            name: dedupe_key.to_string(),
            property: value.clone(),
        }]),
        None => Err(format!(
            "dedupe key '{}' missing from mapped properties",
            dedupe_key
        )),
    }
}

fn handle_row(
    adapter: &dyn NotionAdapter,
    token: &str,
    database_id: &str,
    properties: &Map<String, Value>,
    upsert_config: Option<&ImportUpsertConfig>,
    lookup_cache: Option<&mut LookupCache>,
) -> Result<HandleRowOutcome, RowFailure> {
    if let (Some(config), Some(cache)) = (upsert_config, lookup_cache) {
        let dedupe_key = config.dedupe_key.as_deref().ok_or_else(|| RowFailure {
            code: Some("upsert_dedupe_missing".into()),
            message: "dedupe key missing from upsert config".into(),
            payload: None,
        })?;
        let lookup_props =
            build_lookup_properties(properties, dedupe_key).map_err(|message| RowFailure {
                code: Some("upsert_dedupe_missing".into()),
                message,
                payload: None,
            })?;
        match cache.lookup(&lookup_props) {
            Ok(Some(existing)) => match config.strategy {
                UpsertStrategy::Skip => Ok(HandleRowOutcome::Skipped {
                    previous: existing.properties,
                    strategy: UpsertStrategy::Skip,
                }),
                UpsertStrategy::Overwrite | UpsertStrategy::Merge => {
                    adapter
                        .update_page(token, &existing.page_id, properties.clone())
                        .map_err(|err| RowFailure {
                            code: err
                                .code
                                .clone()
                                .or_else(|| Some(error_kind_code(err.kind).into())),
                            message: err.message,
                            payload: serde_json::to_string(&Value::Object(properties.clone())).ok(),
                        })?;
                    cache.put(
                        &lookup_props,
                        PageSnapshot {
                            page_id: existing.page_id,
                            properties: properties.clone(),
                        },
                    );
                    Ok(HandleRowOutcome::Updated {
                        previous: existing.properties,
                        strategy: config.strategy.clone(),
                    })
                }
            },
            Ok(None) => {
                invoke_create_page(adapter, token, database_id, properties).map_err(|err| {
                    RowFailure {
                        code: err
                            .code
                            .clone()
                            .or_else(|| Some(error_kind_code(err.kind).into())),
                        message: err.message,
                        payload: serde_json::to_string(&Value::Object(properties.clone())).ok(),
                    }
                })?;
                Ok(HandleRowOutcome::Created)
            }
            Err(err) => Err(RowFailure {
                code: err
                    .code
                    .clone()
                    .or_else(|| Some(error_kind_code(err.kind).into())),
                message: err.message,
                payload: serde_json::to_string(&Value::Object(properties.clone())).ok(),
            }),
        }
    } else {
        invoke_create_page(adapter, token, database_id, properties).map_err(|err| RowFailure {
            code: err
                .code
                .clone()
                .or_else(|| Some(error_kind_code(err.kind).into())),
            message: err.message,
            payload: serde_json::to_string(&Value::Object(properties.clone())).ok(),
        })?;
        Ok(HandleRowOutcome::Created)
    }
}

fn build_failure_row(
    job_id: &str,
    row_index: usize,
    error_code: Option<String>,
    error_message: Option<String>,
    payload_json: Option<String>,
) -> ImportJobRowRecord {
    ImportJobRowRecord {
        job_id: job_id.to_string(),
        row_index,
        status: ImportJobRowStatus::Failed,
        error_code,
        error_message,
        error_payload_json: payload_json,
        conflict_type: None,
        previous_snapshot_json: None,
    }
}

fn compute_batch_hash(batch: &[Value]) -> Option<String> {
    if batch.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for item in batch {
        if let Ok(bytes) = serde_json::to_vec(item) {
            hasher.update(bytes);
        }
    }
    Some(hex::encode(hasher.finalize()))
}

fn build_conflict_row(
    job_id: &str,
    row_index: usize,
    status: ImportJobRowStatus,
    strategy: UpsertStrategy,
    previous_properties: &Map<String, Value>,
    conflict_columns: &[String],
) -> ImportJobRowRecord {
    let snapshot_json = serialize_conflict_snapshot(previous_properties, conflict_columns);
    ImportJobRowRecord {
        job_id: job_id.to_string(),
        row_index,
        status,
        error_code: None,
        error_message: None,
        error_payload_json: None,
        conflict_type: Some(upsert_strategy_label(&strategy).into()),
        previous_snapshot_json: snapshot_json,
    }
}

fn serialize_conflict_snapshot(
    previous_properties: &Map<String, Value>,
    conflict_columns: &[String],
) -> Option<String> {
    if previous_properties.is_empty() {
        return None;
    }
    let snapshot = if conflict_columns.is_empty() {
        Value::Object(previous_properties.clone())
    } else {
        let mut subset = Map::new();
        for column in conflict_columns {
            if let Some(value) = previous_properties.get(column) {
                subset.insert(column.clone(), value.clone());
            }
        }
        Value::Object(subset)
    };
    serde_json::to_string(&snapshot).ok()
}

fn upsert_strategy_label(strategy: &UpsertStrategy) -> &'static str {
    match strategy {
        UpsertStrategy::Skip => "skip",
        UpsertStrategy::Overwrite => "overwrite",
        UpsertStrategy::Merge => "merge",
    }
}

fn error_kind_code(kind: NotionApiErrorKind) -> &'static str {
    match kind {
        NotionApiErrorKind::RateLimited => "rate_limited",
        NotionApiErrorKind::Temporary => "temporary",
        NotionApiErrorKind::Validation => "validation",
        NotionApiErrorKind::Unauthorized => "unauthorized",
        NotionApiErrorKind::NotFound => "not_found",
        NotionApiErrorKind::Conflict => "conflict",
        NotionApiErrorKind::Other => "error",
    }
}

fn persist_progress(ctx: &WorkerContext, stats: BatchProgress) -> Result<(), String> {
    let update = ProgressUpdate {
        total: None,
        done: stats.success,
        failed: stats.failed,
        skipped: stats.skipped,
        conflicts: stats.conflicts,
        conflict_total: None,
        next_offset: Some(stats.next_offset),
        rps: stats.rps,
        last_error: stats.last_error.clone(),
        heartbeat_at: Some(now_ms()),
    };
    ctx.job_store.update_progress(&ctx.job_id, update)?;
    ctx.job_runner.update_progress(
        &ctx.job_id,
        JobProgress {
            total: None,
            done: stats.success,
            failed: stats.failed,
            skipped: stats.skipped,
            conflict_total: Some(stats.conflicts),
        },
    );
    Ok(())
}

fn finalize_success(ctx: &WorkerContext, total_processed: usize) {
    ctx.job_runner.emit_log(
        &ctx.job_id,
        JobLogLevel::Info,
        format!("import completed successfully ({} rows)", total_processed),
    );
    let _ = ctx.job_store.mark_state(
        &ctx.job_id,
        StateTransition {
            state: JobState::Completed,
            started_at: ctx.record.started_at.or_else(|| Some(now_ms())),
            ended_at: Some(now_ms()),
            last_error: None,
        },
    );
    ctx.job_runner.set_state(&ctx.job_id, JobState::Completed);
}

fn mark_failed(ctx: &WorkerContext, message: String) {
    ctx.job_runner
        .emit_log(&ctx.job_id, JobLogLevel::Error, message.clone());
    let _ = ctx.job_store.mark_state(
        &ctx.job_id,
        StateTransition {
            state: JobState::Failed,
            started_at: ctx.record.started_at.or_else(|| Some(now_ms())),
            ended_at: Some(now_ms()),
            last_error: Some(message.clone()),
        },
    );
    ctx.job_runner.update_progress(
        &ctx.job_id,
        JobProgress {
            total: None,
            done: 0,
            failed: 0,
            skipped: 0,
            conflict_total: None,
        },
    );
    ctx.job_runner.set_state(&ctx.job_id, JobState::Failed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notion::adapter::{
        CreatePageRequest, CreatePageResponse, LookupProperty, MockNotionAdapter, NotionAdapter,
        NotionApiError, NotionApiErrorKind, PageSnapshot,
    };
    use crate::notion::mapping::build_property_entry;
    use crate::notion::storage::{
        ImportJobRowStatus, ImportJobStore, InMemoryJobStore, NewImportJob,
    };
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tempfile::{Builder, NamedTempFile};

    fn create_engine(
        adapter: Arc<dyn NotionAdapter>,
        job_store: Arc<dyn ImportJobStore>,
        job_runner: Arc<JobRunner>,
    ) -> ImportEngine {
        ImportEngine::new(adapter, job_store, job_runner)
    }

    fn write_json_records(records: &[serde_json::Value]) -> NamedTempFile {
        let mut file = Builder::new()
            .suffix(".json")
            .tempfile()
            .expect("create temp file");
        serde_json::to_writer(file.as_file_mut(), records).expect("write json");
        file
    }

    fn build_upsert_snapshot(
        path: &std::path::Path,
        token_id: &str,
        database_id: &str,
        strategy: &str,
        include_score: bool,
    ) -> String {
        let mut mappings = vec![
            json!({
                "include": true,
                "sourceField": "slug",
                "targetProperty": "Slug",
                "targetType": "rich_text"
            }),
            json!({
                "include": true,
                "sourceField": "title",
                "targetProperty": "Name",
                "targetType": "title"
            }),
        ];
        if include_score {
            mappings.push(json!({
                "include": true,
                "sourceField": "score",
                "targetProperty": "Score",
                "targetType": "number"
            }));
        }
        json!({
            "version": 1,
            "tokenId": token_id,
            "databaseId": database_id,
            "sourceFilePath": path.to_string_lossy(),
            "fileType": "json",
            "mappings": mappings,
            "defaults": null,
            "rateLimit": null,
            "batchSize": 2,
            "upsert": {
                "dedupeKey": "Slug",
                "strategy": strategy,
                "conflictColumns": []
            }
        })
        .to_string()
    }

    #[test]
    fn import_engine_persists_checkpoints() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter::new());
        let engine = create_engine(
            Arc::clone(&adapter),
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let records = vec![
            json!({"name": "Row1"}),
            json!({"name": "Row2"}),
            json!({"name": "Row3"}),
        ];
        let file = write_json_records(&records);

        let job_id = "job-checkpoint".to_string();
        let snapshot = json!({
            "version": 1,
            "tokenId": "tok-ckpt",
            "databaseId": "db-ckpt",
            "sourceFilePath": file.path().to_string_lossy(),
            "fileType": "json",
            "mappings": [{
                "include": true,
                "sourceField": "name",
                "targetProperty": "Name",
                "targetType": "title"
            }],
            "defaults": null,
            "rateLimit": null,
            "batchSize": 1,
        })
        .to_string();

        job_store
            .insert_job(NewImportJob {
                id: job_id.clone(),
                token_id: "tok-ckpt".into(),
                database_id: "db-ckpt".into(),
                source_file_path: file.path().to_string_lossy().into(),
                config_snapshot_json: snapshot,
                total: Some(records.len()),
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");

        job_runner.register_job(job_id.clone());
        job_runner.mark_running(&job_id);

        let handle = engine
            .spawn_job(StartContext {
                job_id: job_id.clone(),
                token: Some("secret-token".into()),
            })
            .expect("spawn job");
        handle.join();

        let checkpoints = job_store
            .recent_checkpoints(&job_id, 10)
            .expect("query checkpoints");
        assert!(
            !checkpoints.is_empty(),
            "expected at least one checkpoint to be recorded"
        );
        let latest = &checkpoints[0];
        assert_eq!(latest.row_index, records.len());
    }

    fn insert_job(
        job_store: &Arc<dyn ImportJobStore>,
        job_id: &str,
        token_id: &str,
        database_id: &str,
        source_file_path: &str,
        config_snapshot_json: String,
        total: usize,
    ) {
        job_store
            .insert_job(NewImportJob {
                id: job_id.to_string(),
                token_id: token_id.to_string(),
                database_id: database_id.to_string(),
                source_file_path: source_file_path.to_string(),
                config_snapshot_json,
                total: Some(total),
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");
    }

    #[test]
    fn worker_processes_entire_file() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter = Arc::new(RecordingAdapter::default());
        let engine = create_engine(
            adapter.clone() as Arc<dyn NotionAdapter>,
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let records = vec![
            json!({"name": "A"}),
            json!({"name": "B"}),
            json!({"name": "C"}),
        ];
        let file = write_json_records(&records);

        let job_id = "job-json".to_string();
        let snapshot = json!({
            "version": 1,
            "tokenId": "tok-1",
            "databaseId": "db-1",
            "sourceFilePath": file.path().to_string_lossy(),
            "fileType": "json",
            "mappings": [{
                "include": true,
                "sourceField": "name",
                "targetProperty": "Name",
                "targetType": "title"
            }],
            "defaults": null,
            "rateLimit": null,
            "batchSize": 2,
        })
        .to_string();

        job_store
            .insert_job(NewImportJob {
                id: job_id.clone(),
                token_id: "tok-1".into(),
                database_id: "db-1".into(),
                source_file_path: file.path().to_string_lossy().into(),
                config_snapshot_json: snapshot,
                total: Some(records.len()),
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");

        job_runner.register_job(job_id.clone());
        job_runner.mark_running(&job_id);

        let handle = engine
            .spawn_job(StartContext {
                job_id: job_id.clone(),
                token: Some("secret".into()),
            })
            .expect("spawn job");

        handle.join();

        let record = job_store
            .load_job(&job_id)
            .expect("load job")
            .expect("job record");
        assert_eq!(record.progress.done, records.len());
        assert_eq!(record.progress.failed, 0);
        assert_eq!(record.state, JobState::Completed);
        assert_eq!(record.next_offset, records.len());

        let calls = adapter.take_calls();
        assert_eq!(calls.len(), records.len());
        let expected_entry = build_property_entry(
            &FieldMapping {
                include: true,
                source_field: "name".into(),
                target_property: "Name".into(),
                target_type: "title".into(),
                transform_code: None,
            },
            &json!("A"),
        )
        .unwrap();
        assert_eq!(calls[0].properties.get("Name").unwrap(), &expected_entry);
    }

    #[test]
    fn worker_respects_pause_and_resume() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter = Arc::new(BlockingAdapter::default());
        let engine = create_engine(
            adapter.clone() as Arc<dyn NotionAdapter>,
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let records = vec![
            json!({"name": "A"}),
            json!({"name": "B"}),
            json!({"name": "C"}),
            json!({"name": "D"}),
        ];
        let file = write_json_records(&records);

        let job_id = "job-pause".to_string();
        let snapshot = json!({
            "version": 1,
            "tokenId": "tok-1",
            "databaseId": "db-1",
            "sourceFilePath": file.path().to_string_lossy(),
            "fileType": "json",
            "mappings": [{
                "include": true,
                "sourceField": "name",
                "targetProperty": "Name",
                "targetType": "title"
            }],
            "defaults": null,
            "rateLimit": null,
            "batchSize": 1,
        })
        .to_string();

        job_store
            .insert_job(NewImportJob {
                id: job_id.clone(),
                token_id: "tok-1".into(),
                database_id: "db-1".into(),
                source_file_path: file.path().to_string_lossy().into(),
                config_snapshot_json: snapshot,
                total: Some(records.len()),
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");

        job_runner.register_job(job_id.clone());
        job_runner.mark_running(&job_id);

        let handle = engine
            .spawn_job(StartContext {
                job_id: job_id.clone(),
                token: Some("secret".into()),
            })
            .expect("spawn job");

        adapter.wait_for_calls(1, Duration::from_millis(200));
        let snapshot_before_pause = job_store.load_job(&job_id).expect("load").expect("record");
        assert!(
            snapshot_before_pause.progress.done > 0,
            "expected progress before pause"
        );

        job_runner.pause(&job_id);

        std::thread::sleep(Duration::from_millis(50));
        let paused_progress = job_store
            .load_job(&job_id)
            .expect("load paused")
            .expect("record paused")
            .progress
            .done;

        std::thread::sleep(Duration::from_millis(120));
        let paused_again = job_store
            .load_job(&job_id)
            .expect("load paused again")
            .expect("record paused again")
            .progress
            .done;
        assert_eq!(paused_progress, paused_again);

        job_runner.resume(&job_id);
        handle.join();

        let final_record = job_store
            .load_job(&job_id)
            .expect("load2")
            .expect("record2");
        assert_eq!(final_record.progress.done, records.len());
        assert_eq!(final_record.state, JobState::Completed);
        assert_eq!(adapter.total_calls(), records.len());
    }

    #[test]
    fn worker_records_failures_and_marks_row_status() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter = Arc::new(FailingAdapter::new(vec![
            Ok(()),
            Err(NotionApiError {
                kind: NotionApiErrorKind::Validation,
                message: "invalid property".into(),
                status: Some(400),
                code: Some("validation_error".into()),
                retry_after_ms: None,
            }),
        ]));
        let engine = create_engine(
            adapter.clone() as Arc<dyn NotionAdapter>,
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let records = vec![json!({"name": "A"}), json!({"name": "B"})];
        let file = write_json_records(&records);

        let job_id = "job-fail".to_string();
        let snapshot = json!({
            "version": 1,
            "tokenId": "tok-1",
            "databaseId": "db-1",
            "sourceFilePath": file.path().to_string_lossy(),
            "fileType": "json",
            "mappings": [{
                "include": true,
                "sourceField": "name",
                "targetProperty": "Name",
                "targetType": "title"
            }],
            "defaults": null,
            "rateLimit": null,
            "batchSize": 1,
        })
        .to_string();

        job_store
            .insert_job(NewImportJob {
                id: job_id.clone(),
                token_id: "tok-1".into(),
                database_id: "db-1".into(),
                source_file_path: file.path().to_string_lossy().into(),
                config_snapshot_json: snapshot,
                total: Some(records.len()),
                created_at: now_ms(),
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            })
            .expect("insert job");

        job_runner.register_job(job_id.clone());
        job_runner.mark_running(&job_id);

        let handle = engine
            .spawn_job(StartContext {
                job_id: job_id.clone(),
                token: Some("secret".into()),
            })
            .expect("spawn job");
        handle.join();

        let record = job_store.load_job(&job_id).expect("load").expect("record");
        assert_eq!(record.progress.done, 1);
        assert_eq!(record.progress.failed, 1);
        assert_eq!(record.state, JobState::Completed);

        let failures = job_store
            .list_recent_failures(&job_id, 10)
            .expect("list failures");
        assert_eq!(failures.len(), 1);
        let failure = &failures[0];
        assert_eq!(failure.row_index, 1);
        assert_eq!(failure.status, ImportJobRowStatus::Failed);
        assert_eq!(failure.error_code.as_deref(), Some("validation_error"));
        assert!(failure
            .error_message
            .as_ref()
            .is_some_and(|msg| msg.contains("invalid property")));
    }

    #[test]
    fn upsert_skip_avoids_duplicate_pages() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let mock = Arc::new(MockNotionAdapter::new());
        let engine = create_engine(
            mock.clone() as Arc<dyn NotionAdapter>,
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let token_id = "tok-upsert";
        let database_id = "db-upsert";

        let initial_records = vec![
            json!({"slug": "alpha", "title": "Alpha"}),
            json!({"slug": "beta", "title": "Beta"}),
        ];
        let file = write_json_records(&initial_records);

        let snapshot = build_upsert_snapshot(file.path(), token_id, database_id, "skip", false);

        insert_job(
            &job_store,
            "job-upsert-1",
            token_id,
            database_id,
            &file.path().to_string_lossy(),
            snapshot.clone(),
            initial_records.len(),
        );
        job_runner.register_job("job-upsert-1");
        job_runner.mark_running("job-upsert-1");
        let first = engine
            .spawn_job(StartContext {
                job_id: "job-upsert-1".into(),
                token: Some("secret".into()),
            })
            .expect("spawn first");
        first.join();

        let pages_after_first = mock.dump_database(database_id);
        assert_eq!(pages_after_first.len(), initial_records.len());

        insert_job(
            &job_store,
            "job-upsert-2",
            token_id,
            database_id,
            &file.path().to_string_lossy(),
            snapshot,
            initial_records.len(),
        );
        job_runner.register_job("job-upsert-2");
        job_runner.mark_running("job-upsert-2");
        let second = engine
            .spawn_job(StartContext {
                job_id: "job-upsert-2".into(),
                token: Some("secret".into()),
            })
            .expect("spawn second");
        second.join();

        let pages_after_second = mock.dump_database(database_id);
        assert_eq!(
            pages_after_second.len(),
            initial_records.len(),
            "skip strategy should not create duplicate pages"
        );
    }

    #[test]
    fn upsert_overwrite_updates_existing_properties() {
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let mock = Arc::new(MockNotionAdapter::new());
        let engine = create_engine(
            mock.clone() as Arc<dyn NotionAdapter>,
            Arc::clone(&job_store),
            Arc::clone(&job_runner),
        );

        let token_id = "tok-upsert";
        let database_id = "db-upsert";

        let initial_records = vec![
            json!({"slug": "alpha", "title": "Alpha", "score": 10}),
            json!({"slug": "beta", "title": "Beta", "score": 20}),
        ];
        let file = write_json_records(&initial_records);

        let snapshot = build_upsert_snapshot(file.path(), token_id, database_id, "overwrite", true);

        insert_job(
            &job_store,
            "job-upsert-3",
            token_id,
            database_id,
            &file.path().to_string_lossy(),
            snapshot.clone(),
            initial_records.len(),
        );
        job_runner.register_job("job-upsert-3");
        job_runner.mark_running("job-upsert-3");
        let first = engine
            .spawn_job(StartContext {
                job_id: "job-upsert-3".into(),
                token: Some("secret".into()),
            })
            .expect("spawn overwrite first");
        first.join();

        let updated_records = vec![
            json!({"slug": "alpha", "title": "Alpha v2", "score": 42}),
            json!({"slug": "beta", "title": "Beta v2", "score": 84}),
        ];
        let serialized = serde_json::to_vec(&updated_records).expect("serialize updated records");
        fs::write(file.path(), serialized).expect("overwrite source file");

        insert_job(
            &job_store,
            "job-upsert-4",
            token_id,
            database_id,
            &file.path().to_string_lossy(),
            snapshot,
            updated_records.len(),
        );
        job_runner.register_job("job-upsert-4");
        job_runner.mark_running("job-upsert-4");
        let second = engine
            .spawn_job(StartContext {
                job_id: "job-upsert-4".into(),
                token: Some("secret".into()),
            })
            .expect("spawn overwrite second");
        second.join();

        let pages_after_update = mock.dump_database(database_id);
        assert_eq!(pages_after_update.len(), updated_records.len());

        for (page, expected) in pages_after_update.into_iter().zip(updated_records.iter()) {
            let score_entry = page
                .properties
                .get("Score")
                .and_then(|entry| entry.get("number"))
                .and_then(|value| value.as_f64())
                .unwrap_or_default();
            assert!(
                (score_entry - expected.get("score").and_then(|v| v.as_f64()).unwrap()).abs()
                    < f64::EPSILON,
                "expected score property to be updated"
            );

            let title_entry = page
                .properties
                .get("Name")
                .and_then(|entry| entry.get("title"))
                .and_then(|arr| arr.as_array())
                .and_then(|arr| arr.first())
                .and_then(|fragment| fragment.get("text"))
                .and_then(|text| text.get("content"))
                .and_then(|content| content.as_str())
                .unwrap_or("");
            assert_eq!(
                title_entry,
                expected.get("title").and_then(|v| v.as_str()).unwrap(),
                "expected name property to be updated"
            );
        }
    }

    #[derive(Default)]
    struct RecordingAdapter {
        calls: Mutex<Vec<CreatePageRequest>>,
    }

    impl RecordingAdapter {
        fn take_calls(&self) -> Vec<CreatePageRequest> {
            self.calls.lock().expect("lock").drain(..).collect()
        }
    }

    impl NotionAdapter for RecordingAdapter {
        fn test_connection(
            &self,
            _token: &str,
        ) -> Result<crate::notion::types::WorkspaceInfo, String> {
            Ok(crate::notion::types::WorkspaceInfo {
                workspace_name: Some("workspace".into()),
                bot_name: Some("bot".into()),
            })
        }

        fn search_databases(
            &self,
            _token: &str,
            _query: Option<String>,
        ) -> Result<Vec<crate::notion::types::DatabaseBrief>, String> {
            Ok(Vec::new())
        }

        fn search_databases_page(
            &self,
            _token: &str,
            _query: Option<String>,
            _cursor: Option<String>,
            _page_size: Option<u32>,
        ) -> Result<crate::notion::types::DatabasePage, String> {
            Ok(crate::notion::types::DatabasePage {
                results: Vec::new(),
                has_more: false,
                next_cursor: None,
            })
        }

        fn get_database_schema(
            &self,
            _token: &str,
            database_id: &str,
        ) -> Result<crate::notion::types::DatabaseSchema, String> {
            Ok(crate::notion::types::DatabaseSchema {
                id: database_id.into(),
                title: database_id.into(),
                properties: Vec::new(),
            })
        }

        fn create_page(
            &self,
            _token: &str,
            request: CreatePageRequest,
        ) -> Result<CreatePageResponse, NotionApiError> {
            self.calls.lock().expect("lock").push(request);
            Ok(CreatePageResponse { page_id: None })
        }

        fn lookup_page(
            &self,
            _token: &str,
            _database_id: &str,
            _properties: &[LookupProperty],
        ) -> Result<Option<PageSnapshot>, NotionApiError> {
            Ok(None)
        }

        fn update_page(
            &self,
            _token: &str,
            _page_id: &str,
            _properties: Map<String, Value>,
        ) -> Result<(), NotionApiError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct BlockingAdapter {
        calls: Mutex<usize>,
    }

    impl BlockingAdapter {
        fn wait_for_calls(&self, expected: usize, timeout: Duration) {
            let start = Instant::now();
            while Instant::now().duration_since(start) < timeout {
                if *self.calls.lock().expect("lock") >= expected {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timeout waiting for calls");
        }

        fn total_calls(&self) -> usize {
            *self.calls.lock().expect("lock")
        }
    }

    impl NotionAdapter for BlockingAdapter {
        fn test_connection(
            &self,
            _token: &str,
        ) -> Result<crate::notion::types::WorkspaceInfo, String> {
            Ok(crate::notion::types::WorkspaceInfo {
                workspace_name: None,
                bot_name: None,
            })
        }

        fn search_databases(
            &self,
            _token: &str,
            _query: Option<String>,
        ) -> Result<Vec<crate::notion::types::DatabaseBrief>, String> {
            Ok(Vec::new())
        }

        fn search_databases_page(
            &self,
            _token: &str,
            _query: Option<String>,
            _cursor: Option<String>,
            _page_size: Option<u32>,
        ) -> Result<crate::notion::types::DatabasePage, String> {
            Ok(crate::notion::types::DatabasePage {
                results: Vec::new(),
                has_more: false,
                next_cursor: None,
            })
        }

        fn get_database_schema(
            &self,
            _token: &str,
            database_id: &str,
        ) -> Result<crate::notion::types::DatabaseSchema, String> {
            Ok(crate::notion::types::DatabaseSchema {
                id: database_id.into(),
                title: database_id.into(),
                properties: Vec::new(),
            })
        }

        fn create_page(
            &self,
            _token: &str,
            request: CreatePageRequest,
        ) -> Result<CreatePageResponse, NotionApiError> {
            std::thread::sleep(Duration::from_millis(30));
            *self.calls.lock().expect("lock") += 1;
            Ok(CreatePageResponse {
                page_id: request
                    .properties
                    .get("Name")
                    .and_then(|entry| entry.get("title"))
                    .and_then(|arr| arr.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|fragment| fragment.get("text"))
                    .and_then(|text| text.get("content"))
                    .and_then(|content| content.as_str())
                    .map(|s| s.to_string()),
            })
        }

        fn lookup_page(
            &self,
            _token: &str,
            _database_id: &str,
            _properties: &[LookupProperty],
        ) -> Result<Option<PageSnapshot>, NotionApiError> {
            Ok(None)
        }

        fn update_page(
            &self,
            _token: &str,
            _page_id: &str,
            _properties: Map<String, Value>,
        ) -> Result<(), NotionApiError> {
            Ok(())
        }
    }

    struct FailingAdapter {
        outcomes: Mutex<Vec<Result<(), NotionApiError>>>,
    }

    impl FailingAdapter {
        fn new(outcomes: Vec<Result<(), NotionApiError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes),
            }
        }
    }

    impl NotionAdapter for FailingAdapter {
        fn test_connection(
            &self,
            _token: &str,
        ) -> Result<crate::notion::types::WorkspaceInfo, String> {
            Ok(crate::notion::types::WorkspaceInfo {
                workspace_name: None,
                bot_name: None,
            })
        }

        fn search_databases(
            &self,
            _token: &str,
            _query: Option<String>,
        ) -> Result<Vec<crate::notion::types::DatabaseBrief>, String> {
            Ok(Vec::new())
        }

        fn search_databases_page(
            &self,
            _token: &str,
            _query: Option<String>,
            _cursor: Option<String>,
            _page_size: Option<u32>,
        ) -> Result<crate::notion::types::DatabasePage, String> {
            Ok(crate::notion::types::DatabasePage {
                results: Vec::new(),
                has_more: false,
                next_cursor: None,
            })
        }

        fn get_database_schema(
            &self,
            _token: &str,
            database_id: &str,
        ) -> Result<crate::notion::types::DatabaseSchema, String> {
            Ok(crate::notion::types::DatabaseSchema {
                id: database_id.into(),
                title: database_id.into(),
                properties: Vec::new(),
            })
        }

        fn create_page(
            &self,
            _token: &str,
            request: CreatePageRequest,
        ) -> Result<CreatePageResponse, NotionApiError> {
            let mut guard = self.outcomes.lock().expect("lock");
            let outcome = guard.remove(0);
            match outcome {
                Ok(()) => Ok(CreatePageResponse {
                    page_id: request
                        .properties
                        .get("Name")
                        .and_then(|entry| entry.get("title"))
                        .and_then(|arr| arr.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|fragment| fragment.get("text"))
                        .and_then(|text| text.get("content"))
                        .and_then(|content| content.as_str())
                        .map(|s| s.to_string()),
                }),
                Err(err) => Err(err),
            }
        }

        fn lookup_page(
            &self,
            _token: &str,
            _database_id: &str,
            _properties: &[LookupProperty],
        ) -> Result<Option<PageSnapshot>, NotionApiError> {
            Ok(None)
        }

        fn update_page(
            &self,
            _token: &str,
            _page_id: &str,
            _properties: Map<String, Value>,
        ) -> Result<(), NotionApiError> {
            Ok(())
        }
    }
}
