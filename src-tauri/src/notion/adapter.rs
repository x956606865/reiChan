use super::types::{DatabaseBrief, DatabasePage, DatabaseProperty, DatabaseSchema, WorkspaceInfo};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct CreatePageRequest {
    pub database_id: String,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct CreatePageResponse {
    pub page_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LookupProperty {
    pub name: String,
    pub property: Value,
}

#[derive(Debug, Clone)]
pub struct PageSnapshot {
    pub page_id: String,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotionApiErrorKind {
    RateLimited,
    Temporary,
    Validation,
    Unauthorized,
    NotFound,
    Conflict,
    Other,
}

impl NotionApiErrorKind {
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::Temporary)
    }
}

#[derive(Debug, Clone)]
pub struct NotionApiError {
    pub kind: NotionApiErrorKind,
    pub message: String,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub retry_after_ms: Option<u64>,
}

pub trait NotionAdapter: Send + Sync {
    fn test_connection(&self, token: &str) -> Result<WorkspaceInfo, String>;
    fn search_databases(
        &self,
        token: &str,
        query: Option<String>,
    ) -> Result<Vec<DatabaseBrief>, String>;
    fn search_databases_page(
        &self,
        token: &str,
        query: Option<String>,
        start_cursor: Option<String>,
        page_size: Option<u32>,
    ) -> Result<DatabasePage, String>;
    fn get_database_schema(&self, token: &str, database_id: &str)
        -> Result<DatabaseSchema, String>;
    fn create_page(
        &self,
        token: &str,
        request: CreatePageRequest,
    ) -> Result<CreatePageResponse, NotionApiError>;
    fn lookup_page(
        &self,
        token: &str,
        database_id: &str,
        properties: &[LookupProperty],
    ) -> Result<Option<PageSnapshot>, NotionApiError>;
    fn update_page(
        &self,
        token: &str,
        page_id: &str,
        properties: Map<String, Value>,
    ) -> Result<(), NotionApiError>;
}

/// A placeholder adapter that does not perform network calls.
/// It allows wiring the UI and commands without requiring network or credentials.
#[derive(Clone, Default)]
pub struct MockNotionAdapter {
    state: Arc<Mutex<MockNotionState>>,
}

#[derive(Default)]
struct MockNotionState {
    seq: u64,
    databases: HashMap<String, Vec<MockPage>>,
}

#[derive(Clone)]
pub struct MockPage {
    pub id: String,
    pub properties: Map<String, Value>,
}

impl MockNotionAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dump_database(&self, database_id: &str) -> Vec<MockPage> {
        let guard = self.state.lock().expect("mock notion adapter poisoned");
        guard
            .databases
            .get(database_id)
            .cloned()
            .unwrap_or_default()
    }

    fn next_page_id(state: &mut MockNotionState) -> String {
        state.seq += 1;
        format!("mock_page_{}", state.seq)
    }
}

impl NotionAdapter for MockNotionAdapter {
    fn test_connection(&self, _token: &str) -> Result<WorkspaceInfo, String> {
        Ok(WorkspaceInfo {
            workspace_name: Some("Offline Workspace".into()),
            bot_name: Some("Mock Bot".into()),
        })
    }

    fn search_databases(
        &self,
        _token: &str,
        query: Option<String>,
    ) -> Result<Vec<DatabaseBrief>, String> {
        let q = query.unwrap_or_default();
        // Return first page results for compatibility.
        Ok(self
            .search_databases_page("", Some(q), None, Some(10))?
            .results)
    }

