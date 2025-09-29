use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::types::TokenRow;

pub trait TokenStore: Send + Sync {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow;
    fn list(&self) -> Vec<TokenRow>;
    fn delete(&self, id: &str) -> bool;
    fn get_token(&self, id: &str) -> Option<String>;
}

#[derive(Default)]
pub struct InMemoryTokenStore {
    inner: Mutex<StoreInner>,
}

#[derive(Default)]
struct StoreInner {
    seq: u64,
    rows: HashMap<String, (TokenRow, String)>, // id -> (row, token_plain)
}

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(seq: &mut u64) -> String {
        *seq += 1;
        let now_ms = chrono::Utc::now().timestamp_millis();
        format!("tok-{}-{}", now_ms, *seq)
    }
}

impl TokenStore for InMemoryTokenStore {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow {
        let mut guard = self.inner.lock().expect("poisoned");
        let id = Self::next_id(&mut guard.seq);
        let now = chrono::Utc::now().timestamp_millis();
        let row = TokenRow {
            id: id.clone(),
            name: name.to_string(),
            workspace_name,
            created_at: now,
            last_used_at: Some(now),
        };
        guard.rows.insert(id.clone(), (row.clone(), token.to_string()));
        row
    }

    fn list(&self) -> Vec<TokenRow> {
        let guard = self.inner.lock().expect("poisoned");
        let mut rows: Vec<_> = guard.rows.values().map(|(r, _)| r.clone()).collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        rows
    }

    fn delete(&self, id: &str) -> bool {
        let mut guard = self.inner.lock().expect("poisoned");
        guard.rows.remove(id).is_some()
    }

    fn get_token(&self, id: &str) -> Option<String> {
        let mut guard = self.inner.lock().expect("poisoned");
        if let Some((row, token)) = guard.rows.get_mut(id) {
            row.last_used_at = Some(chrono::Utc::now().timestamp_millis());
            Some(token.clone())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip() {
        let store = InMemoryTokenStore::new();
        let saved = store.save("demo", "secret-123", Some("Workspace".into()));
        assert!(saved.id.starts_with("tok-"));
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "demo");
        let token = store.get_token(&saved.id).unwrap();
        assert_eq!(token, "secret-123");
        assert!(store.delete(&saved.id));
        assert!(store.list().is_empty());
    }
}

// -----------------------------
// SQLite-backed TokenStore
// -----------------------------

#[cfg(feature = "notion-sqlite")]
pub struct SqliteTokenStore {
    db_path: PathBuf,
}

#[cfg(feature = "notion-sqlite")]
impl SqliteTokenStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

#[cfg(feature = "notion-sqlite")]
impl TokenStore for SqliteTokenStore {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let now = chrono::Utc::now().timestamp_millis();
        // Use SQLite to generate a random 128-bit id.
        let mut stmt = conn
            .prepare(
                "INSERT INTO notion_tokens (id, name, token_cipher, workspace_name, created_at, last_used_at, encryption_salt)
                 VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, NULL)
                 RETURNING id",
            )
            .expect("prepare insert");
        let id: String = stmt
            .query_row((name, token, workspace_name.clone(), now, now), |row| row.get(0))
            .expect("insert row");
        TokenRow { id, name: name.to_string(), workspace_name, created_at: now, last_used_at: Some(now) }
    }

    fn list(&self) -> Vec<TokenRow> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let mut stmt = conn
            .prepare(
                "SELECT id, name, workspace_name, created_at, last_used_at
                 FROM notion_tokens ORDER BY created_at",
            )
            .expect("prepare list");
        let rows = stmt
            .query_map([], |row| {
                Ok(TokenRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    workspace_name: row.get(2)?,
                    created_at: row.get(3)?,
                    last_used_at: row.get(4)?,
                })
            })
            .expect("query map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn delete(&self, id: &str) -> bool {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let affected = conn
            .execute("DELETE FROM notion_tokens WHERE id = ?1", [id])
            .expect("delete token");
        affected > 0
    }

    fn get_token(&self, id: &str) -> Option<String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let now = chrono::Utc::now().timestamp_millis();
        let _ = conn
            .execute(
                "UPDATE notion_tokens SET last_used_at = ?2 WHERE id = ?1",
                (id, now),
            )
            .ok();
        // token_cipher is declared as BLOB but stores UTF-8 text in M1.
        // Read as String for maximum compatibility; future encryption can switch representation safely.
        let mut stmt = conn
            .prepare("SELECT token_cipher FROM notion_tokens WHERE id = ?1")
            .expect("prepare select token");
        let token: Option<String> = stmt.query_row([id], |row| row.get::<_, String>(0)).ok();
        token
    }
}
