use serde_json::{json, Map, Value};

use super::types::FieldMapping;

/// Build Notion `properties` payload from a source record and field mappings.
/// - This version intentionally ignores `transformCode` (M2 占位，先不执行 JS)。
/// - It handles a subset of common property types for Dry-run 构造验证。
pub fn build_properties(
    record: &Map<String, Value>,
    mappings: &[FieldMapping],
) -> Result<Map<String, Value>, String> {
    let mut props = Map::new();
    for m in mappings.iter().filter(|m| m.include) {
        let src_val = record.get(&m.source_field).cloned().unwrap_or(Value::Null);
        let key = m.target_property.clone();
        let entry = build_property_entry(m, &src_val)?;
        props.insert(key, entry);
    }
    Ok(props)
}

pub fn build_property_entry(mapping: &FieldMapping, src_val: &Value) -> Result<Value, String> {
    let entry = match mapping.target_type.as_str() {
        "title" => json!({
            "title": [{
                "type": "text",
                "text": {"content": to_string(src_val)},
            }]
        }),
        "rich_text" => json!({
            "rich_text": [{
                "type": "text",
                "text": {"content": to_string(src_val)},
            }]
        }),
        "number" => json!({
            "number": to_number(src_val)
        }),
        "select" => {
            let name = to_string_opt(src_val);
            json!({ "select": name.map(|n| json!({"name": n})).unwrap_or(Value::Null) })
        }
        "multi_select" => {
            let arr = to_string_array(src_val);
            json!({ "multi_select": arr.into_iter().map(|n| json!({"name": n})).collect::<Vec<_>>() })
        }
        "status" => {
            let name = to_string_opt(src_val);
            json!({ "status": name.map(|n| json!({"name": n})).unwrap_or(Value::Null) })
        }
        "date" => {
            let iso = to_string_opt(src_val);
            json!({ "date": iso.map(|s| json!({"start": s})).unwrap_or(Value::Null) })
        }
        "checkbox" => json!({
            "checkbox": to_bool(src_val)
        }),
        "url" => json!({ "url": to_string_opt(src_val) }),
        "email" => json!({ "email": to_string_opt(src_val) }),
        "phone_number" => json!({ "phone_number": to_string_opt(src_val) }),
        "people" => {
            let entries = to_people_entries(src_val)?;
            json!({ "people": entries })
        }
        "relation" => {
            let entries = to_relation_entries(src_val)?;
            json!({ "relation": entries })
        }
        "files" => {
            let entries = to_file_entries(src_val)?;
            json!({ "files": entries })
        }
        other => return Err(format!("Unsupported targetType: {}", other)),
    };
    Ok(entry)
}

fn to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn to_string_opt(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        _ => Some(to_string(v)),
    }
}

fn to_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn to_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(false),
        Value::String(s) => {
            let t = s.trim().to_lowercase();
            matches!(t.as_str(), "1" | "true" | "yes" | "y")
        }
        _ => false,
    }
}

fn to_string_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr.iter().map(to_string).collect(),
        Value::String(s) => {
            // Split by comma for convenience
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn to_people_entries(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(convert_single_person(item)?);
            }
            Ok(out)
        }
        other => Ok(vec![convert_single_person(other)?]),
    }
}

fn convert_single_person(value: &Value) -> Result<Value, String> {
    match value {
        Value::Object(obj) => {
            if obj.contains_key("id") {
                Ok(json!({
                    "object": "user",
                    "id": to_string(&obj["id"]),
                }))
            } else if let Some(email) = obj
                .get("person")
                .and_then(|p| p.get("email"))
                .and_then(|e| e.as_str())
            {
                Ok(json!({
                    "object": "user",
                    "person": { "email": email },
                }))
            } else {
                Err("people value object must include id or person.email".into())
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err("people value string cannot be empty".into());
            }
            if trimmed.contains('@') {
                Ok(json!({
                    "object": "user",
                    "person": { "email": trimmed },
                }))
            } else {
                Ok(json!({
                    "object": "user",
                    "id": trimmed,
                }))
            }
        }
        _ => Err("people value must be string, object or array".into()),
    }
}

fn to_relation_entries(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let id = extract_relation_id(item)?;
                out.push(json!({ "id": id }));
            }
            Ok(out)
        }
        other => {
            let id = extract_relation_id(other)?;
            Ok(vec![json!({ "id": id })])
        }
    }
}

fn extract_relation_id(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Err("relation id cannot be empty".into())
            } else if is_uuid_like(trimmed) {
                Ok(trimmed.to_string())
            } else {
                Err(format!(
                    "relation id '{}' is not a valid Notion UUID",
                    trimmed
                ))
            }
        }
        Value::Object(obj) => {
            if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                extract_relation_id(&Value::String(id.into()))
            } else {
                Err("relation object must include id".into())
            }
        }
        _ => Err("relation value must be string, object or array".into()),
    }
}