    fn search_databases_page(
        &self,
        _token: &str,
        query: Option<String>,
        start_cursor: Option<String>,
        page_size: Option<u32>,
    ) -> Result<DatabasePage, String> {
        let q = query.unwrap_or_default();
        // Simulate a long list of databases to exercise pagination.
        let total = 42usize;
        let mut all: Vec<DatabaseBrief> = (1..=total)
            .map(|i| {
                let title = if i % 7 == 0 {
                    String::new()
                } else {
                    format!("Sample DB {:02} {}", i, q)
                };
                DatabaseBrief {
                    id: format!("db_mock_{:02}", i),
                    title,
                    icon: Some(
                        if i % 3 == 0 {
                            "📒"
                        } else if i % 3 == 1 {
                            "✅"
                        } else {
                            "📝"
                        }
                        .into(),
                    ),
                }
            })
            .collect();
        // Apply a naive filter on `q`.
        if !q.is_empty() {
            all.retain(|d| d.title.to_lowercase().contains(&q.to_lowercase()));
        }
        let page_size = page_size.unwrap_or(10).max(1) as usize;
        let start = start_cursor
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0)
            .min(all.len());
        let end = (start + page_size).min(all.len());
        let slice = all[start..end].to_vec();
        let has_more = end < all.len();
        let next_cursor = if has_more {
            Some(end.to_string())
        } else {
            None
        };
        Ok(DatabasePage {
            results: slice,
            has_more,
            next_cursor,
        })
    }

    fn get_database_schema(
        &self,
        _token: &str,
        database_id: &str,
    ) -> Result<DatabaseSchema, String> {
        // Return a stable mock schema for development.
        let props = vec![
            DatabaseProperty {
                name: "Name".into(),
                type_: "title".into(),
                required: Some(true),
                options: None,
            },
            DatabaseProperty {
                name: "Score".into(),
                type_: "number".into(),
                required: None,
                options: None,
            },
            DatabaseProperty {
                name: "Tag".into(),
                type_: "select".into(),
                required: None,
                options: Some(vec!["A".into(), "B".into()]),
            },
            DatabaseProperty {
                name: "Tags".into(),
                type_: "multi_select".into(),
                required: None,
                options: Some(vec!["A".into(), "B".into(), "C".into()]),
            },
            DatabaseProperty {
                name: "Date".into(),
                type_: "date".into(),
                required: None,
                options: None,
            },
            DatabaseProperty {
                name: "Done".into(),
                type_: "checkbox".into(),
                required: None,
                options: None,
            },
        ];
        Ok(DatabaseSchema {
            id: database_id.into(),
            title: format!("{} (Mock)", database_id),
            properties: props,
        })
    }

    fn create_page(
        &self,
        _token: &str,
        request: CreatePageRequest,
    ) -> Result<CreatePageResponse, NotionApiError> {
        let mut guard = self.state.lock().expect("mock notion adapter poisoned");
        let page_id = Self::next_page_id(&mut guard);
        guard
            .databases
            .entry(request.database_id.clone())
            .or_default()
            .push(MockPage {
                id: page_id.clone(),
                properties: request.properties.clone(),
            });
        Ok(CreatePageResponse {
            page_id: Some(page_id),
        })
    }

    fn lookup_page(
        &self,
        _token: &str,
        database_id: &str,
        properties: &[LookupProperty],
    ) -> Result<Option<PageSnapshot>, NotionApiError> {
        let guard = self.state.lock().expect("mock notion adapter poisoned");
        let pages = guard.databases.get(database_id);
        if let Some(pages) = pages {
            for page in pages {
                let mut matched = true;
                for prop in properties {
                    let is_match = page
                        .properties
                        .get(&prop.name)
                        .map(|stored| stored == &prop.property)
                        .unwrap_or(false);
                    if !is_match {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    return Ok(Some(PageSnapshot {
                        page_id: page.id.clone(),
                        properties: page.properties.clone(),
                    }));
                }
            }
        }
        Ok(None)
    }

    fn update_page(
        &self,
        _token: &str,
        page_id: &str,
        properties: Map<String, Value>,
    ) -> Result<(), NotionApiError> {
        let mut guard = self.state.lock().expect("mock notion adapter poisoned");
        for pages in guard.databases.values_mut() {
            if let Some(page) = pages.iter_mut().find(|p| p.id == page_id) {
                page.properties = properties;
                return Ok(());
            }
        }
        Err(NotionApiError {
            kind: NotionApiErrorKind::NotFound,
            message: format!("mock page {} not found", page_id),
            status: None,
            code: None,
            retry_after_ms: None,
        })
    }
}

#[cfg(feature = "notion-http")]
pub struct HttpNotionAdapter;

#[cfg(feature = "notion-http")]
impl HttpNotionAdapter {
    fn client_with_token(_token: &str) -> reqwest::blocking::Client {
        // Use a reasonable timeout; tune as needed later.
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("build client")
    }
}

