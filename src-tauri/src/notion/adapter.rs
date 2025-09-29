use super::types::{DatabaseBrief, DatabasePage, WorkspaceInfo, DatabaseSchema, DatabaseProperty};

pub trait NotionAdapter: Send + Sync {
    fn test_connection(&self, token: &str) -> Result<WorkspaceInfo, String>;
    fn search_databases(&self, token: &str, query: Option<String>) -> Result<Vec<DatabaseBrief>, String>;
    fn search_databases_page(
        &self,
        token: &str,
        query: Option<String>,
        start_cursor: Option<String>,
        page_size: Option<u32>,
    ) -> Result<DatabasePage, String>;
    fn get_database_schema(&self, token: &str, database_id: &str) -> Result<DatabaseSchema, String>;
}

/// A placeholder adapter that does not perform network calls.
/// It allows wiring the UI and commands without requiring network or credentials.
pub struct MockNotionAdapter;

impl NotionAdapter for MockNotionAdapter {
    fn test_connection(&self, _token: &str) -> Result<WorkspaceInfo, String> {
        Ok(WorkspaceInfo {
            workspace_name: Some("Offline Workspace".into()),
            bot_name: Some("Mock Bot".into()),
        })
    }

    fn search_databases(&self, _token: &str, query: Option<String>) -> Result<Vec<DatabaseBrief>, String> {
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
                let title = if i % 7 == 0 { String::new() } else { format!("Sample DB {:02} {}", i, q) };
                DatabaseBrief {
                    id: format!("db_mock_{:02}", i),
                    title,
                    icon: Some(if i % 3 == 0 { "📒" } else if i % 3 == 1 { "✅" } else { "📝" }.into()),
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
        let next_cursor = if has_more { Some(end.to_string()) } else { None };
        Ok(DatabasePage { results: slice, has_more, next_cursor })
    }

    fn get_database_schema(&self, _token: &str, database_id: &str) -> Result<DatabaseSchema, String> {
        // Return a stable mock schema for development.
        let props = vec![
            DatabaseProperty { name: "Name".into(), type_: "title".into(), required: Some(true), options: None },
            DatabaseProperty { name: "Score".into(), type_: "number".into(), required: None, options: None },
            DatabaseProperty { name: "Tag".into(), type_: "select".into(), required: None, options: Some(vec!["A".into(), "B".into()]) },
            DatabaseProperty { name: "Tags".into(), type_: "multi_select".into(), required: None, options: Some(vec!["A".into(), "B".into(), "C".into()]) },
            DatabaseProperty { name: "Date".into(), type_: "date".into(), required: None, options: None },
            DatabaseProperty { name: "Done".into(), type_: "checkbox".into(), required: None, options: None },
        ];
        Ok(DatabaseSchema { id: database_id.into(), title: format!("{} (Mock)", database_id), properties: props })
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
        Ok(WorkspaceInfo { workspace_name: None, bot_name: None })
    }

    fn search_databases(&self, token: &str, query: Option<String>) -> Result<Vec<DatabaseBrief>, String> {
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
                    out.push(DatabaseBrief { id: id.to_string(), title, icon });
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
        if let Some(sz) = page_size { payload["page_size"] = json!(sz); }
        if let Some(cur) = start_cursor.clone() { payload["start_cursor"] = json!(cur); }
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
                    results.push(DatabaseBrief { id: id.to_string(), title, icon });
                }
            }
        }
        let has_more = v.get("has_more").and_then(|b| b.as_bool()).unwrap_or(false);
        let next_cursor = v.get("next_cursor").and_then(|c| c.as_str()).map(|s| s.to_string());
        Ok(DatabasePage { results, has_more, next_cursor })
    }

    fn get_database_schema(&self, token: &str, database_id: &str) -> Result<DatabaseSchema, String> {
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
                    if let Some(t) = frag.get("plain_text").and_then(|x| x.as_str()) { s.push_str(t); }
                }
                s
            })
            .unwrap_or_default();
        let mut properties: Vec<DatabaseProperty> = Vec::new();
        if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
            for (name, pdef) in props {
                let t = pdef.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let options = match t.as_str() {
                    "select" => pdef
                        .get("select")
                        .and_then(|s| s.get("options"))
                        .and_then(|o| o.as_array())
                        .map(|arr| arr.iter().filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())).collect()),
                    "multi_select" => pdef
                        .get("multi_select")
                        .and_then(|s| s.get("options"))
                        .and_then(|o| o.as_array())
                        .map(|arr| arr.iter().filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())).collect()),
                    _ => None,
                };
                properties.push(DatabaseProperty { name: name.clone(), type_: t, required: None, options });
            }
        }
        Ok(DatabaseSchema { id: database_id.to_string(), title, properties })
    }
}
