use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRow {
    pub id: String,
    pub name: String,
    pub workspace_name: Option<String>,
    pub created_at: i64,   // unix ms
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowError {
    pub row_index: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReport {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub errors: Vec<RowError>,
}