#[cfg(feature = "notion-http")]
impl NotionAdapter for HttpNotionAdapter {
    fn test_connection(&self, token: &str) -> Result<WorkspaceInfo, String> {
        let client = Self::client_with_token(token);
        let url = "https://api.notion.com/v1/users/me";
        let resp = client
            .get(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        // We don't rely on the response schema yet; just return Ok with minimal info.
        Ok(WorkspaceInfo {
            workspace_name: None,
            bot_name: None,
        })
    }

    fn search_databases(
        &self,
        token: &str,
        query: Option<String>,
    ) -> Result<Vec<DatabaseBrief>, String> {
        use serde_json::json;
        let client = Self::client_with_token(token);
        let url = "https://api.notion.com/v1/search";
        let payload = json!({
            "query": query.unwrap_or_default(),
            "filter": {"property": "object", "value": "database"}
        });
        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .json(&payload)
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
            for item in results {
                if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                    let title = item
                        .get("title")
                        .and_then(|t| t.as_array())
                        .and_then(|arr| arr.get(0))
                        .and_then(|x| x.get("plain_text"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let icon = item
                        .get("icon")
                        .and_then(|ic| ic.get("emoji").and_then(|e| e.as_str()))
                        .map(|s| s.to_string());
                    out.push(DatabaseBrief {
                        id: id.to_string(),
                        title,
                        icon,
                    });
                }
            }
        }
        Ok(out)
    }

    fn search_databases_page(
        &self,
        token: &str,
        query: Option<String>,
        start_cursor: Option<String>,
        page_size: Option<u32>,
    ) -> Result<DatabasePage, String> {
        use serde_json::json;
        let client = Self::client_with_token(token);
        let url = "https://api.notion.com/v1/search";
        let mut payload = json!({
            "query": query.clone().unwrap_or_default(),
            "filter": {"property": "object", "value": "database"},
        });
        if let Some(sz) = page_size {
            payload["page_size"] = json!(sz);
        }
        if let Some(cur) = start_cursor.clone() {
            payload["start_cursor"] = json!(cur);
        }
        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .json(&payload)
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        if let Some(items) = v.get("results").and_then(|r| r.as_array()) {
            for item in items {
                if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                    let title = item
                        .get("title")
                        .and_then(|t| t.as_array())
                        .map(|arr| {
                            // Concatenate plain_text from all title fragments to better reflect actual title
                            let mut s = String::new();
                            for frag in arr {
                                if let Some(t) = frag.get("plain_text").and_then(|x| x.as_str()) {
                                    s.push_str(t);
                                }
                            }
                            s
                        })
                        .unwrap_or_else(String::new);
                    let icon = item
                        .get("icon")
                        .and_then(|ic| ic.get("emoji").and_then(|e| e.as_str()))
                        .map(|s| s.to_string());
                    results.push(DatabaseBrief {
                        id: id.to_string(),
                        title,
                        icon,
                    });
                }
            }
        }
        let has_more = v.get("has_more").and_then(|b| b.as_bool()).unwrap_or(false);
        let next_cursor = v
            .get("next_cursor")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        Ok(DatabasePage {
            results,
            has_more,
            next_cursor,
        })
    }

