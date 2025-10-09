use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSettings {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
}

impl Default for OAuthSettings {
    fn default() -> Self {
        Self {
            client_id: "a2f7c25f-8eb8-4029-b280-7010a36a98a0".into(),
            client_secret: String::new(),
            redirect_uri: "https://www.yuributa.com".into(),
            token_url: None,
        }
    }
}

impl OAuthSettings {
    /// Returns a redacted clone that replaces the secret with masked form when non-empty.
    pub fn masked(&self) -> Self {
        let mut cloned = self.clone();
        if !cloned.client_secret.is_empty() {
            let prefix: String = cloned.client_secret.chars().take(2).collect();
            cloned.client_secret = format!("{}****", prefix);
        }
        cloned
    }

    pub fn normalize(mut self) -> Self {
        self.client_id = self.client_id.trim().to_string();
        self.client_secret = self.client_secret.trim().to_string();
        self.redirect_uri = self.redirect_uri.trim().to_string();
        if let Some(url) = self.token_url.take() {
            let trimmed = url.trim();
            if trimmed.is_empty() {
                self.token_url = None;
            } else {
                self.token_url = Some(trimmed.to_string());
            }
        }
        self
    }
}

pub fn load_oauth_settings(path: &Path) -> io::Result<OAuthSettings> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub fn save_oauth_settings(path: &Path, settings: &OAuthSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(path, json)
}

pub fn default_settings_path(root: &Path) -> PathBuf {
    root.join("notion_oauth_settings.json")
}
