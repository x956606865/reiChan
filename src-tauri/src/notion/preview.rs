//! Data preview utilities for Notion import.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    pub path: String,
    pub file_type: Option<String>,
    pub limit_rows: Option<usize>,
    pub limit_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResponse {
    pub fields: Vec<String>,
    pub records: Vec<Value>,
}

pub fn preview_file(req: &PreviewRequest) -> Result<PreviewResponse, String> {
    let path = PathBuf::from(&req.path);
    if !path.exists() {
        return Err("source file not found".into());
    }

    let limit_rows = req.limit_rows.unwrap_or(50).max(1);
    let limit_bytes = req.limit_bytes.unwrap_or(512 * 1024).max(256);
    let kind = detect_file_kind(req.file_type.as_deref(), &path)
        .ok_or_else(|| "unsupported or unknown file type".to_string())?;

    match kind {
        FileKind::Csv => preview_csv(&path, limit_rows, limit_bytes),
        FileKind::Json | FileKind::JsonLines => preview_json(&path, limit_rows, limit_bytes, kind),
    }
}

fn preview_csv(
    path: &Path,
    limit_rows: usize,
    limit_bytes: usize,
) -> Result<PreviewResponse, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let reader = BufReader::new(file);
    let limited = reader.take(limit_bytes as u64);

    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(limited);

    let mut fields: Vec<String> = csv_reader
        .headers()
        .map_err(|err| err.to_string())?
        .iter()
        .map(|h| h.to_string())
        .collect();

    let mut records = Vec::new();
    for (idx, row) in csv_reader.records().enumerate() {
        if idx >= limit_rows {
            break;
        }
        let record = row.map_err(|err| err.to_string())?;
        if record.len() > fields.len() {
            for col_idx in fields.len()..record.len() {
                fields.push(format!("column_{}", col_idx + 1));
            }
        }
        let mut obj = Map::new();
        for (col_idx, key) in fields.iter().enumerate() {
            let value = record.get(col_idx).unwrap_or("");
            obj.insert(key.clone(), Value::String(value.to_string()));
        }
        records.push(Value::Object(obj));
    }

    Ok(PreviewResponse { fields, records })
}

fn preview_json(
    path: &Path,
    limit_rows: usize,
    limit_bytes: usize,
    kind: FileKind,
) -> Result<PreviewResponse, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let mut reader = BufReader::new(file);
    let mut buffer = String::new();
    reader
        .by_ref()
        .take(limit_bytes as u64)
        .read_to_string(&mut buffer)
        .map_err(|err| err.to_string())?;

    if buffer.trim().is_empty() {
        return Ok(PreviewResponse {
            fields: Vec::new(),
            records: Vec::new(),
        });
    }

    let mut rows: Vec<Value> = Vec::new();
    match kind {
        FileKind::Json => match serde_json::from_str::<Value>(&buffer) {
            Ok(Value::Array(items)) => {
                for item in items.into_iter().take(limit_rows) {
                    rows.push(normalize_record(item));
                }
            }
            Ok(Value::Object(obj)) => {
                rows.push(Value::Object(obj));
            }
            Ok(other) => {
                rows.push(normalize_record(other));
            }
            Err(_) => {
                // Fallback to JSONL parsing if array parsing fails
                parse_json_lines(buffer.lines(), limit_rows, &mut rows)?;
            }
        },
        FileKind::JsonLines => {
            parse_json_lines(buffer.lines(), limit_rows, &mut rows)?;
        }
        FileKind::Csv => unreachable!(),
    }

    let mut field_order = Vec::new();
    let mut field_set = HashSet::new();
    for record in &rows {
        if let Value::Object(map) = record {
            for key in map.keys() {
                if field_set.insert(key.clone()) {
                    field_order.push(key.clone());
                }
            }
        }
    }
    if field_order.is_empty() {
        field_order.push("_value".into());
        rows = rows
            .into_iter()
            .map(|record| match record {
                Value::Object(map) => Value::Object(map),
                other => {
                    let mut obj = Map::new();
                    obj.insert("_value".into(), other);
                    Value::Object(obj)
                }
            })
            .collect();
    }

    Ok(PreviewResponse {
        fields: field_order,
        records: rows,
    })
}

fn parse_json_lines<'a, I>(lines: I, limit_rows: usize, rows: &mut Vec<Value>) -> Result<(), String>
where
    I: Iterator<Item = &'a str>,
{
    for line in lines.filter(|line| !line.trim().is_empty()) {
        if rows.len() >= limit_rows {
            break;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(val) => rows.push(normalize_record(val)),
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(())
}

fn normalize_record(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        other => {
            let mut map = Map::new();
            map.insert("_value".into(), other);
            Value::Object(map)
        }
    }
}

enum FileKind {
    Csv,
    Json,
    JsonLines,
}

fn detect_file_kind(file_type: Option<&str>, path: &Path) -> Option<FileKind> {
    let normalized = file_type.map(|s| s.to_lowercase()).or_else(|| {
        path.extension()
            .and_then(|ext| ext.to_str().map(|s| s.to_lowercase()))
    });

    match normalized.as_deref() {
        Some("csv") => Some(FileKind::Csv),
        Some("jsonl") | Some("jsonlines") | Some("ndjson") => Some(FileKind::JsonLines),
        Some("json") => Some(FileKind::Json),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_handles_empty_csv() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "header1,header2\n").unwrap();
        let req = PreviewRequest {
            path: tmp.path().to_string_lossy().to_string(),
            file_type: Some("csv".into()),
            limit_rows: Some(10),
            limit_bytes: Some(1024),
        };
        let resp = preview_file(&req).expect("preview");
        assert_eq!(resp.fields, vec!["header1", "header2"]);
        assert!(resp.records.is_empty());
    }

    #[test]
    fn preview_parses_json_array() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"[{"title":"A"},{"title":"B","extra":1},{"title":"C"}]"#,
        )
        .unwrap();
        let req = PreviewRequest {
            path: tmp.path().to_string_lossy().to_string(),
            file_type: Some("json".into()),
            limit_rows: Some(2),
            limit_bytes: Some(4096),
        };
        let resp = preview_file(&req).expect("preview");
        assert_eq!(resp.fields, vec!["title", "extra"]);
        assert_eq!(resp.records.len(), 2);
    }

    #[test]
    fn preview_parses_json_lines() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{\"x\":1}\n{\"y\":2}\n").unwrap();
        let req = PreviewRequest {
            path: tmp.path().to_string_lossy().to_string(),
            file_type: Some("jsonl".into()),
            limit_rows: Some(5),
            limit_bytes: Some(4096),
        };
        let resp = preview_file(&req).expect("preview");
        assert_eq!(resp.fields, vec!["x", "y"]);
        assert_eq!(resp.records.len(), 2);
    }
}
