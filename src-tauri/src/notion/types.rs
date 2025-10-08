use super::job_runner::{JobProgress, JobState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRow {
    pub id: String,
    pub name: String,
    pub workspace_name: Option<String>,
    pub created_at: i64, // unix ms
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub workspace_name: Option<String>,
    pub bot_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBrief {
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabasePage {
    pub results: Vec<DatabaseBrief>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTokenRequest {
    pub name: String,
    pub token: String,
}

// -----------------------------
// M2: Database schema & templates & mappings
// -----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseProperty {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub required: Option<bool>,
    pub options: Option<Vec<String>>, // For select/multi_select/status etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSchema {
    pub id: String,
    pub title: String,
    pub properties: Vec<DatabaseProperty>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMapping {
    pub include: bool,
    pub source_field: String,
    pub target_property: String,
    pub target_type: String,
    pub transform_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpsertStrategy {
    Skip,
    Overwrite,
    Merge,
}

fn default_upsert_strategy() -> UpsertStrategy {
    UpsertStrategy::Skip
}

mod dedupe_key_format {
    use serde::{de, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(key) => serializer.serialize_str(key),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<Value>::deserialize(deserializer)?;
        match value {
            Some(Value::String(s)) => Ok(Some(s)),
            Some(Value::Array(arr)) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        return Ok(Some(s.to_string()));
                    }
                }
                Ok(None)
            }
            Some(Value::Null) | None => Ok(None),
            Some(other) => Err(de::Error::custom(format!(
                "dedupeKey must be string or array of strings, got {}",
                other
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportUpsertConfig {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "dedupe_key_format::deserialize",
        serialize_with = "dedupe_key_format::serialize"
    )]
    pub dedupe_key: Option<String>,
    #[serde(default = "default_upsert_strategy")]
    pub strategy: UpsertStrategy,
    #[serde(default)]
    pub conflict_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTemplate {
    pub id: Option<String>,
    pub name: String,
    pub token_id: String,
    pub database_id: String,
    pub mappings: Vec<FieldMapping>,
    pub defaults: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunInput {
    pub schema: DatabaseSchema,
    pub mappings: Vec<FieldMapping>,
    pub records: Vec<Value>, // sample records (array of objects)
    #[serde(default)]
    pub defaults: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowError {
    pub row_index: usize,
    pub message: String,
    pub kind: DryRunErrorKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DryRunErrorKind {
    Transform,
    Mapping,
    Validation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReport {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub errors: Vec<RowError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformEvalRequest {
    pub code: String,
    pub value: Value,
    pub record: Value,
    pub row_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformEvalResult {
    pub result: Value,
}

// -----------------------------
// M3: Import job commands
// -----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobRequest {
    #[serde(default)]
    pub job_id: Option<String>,
    pub token_id: String,
    pub database_id: String,
    pub source_file_path: String,
    pub file_type: String,
    pub mappings: Vec<FieldMapping>,
    #[serde(default)]
    pub defaults: Option<Value>,
    #[serde(default)]
    pub rate_limit: Option<u32>,
    #[serde(default)]
    pub batch_size: Option<usize>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub upsert: Option<ImportUpsertConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobHandle {
    pub job_id: String,
    pub state: JobState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobSummary {
    pub job_id: String,
    pub state: JobState,
    pub progress: JobProgress,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub lease_expires_at: Option<i64>,
    #[serde(default)]
    pub token_id: Option<String>,
    #[serde(default)]
    pub database_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub rps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    Skip,
    Overwrite,
    Merge,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowErrorSummary {
    pub row_index: usize,
    pub error_code: Option<String>,
    pub error_message: String,
    #[serde(default)]
    pub conflict_type: Option<ConflictType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgressEvent {
    pub job_id: String,
    pub state: JobState,
    pub progress: JobProgress,
    pub rps: Option<f64>,
    pub recent_errors: Vec<RowErrorSummary>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub lease_expires_at: Option<i64>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportLogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportLogEvent {
    pub job_id: String,
    pub level: ImportLogLevel,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDoneEvent {
    pub job_id: String,
    pub state: JobState,
    pub progress: JobProgress,
    pub rps: Option<f64>,
    pub finished_at: i64,
    pub last_error: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub conflict_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFailedResult {
    pub job_id: String,
    pub path: String,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportQueueSnapshot {
    pub running: Vec<ImportJobSummary>,
    pub waiting: Vec<ImportJobSummary>,
    pub paused: Vec<ImportJobSummary>,
    pub timestamp: i64,
}