    fn get_database_schema(
        &self,
        token: &str,
        database_id: &str,
    ) -> Result<DatabaseSchema, String> {
        let client = Self::client_with_token(token);
        let url = format!("https://api.notion.com/v1/databases/{}", database_id);
        let resp = client
            .get(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
        let title = v
            .get("title")
            .and_then(|arr| arr.as_array())
            .map(|arr| {
                let mut s = String::new();
                for frag in arr {
                    if let Some(t) = frag.get("plain_text").and_then(|x| x.as_str()) {
                        s.push_str(t);
                    }
                }
                s
            })
            .unwrap_or_default();
        let mut properties: Vec<DatabaseProperty> = Vec::new();
        if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
            for (name, pdef) in props {
                let t = pdef
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let options = match t.as_str() {
                    "select" => pdef
                        .get("select")
                        .and_then(|s| s.get("options"))
                        .and_then(|o| o.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| {
                                    x.get("name")
                                        .and_then(|n| n.as_str())
                                        .map(|s| s.to_string())
                                })
                                .collect()
                        }),
                    "multi_select" => pdef
                        .get("multi_select")
                        .and_then(|s| s.get("options"))
                        .and_then(|o| o.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| {
                                    x.get("name")
                                        .and_then(|n| n.as_str())
                                        .map(|s| s.to_string())
                                })
                                .collect()
                        }),
                    _ => None,
                };
                properties.push(DatabaseProperty {
                    name: name.clone(),
                    type_: t,
                    required: None,
                    options,
                });
            }
        }
        Ok(DatabaseSchema {
            id: database_id.to_string(),
            title,
            properties,
        })
    }

    fn create_page(
        &self,
        token: &str,
        request: CreatePageRequest,
    ) -> Result<CreatePageResponse, NotionApiError> {
        use serde_json::json;
        let client = Self::client_with_token(token);
        let url = "https://api.notion.com/v1/pages";
        let payload = json!({
            "parent": { "database_id": request.database_id },
            "properties": request.properties,
        });
        let response = client
            .post(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .json(&payload)
            .send()
            .map_err(|err| NotionApiError {
                kind: NotionApiErrorKind::Temporary,
                message: err.to_string(),
                status: None,
                code: None,
                retry_after_ms: None,
            })?;

        if response.status().is_success() {
            let json: serde_json::Value = response.json().unwrap_or_else(|_| serde_json::json!({}));
            // println!("[notion] create_page success: {}", json);
            let page_id = json
                .get("id")
                .and_then(|val| val.as_str())
                .map(|s| s.to_string());
            Ok(CreatePageResponse { page_id })
        } else {
            let status = response.status();
            let retry_after_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|header| header.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| seconds.saturating_mul(1000));
            let body = response.text().unwrap_or_default();
            let kind = match status.as_u16() {
                401 | 403 => NotionApiErrorKind::Unauthorized,
                404 => NotionApiErrorKind::NotFound,
                409 => NotionApiErrorKind::Conflict,
                429 => NotionApiErrorKind::RateLimited,
                code if code >= 500 => NotionApiErrorKind::Temporary,
                _ => NotionApiErrorKind::Validation,
            };
            Err(NotionApiError {
                kind,
                message: body,
                status: Some(status.as_u16()),
                code: None,
                retry_after_ms,
            })
        }
    }

    fn lookup_page(
        &self,
        _token: &str,
        _database_id: &str,
        _properties: &[LookupProperty],
    ) -> Result<Option<PageSnapshot>, NotionApiError> {
        use serde_json::json;

        if _properties.is_empty() {
            return Ok(None);
        }

        let filter = Self::build_lookup_filter(_properties).map_err(|message| NotionApiError {
            kind: NotionApiErrorKind::Validation,
            message,
            status: None,
            code: None,
            retry_after_ms: None,
        })?;

        let client = Self::client_with_token(_token);
        let url = format!("https://api.notion.com/v1/databases/{}/query", _database_id);
        let payload = json!({
            "page_size": 5,
            "filter": filter,
        });

        let response = client
            .post(url)
            .header("Authorization", format!("Bearer {}", _token))
            .header("Notion-Version", "2022-06-28")
            .json(&payload)
            .send()
            .map_err(|err| NotionApiError {
                kind: NotionApiErrorKind::Temporary,
                message: err.to_string(),
                status: None,
                code: None,
                retry_after_ms: None,
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|header| header.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| seconds.saturating_mul(1000));
            let body = response.text().unwrap_or_default();
            let kind = match status.as_u16() {
                401 | 403 => NotionApiErrorKind::Unauthorized,
                404 => NotionApiErrorKind::NotFound,
                409 => NotionApiErrorKind::Conflict,
                429 => NotionApiErrorKind::RateLimited,
                code if code >= 500 => NotionApiErrorKind::Temporary,
                _ => NotionApiErrorKind::Validation,
            };
            return Err(NotionApiError {
                kind,
                message: body,
                status: Some(status.as_u16()),
                code: None,
                retry_after_ms,
            });
        }

        let json: serde_json::Value = response.json().unwrap_or_else(|_| serde_json::json!({}));
        let first_result = json
            .get("results")
            .and_then(|val| val.as_array())
            .and_then(|arr| arr.first())
            .cloned();

        let Some(page) = first_result else {
            return Ok(None);
        };

        let page_id = match page.get("id").and_then(|id| id.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(None),
        };

        let mut snapshot_properties = Map::new();
        if let Some(props) = page.get("properties").and_then(|val| val.as_object()) {
            snapshot_properties = Self::normalize_properties(props);
        }

        // 保证至少返回查重字段，防止 Notion 响应缺少某些转换。
        for lookup in _properties {
            snapshot_properties
                .entry(lookup.name.clone())
                .or_insert_with(|| lookup.property.clone());
        }

        Ok(Some(PageSnapshot {
            page_id,
            properties: snapshot_properties,
        }))
    }

    fn update_page(
        &self,
        token: &str,
        page_id: &str,
        properties: Map<String, Value>,
    ) -> Result<(), NotionApiError> {
        use serde_json::json;
        let client = Self::client_with_token(token);
        let url = format!("https://api.notion.com/v1/pages/{}", page_id);
        let payload = json!({
            "properties": properties,
        });
        let response = client
            .patch(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Notion-Version", "2022-06-28")
            .json(&payload)
            .send()
            .map_err(|err| NotionApiError {
                kind: NotionApiErrorKind::Temporary,
                message: err.to_string(),
                status: None,
                code: None,
                retry_after_ms: None,
            })?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let retry_after_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|header| header.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| seconds.saturating_mul(1000));
            let body = response.text().unwrap_or_default();
            let kind = match status.as_u16() {
                401 | 403 => NotionApiErrorKind::Unauthorized,
                404 => NotionApiErrorKind::NotFound,
                409 => NotionApiErrorKind::Conflict,
                429 => NotionApiErrorKind::RateLimited,
                code if code >= 500 => NotionApiErrorKind::Temporary,
                _ => NotionApiErrorKind::Validation,
            };
            Err(NotionApiError {
                kind,
                message: body,
                status: Some(status.as_u16()),
                code: None,
                retry_after_ms,
            })
        }
    }
}