fn is_uuid_like(value: &str) -> bool {
    let normalized = value.replace('-', "");
    normalized.len() == 32 && normalized.chars().all(|c| c.is_ascii_hexdigit())
}

fn to_file_entries(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(convert_single_file(item)?);
            }
            Ok(out)
        }
        other => Ok(vec![convert_single_file(other)?]),
    }
}

fn convert_single_file(value: &Value) -> Result<Value, String> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err("file entry string cannot be empty".into());
            }
            let name = infer_file_name(trimmed);
            let url = if is_http_url(trimmed) {
                trimmed.to_string()
            } else if trimmed.starts_with("file://") {
                trimmed.to_string()
            } else {
                format!("file://{}", trimmed)
            };
            Ok(json!({
                "name": name,
                "external": { "url": url },
            }))
        }
        Value::Object(obj) => {
            if obj.contains_key("external") || obj.contains_key("file") {
                Ok(Value::Object(obj.clone()))
            } else {
                Err("file object must include external or file".into())
            }
        }
        _ => Err("files value must be string, object or array".into()),
    }
}

fn infer_file_name(path: &str) -> String {
    if let Some(name) = path.split(['/', '\\']).filter(|seg| !seg.is_empty()).last() {
        name.to_string()
    } else {
        "attachment".into()
    }
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_basic_properties() {
        let rec = json!({
            "title": "Hello",
            "score": "42",
            "tag": "X",
            "tags": ["A","B"],
            "when": "2025-01-02",
            "ok": "true",
        });
        let rec_map = rec.as_object().unwrap().clone();
        let mappings = vec![
            FieldMapping {
                include: true,
                source_field: "title".into(),
                target_property: "Name".into(),
                target_type: "title".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "score".into(),
                target_property: "Score".into(),
                target_type: "number".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "tag".into(),
                target_property: "Tag".into(),
                target_type: "select".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "tags".into(),
                target_property: "Tags".into(),
                target_type: "multi_select".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "when".into(),
                target_property: "Date".into(),
                target_type: "date".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "ok".into(),
                target_property: "Done".into(),
                target_type: "checkbox".into(),
                transform_code: None,
            },
        ];
        let props = build_properties(&rec_map, &mappings).expect("ok");
        assert!(props.get("Name").is_some());
        assert!(props.get("Score").is_some());
        assert!(props.get("Tag").is_some());
        assert!(props.get("Tags").is_some());
        assert!(props.get("Date").is_some());
        assert!(props.get("Done").is_some());
    }

    #[test]
    fn build_property_entry_transforms_number() {
        let mapping = FieldMapping {
            include: true,
            source_field: "score".into(),
            target_property: "Score".into(),
            target_type: "number".into(),
            transform_code: None,
        };
        let entry = build_property_entry(&mapping, &json!("12")).expect("entry");
        assert_eq!(entry.get("number").and_then(|v| v.as_f64()), Some(12.0));
    }

    #[test]
    fn builds_extended_properties() {
        let rec = json!({
            "status": "In Progress",
            "assignees": ["user-123"],
            "related": ["aaaaaaaa-bbbb-cccc-dddd-eeeeffffffff"],
            "attachment": "https://example.com/file.png",
        });
        let rec_map = rec.as_object().unwrap().clone();
        let mappings = vec![
            FieldMapping {
                include: true,
                source_field: "status".into(),
                target_property: "Status".into(),
                target_type: "status".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "assignees".into(),
                target_property: "Assignees".into(),
                target_type: "people".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "related".into(),
                target_property: "Related".into(),
                target_type: "relation".into(),
                transform_code: None,
            },
            FieldMapping {
                include: true,
                source_field: "attachment".into(),
                target_property: "Files".into(),
                target_type: "files".into(),
                transform_code: None,
            },
        ];
        let props = build_properties(&rec_map, &mappings).expect("ok");
        assert!(props.get("Status").is_some());
        assert!(props.get("Assignees").is_some());
        assert!(props.get("Related").is_some());
        assert!(props.get("Files").is_some());
    }

    #[test]
    fn relation_id_validation() {
        let mapping = FieldMapping {
            include: true,
            source_field: "related".into(),
            target_property: "Related".into(),
            target_type: "relation".into(),
            transform_code: None,
        };
        let err = build_property_entry(&mapping, &json!(["not-a-uuid"])).expect_err("should fail");
        assert!(err.contains("Notion UUID"));
    }
}