#[cfg(feature = "notion-http")]
impl HttpNotionAdapter {
    fn build_lookup_filter(properties: &[LookupProperty]) -> Result<serde_json::Value, String> {
        use serde_json::json;

        let mut filters = Vec::with_capacity(properties.len());
        for prop in properties {
            let value = &prop.property;
            if let Some(obj) = value.as_object() {
                if let Some(filter) = Self::filter_for_property(&prop.name, obj)? {
                    filters.push(filter);
                } else {
                    return Err(format!(
                        "唯一键字段 '{}' 当前值为空，无法构建查询。",
                        prop.name
                    ));
                }
            } else {
                return Err(format!(
                    "唯一键字段 '{}' 的值格式不受支持，需为对象类型。",
                    prop.name
                ));
            }
        }

        if filters.is_empty() {
            return Err("缺少可用于查询的唯一键字段。".into());
        }

        if filters.len() == 1 {
            Ok(filters.into_iter().next().unwrap())
        } else {
            Ok(json!({ "and": filters }))
        }
    }

    fn filter_for_property(
        name: &str,
        property: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, String> {
        use serde_json::json;

        if let Some(title) = property.get("title") {
            let content = Self::extract_plain_text(title);
            return Self::build_string_filter(name, "title", content);
        }
        if let Some(rich_text) = property.get("rich_text") {
            let content = Self::extract_plain_text(rich_text);
            return Self::build_string_filter(name, "rich_text", content);
        }
        if let Some(number) = property.get("number") {
            if number.is_null() {
                return Ok(None);
            }
            if let Some(f) = number.as_f64() {
                return Ok(Some(json!({
                    "property": name,
                    "number": { "equals": f }
                })));
            }
            return Err(format!("唯一键字段 '{}' 的 number 值无法解析。", name));
        }
        if let Some(select) = property.get("select") {
            if select.is_null() {
                return Ok(None);
            }
            if let Some(option) = select
                .as_object()
                .and_then(|obj| obj.get("name"))
                .and_then(|v| v.as_str())
            {
                return Ok(Some(json!({
                    "property": name,
                    "select": { "equals": option }
                })));
            }
            return Err(format!("唯一键字段 '{}' 的 select 值缺少 name。", name));
        }
        if let Some(status) = property.get("status") {
            if status.is_null() {
                return Ok(None);
            }
            if let Some(option) = status
                .as_object()
                .and_then(|obj| obj.get("name"))
                .and_then(|v| v.as_str())
            {
                return Ok(Some(json!({
                    "property": name,
                    "status": { "equals": option }
                })));
            }
            return Err(format!("唯一键字段 '{}' 的 status 值缺少 name。", name));
        }
        if let Some(checkbox) = property.get("checkbox") {
            if let Some(flag) = checkbox.as_bool() {
                return Ok(Some(json!({
                    "property": name,
                    "checkbox": { "equals": flag }
                })));
            }
            return Err(format!("唯一键字段 '{}' 的 checkbox 值无法解析。", name));
        }
        if let Some(date) = property.get("date") {
            if date.is_null() {
                return Ok(None);
            }
            if let Some(start) = date
                .as_object()
                .and_then(|obj| obj.get("start"))
                .and_then(|v| v.as_str())
            {
                return Ok(Some(json!({
                    "property": name,
                    "date": { "equals": start }
                })));
            }
            return Err(format!("唯一键字段 '{}' 的 date 值缺少 start。", name));
        }
        if let Some(url) = property.get("url") {
            return Self::build_simple_equals(name, "url", url);
        }
        if let Some(email) = property.get("email") {
            return Self::build_simple_equals(name, "email", email);
        }
        if let Some(phone) = property.get("phone_number") {
            return Self::build_simple_equals(name, "phone_number", phone);
        }

        if property.get("multi_select").is_some() {
            return Err(format!(
                "唯一键字段 '{}' 使用了 multi_select，目前不能用作去重键。",
                name
            ));
        }
        if property.get("people").is_some() {
            return Err(format!(
                "唯一键字段 '{}' 使用了 people，目前不能用作去重键。",
                name
            ));
        }
        if property.get("relation").is_some() {
            return Err(format!(
                "唯一键字段 '{}' 使用了 relation，目前不能用作去重键。",
                name
            ));
        }
        if property.get("files").is_some() {
            return Err(format!(
                "唯一键字段 '{}' 使用了 files，目前不能用作去重键。",
                name
            ));
        }

        Err(format!("唯一键字段 '{}' 的类型暂不支持查询。", name))
    }

    fn build_simple_equals(
        name: &str,
        filter_key: &str,
        value: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, String> {
        use serde_json::json;

        if value.is_null() {
            return Ok(None);
        }
        if let Some(s) = value.as_str() {
            if s.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(json!({
                "property": name,
                filter_key: { "equals": s }
            })));
        }
        Err(format!("唯一键字段 '{}' 的值必须是字符串。", name))
    }

    fn build_string_filter(
        name: &str,
        filter_key: &str,
        value: Option<String>,
    ) -> Result<Option<serde_json::Value>, String> {
        use serde_json::json;
        match value {
            Some(content) if !content.trim().is_empty() => Ok(Some(json!({
                "property": name,
                filter_key: { "equals": content }
            }))),
            _ => Ok(None),
        }
    }

    fn extract_plain_text(value: &serde_json::Value) -> Option<String> {
        value
            .as_array()
            .map(|arr| {
                let mut out = String::new();
                for item in arr {
                    if let Some(text) = item
                        .get("text")
                        .and_then(|t| t.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        out.push_str(text);
                        continue;
                    }
                    if let Some(plain) = item.get("plain_text").and_then(|v| v.as_str()) {
                        out.push_str(plain);
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            })
            .flatten()
    }

    fn normalize_properties(
        props: &serde_json::Map<String, serde_json::Value>,
    ) -> Map<String, Value> {
        let mut out = Map::new();
        for (name, value) in props {
            if let Some(normalized) = Self::normalize_property_value(value) {
                out.insert(name.clone(), normalized);
            }
        }
        out
    }

    fn normalize_property_value(value: &serde_json::Value) -> Option<serde_json::Value> {
        use serde_json::json;

        let obj = value.as_object()?;
        let type_name = obj.get("type")?.as_str()?;
        match type_name {
            "title" => {
                let fragments = obj.get("title")?;
                let normalized = fragments
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|fragment| {
                                if let Some(text) = fragment
                                    .get("text")
                                    .and_then(|t| t.get("content"))
                                    .and_then(|c| c.as_str())
                                {
                                    Some(json!({
                                        "type": "text",
                                        "text": { "content": text },
                                    }))
                                } else if let Some(plain) =
                                    fragment.get("plain_text").and_then(|v| v.as_str())
                                {
                                    Some(json!({
                                        "type": "text",
                                        "text": { "content": plain },
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({ "title": normalized }))
            }
            "rich_text" => {
                let fragments = obj.get("rich_text")?;
                let normalized = fragments
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|fragment| {
                                if let Some(text) = fragment
                                    .get("text")
                                    .and_then(|t| t.get("content"))
                                    .and_then(|c| c.as_str())
                                {
                                    Some(json!({
                                        "type": "text",
                                        "text": { "content": text },
                                    }))
                                } else if let Some(plain) =
                                    fragment.get("plain_text").and_then(|v| v.as_str())
                                {
                                    Some(json!({
                                        "type": "text",
                                        "text": { "content": plain },
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({ "rich_text": normalized }))
            }
            "number" => {
                Some(json!({ "number": obj.get("number").cloned().unwrap_or(Value::Null) }))
            }
            "checkbox" => Some(
                json!({ "checkbox": obj.get("checkbox").and_then(|v| v.as_bool()).unwrap_or(false) }),
            ),
            "select" => {
                let select = obj.get("select");
                match select {
                    Some(sel) if sel.is_null() => Some(json!({ "select": Value::Null })),
                    Some(sel) => sel
                        .as_object()
                        .and_then(|inner| inner.get("name"))
                        .and_then(|name| name.as_str())
                        .map(|name| json!({ "select": { "name": name } })),
                    None => None,
                }
            }
            "status" => {
                let status = obj.get("status");
                match status {
                    Some(sel) if sel.is_null() => Some(json!({ "status": Value::Null })),
                    Some(sel) => sel
                        .as_object()
                        .and_then(|inner| inner.get("name"))
                        .and_then(|name| name.as_str())
                        .map(|name| json!({ "status": { "name": name } })),
                    None => None,
                }
            }
            "date" => {
                let date_obj = obj.get("date")?.as_object()?;
                let mut payload = serde_json::Map::new();
                if let Some(start) = date_obj.get("start") {
                    payload.insert("start".into(), start.clone());
                }
                if let Some(end) = date_obj.get("end") {
                    payload.insert("end".into(), end.clone());
                }
                if let Some(tz) = date_obj.get("time_zone") {
                    payload.insert("time_zone".into(), tz.clone());
                }
                Some(json!({ "date": serde_json::Value::Object(payload) }))
            }
            "url" => Some(json!({ "url": obj.get("url").cloned().unwrap_or(Value::Null) })),
            "email" => Some(json!({ "email": obj.get("email").cloned().unwrap_or(Value::Null) })),
            "phone_number" => Some(
                json!({ "phone_number": obj.get("phone_number").cloned().unwrap_or(Value::Null) }),
            ),
            "multi_select" => {
                let entries = obj
                    .get("multi_select")
                    .and_then(|arr| arr.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                item.as_object()
                                    .and_then(|o| o.get("name"))
                                    .and_then(|v| v.as_str())
                                    .map(|name| json!({ "name": name }))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({ "multi_select": entries }))
            }
            "people" => {
                let entries = obj
                    .get("people")
                    .and_then(|arr| arr.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                                    Some(json!({ "object": "user", "id": id }))
                                } else if let Some(email) = item
                                    .get("person")
                                    .and_then(|p| p.get("email"))
                                    .and_then(|v| v.as_str())
                                {
                                    Some(json!({ "object": "user", "person": { "email": email } }))
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({ "people": entries }))
            }
            "relation" => {
                let entries = obj
                    .get("relation")
                    .and_then(|arr| arr.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                item.as_object()
                                    .and_then(|o| o.get("id"))
                                    .and_then(|v| v.as_str())
                                    .map(|id| json!({ "id": id }))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({ "relation": entries }))
            }
            "files" => Some(
                json!({ "files": obj.get("files").cloned().unwrap_or(Value::Array(Vec::new())) }),
            ),
            _ => None,
        }
    }
}
