use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};
use thiserror::Error;
use url::{form_urlencoded, Url};

use super::settings::OAuthSettings;
use super::storage::{OAuthRefreshSuccess, OAuthTokenParams, TokenStore};
use super::types::TokenRow;

const AUTHORIZATION_ENDPOINT: &str = "https://api.notion.com/v1/oauth/authorize";
const TOKEN_ENDPOINT: &str = "https://api.notion.com/v1/oauth/token";
const NOTION_VERSION: &str = "2022-06-28";
const STATE_TTL: Duration = Duration::from_secs(10 * 60);
const DEFAULT_EXPIRES_FALLBACK_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
enum GrantType {
    AuthorizationCode { code: String },
    RefreshToken { token: String },
}

impl GrantType {
    fn as_payload(&self, redirect_uri: &str) -> serde_json::Value {
        match self {
            Self::AuthorizationCode { code } => json!({
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": redirect_uri,
            }),
            Self::RefreshToken { token } => json!({
                "grant_type": "refresh_token",
                "refresh_token": token,
            }),
        }
    }

    #[cfg(test)]
    fn grant_type_name(&self) -> &'static str {
        match self {
            Self::AuthorizationCode { .. } => "authorization_code",
            Self::RefreshToken { .. } => "refresh_token",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OAuthSessionConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub owner: String,
    pub response_type: String,
    pub token_url: String,
}

impl OAuthSessionConfig {
    pub fn new(client_id: String, client_secret: String, redirect_uri: String) -> Self {
        Self {
            client_id,
            client_secret,
            redirect_uri,
            owner: "user".into(),
            response_type: "code".into(),
            token_url: TOKEN_ENDPOINT.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartOAuthSession {
    pub authorization_url: String,
    pub state: String,
    pub expires_at: i64,
}

#[derive(Debug)]
struct SessionRecord {
    created_at: Instant,
}

#[derive(Debug, Default)]
pub struct OAuthSessionManager {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    ttl: Duration,
}

impl OAuthSessionManager {
    pub fn new(ttl: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(STATE_TTL)
    }

    pub fn start_session(&self, config: &OAuthSessionConfig) -> StartOAuthSession {
        self.purge_expired();
        let state = generate_state();
        {
            let mut guard = self.sessions.lock().expect("oauth sessions poisoned");
            guard.insert(state.clone(), SessionRecord::new());
        }
        let authorization_url = build_authorization_url(config, &state);
        let expires_at = compute_expiration(self.ttl);
        StartOAuthSession {
            authorization_url,
            state,
            expires_at,
        }
    }

    pub fn exchange_code(
        &self,
        config: &OAuthSessionConfig,
        store: Arc<dyn TokenStore>,
        token_name: &str,
        pasted_url: &str,
    ) -> Result<TokenRow, OAuthExchangeError> {
        if config.client_secret.trim().is_empty() {
            return Err(OAuthExchangeError::MissingClientSecret);
        }

        let parsed = Url::parse(pasted_url).map_err(|_| OAuthExchangeError::InvalidRedirect)?;
        let mut code: Option<String> = None;
        let mut state_param: Option<String> = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state_param = Some(value.into_owned()),
                _ => {}
            }
        }

        let state_value = state_param.ok_or(OAuthExchangeError::MissingState)?;
        self.consume_state(&state_value)?;

        let code_value = code.ok_or(OAuthExchangeError::MissingCode)?;
        let expected_redirect =
            Url::parse(&config.redirect_uri).map_err(|_| OAuthExchangeError::InvalidRedirect)?;
        if parsed.origin() != expected_redirect.origin() {
            return Err(OAuthExchangeError::InvalidRedirect);
        }
        let token_result = self.request_authorization_code(config, &code_value)?;
        let row = catch_unwind(AssertUnwindSafe(|| {
            store.save_oauth(OAuthTokenParams {
                name: token_name.to_string(),
                access_token: token_result.access_token,
                refresh_token: token_result.refresh_token,
                expires_at: compute_oauth_expires_at(token_result.expires_in),
                workspace_name: token_result.workspace_name,
                workspace_icon: token_result.workspace_icon,
                workspace_id: token_result.workspace_id,
            })
        }))
        .map_err(|payload| map_storage_panic(payload))?;
        self.purge_expired();
        Ok(row)
    }

    pub fn refresh_token(
        &self,
        config: &OAuthSessionConfig,
        refresh_token: &str,
    ) -> Result<OAuthRefreshSuccess, OAuthExchangeError> {
        if config.client_secret.trim().is_empty() {
            return Err(OAuthExchangeError::MissingClientSecret);
        }
        if refresh_token.trim().is_empty() {
            return Err(OAuthExchangeError::MissingRefreshToken);
        }
        let result = self.request_refresh_token(config, refresh_token)?;
        Ok(OAuthRefreshSuccess {
            access_token: result.access_token,
            refresh_token: result.refresh_token,
            expires_at: compute_oauth_expires_at(result.expires_in),
            workspace_name: result.workspace_name,
            workspace_icon: result.workspace_icon,
            workspace_id: result.workspace_id,
        })
    }

    fn request_authorization_code(
        &self,
        config: &OAuthSessionConfig,
        code: &str,
    ) -> Result<TokenResult, OAuthExchangeError> {
        let grant = GrantType::AuthorizationCode {
            code: code.to_string(),
        };
        self.request_token(config, &grant)
    }

    fn request_refresh_token(
        &self,
        config: &OAuthSessionConfig,
        refresh_token: &str,
    ) -> Result<TokenResult, OAuthExchangeError> {
        let grant = GrantType::RefreshToken {
            token: refresh_token.to_string(),
        };
        self.request_token(config, &grant)
    }

    fn request_token(
        &self,
        config: &OAuthSessionConfig,
        grant: &GrantType,
    ) -> Result<TokenResult, OAuthExchangeError> {
        let payload = grant.as_payload(&config.redirect_uri);
        #[cfg(test)]
        {
            if config.token_url.starts_with("mock://") {
                return parse_mock_token(&config.token_url, grant);
            }
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| OAuthExchangeError::RequestFailed(err.to_string()))?;
        let response = client
            .post(&config.token_url)
            .basic_auth(&config.client_id, Some(&config.client_secret))
            .header("Content-Type", "application/json")
            .header("Notion-Version", NOTION_VERSION)
            .json(&payload)
            .send()
            .map_err(|err| OAuthExchangeError::RequestFailed(err.to_string()))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(parse_oauth_error(status.as_u16(), &text, grant));
        }
        let parsed: OAuthTokenResponse = serde_json::from_str(&text)
            .map_err(|err| OAuthExchangeError::RequestFailed(format!("解析响应失败: {}", err)))?;
        let access_token = parsed.access_token.unwrap_or_default();
        if access_token.is_empty() {
            return Err(OAuthExchangeError::MissingAccessToken);
        }
        Ok(TokenResult {
            access_token,
            refresh_token: parsed.refresh_token,
            expires_in: parsed.expires_in,
            workspace_name: parsed.workspace_name,
            workspace_icon: parsed.workspace_icon,
            workspace_id: parsed.workspace_id,
        })
    }

    fn purge_expired(&self) {
        let mut guard = self.sessions.lock().expect("oauth sessions poisoned");
        guard.retain(|_, record| record.created_at.elapsed() < self.ttl);
    }

    fn consume_state(&self, state: &str) -> Result<(), OAuthExchangeError> {
        let mut guard = self.sessions.lock().expect("oauth sessions poisoned");
        if let Some(record) = guard.remove(state) {
            if record.created_at.elapsed() > self.ttl {
                return Err(OAuthExchangeError::StateExpired);
            }
            Ok(())
        } else {
            Err(OAuthExchangeError::StateMismatch)
        }
    }
}

#[cfg(test)]
fn parse_mock_token(url: &str, grant: &GrantType) -> Result<TokenResult, OAuthExchangeError> {
    let scenario = url.trim_start_matches("mock://");
    match scenario {
        "exchange-success" => {
            if grant.grant_type_name() != "authorization_code" {
                return Err(OAuthExchangeError::RequestFailed(
                    "unexpected grant type".into(),
                ));
            }
            Ok(TokenResult {
                access_token: "secret_token_value".into(),
                refresh_token: Some("refresh_token_value".into()),
                expires_in: Some(3600),
                workspace_name: Some("My Workspace".into()),
                workspace_icon: Some("https://icon.example/icon.png".into()),
                workspace_id: Some("ws_123".into()),
            })
        }
        "refresh-success" => {
            if grant.grant_type_name() != "refresh_token" {
                return Err(OAuthExchangeError::RequestFailed(
                    "unexpected grant type".into(),
                ));
            }
            Ok(TokenResult {
                access_token: "refreshed_token".into(),
                refresh_token: Some("new_refresh_token".into()),
                expires_in: Some(1800),
                workspace_name: Some("Refresh Workspace".into()),
                workspace_icon: None,
                workspace_id: Some("ws_321".into()),
            })
        }
        "exchange-invalid-grant" => {
            if grant.grant_type_name() != "authorization_code" {
                return Err(OAuthExchangeError::RequestFailed(
                    "unexpected grant type".into(),
                ));
            }
            Err(OAuthExchangeError::AuthorizationCodeExpired)
        }
        "refresh-invalid-grant" => {
            if grant.grant_type_name() != "refresh_token" {
                return Err(OAuthExchangeError::RequestFailed(
                    "unexpected grant type".into(),
                ));
            }
            Err(OAuthExchangeError::RefreshTokenInvalid)
        }
        "invalid-client" => Err(OAuthExchangeError::InvalidClientCredentials),
        "access-denied" => Err(OAuthExchangeError::AccessDenied),
        other => Err(OAuthExchangeError::RequestFailed(format!(
            "unknown mock scenario: {}",
            other
        ))),
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OAuthExchangeError {
    #[error("OAuth state 不匹配，请重新开始授权")]
    StateMismatch,
    #[error("OAuth 状态已过期，请重新开始授权")]
    StateExpired,
    #[error("授权回调 URL 缺少 state 参数")]
    MissingState,
    #[error("授权回调 URL 缺少 code 参数")]
    MissingCode,
    #[error("授权回调 URL 无效")]
    InvalidRedirect,
    #[error("未配置 Notion OAuth client secret，请检查 NOTION_OAUTH_CLIENT_SECRET 环境变量")]
    MissingClientSecret,
    #[error("Notion 授权码已过期或已使用，请重新生成授权链接")]
    AuthorizationCodeExpired,
    #[error("当前 OAuth Token 已失效，请重新授权")]
    RefreshTokenInvalid,
    #[error("Notion OAuth 客户端凭证无效，请检查 NOTION_OAUTH_CLIENT_ID/SECRET 配置")]
    InvalidClientCredentials,
    #[error("已取消授权，请重新确认后再试")]
    AccessDenied,
    #[error("Notion OAuth 请求失败：{0}")]
    RequestFailed(String),
    #[error("Notion OAuth 响应缺少 access_token")]
    MissingAccessToken,
    #[error("当前 OAuth Token 不支持刷新操作")]
    MissingRefreshToken,
    #[error("保存 OAuth Token 时出现错误：{0}")]
    StorageFailure(String),
    #[error("当前数据库缺少 OAuth 所需字段，请参照 docs/notion-import-oauth-token-plan.md 中的 SQL 指引执行手动 ALTER TABLE 后重试")]
    StorageMissingColumns,
}

#[derive(Debug)]
struct TokenResult {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    workspace_name: Option<String>,
    workspace_icon: Option<String>,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    workspace_id: Option<String>,
    workspace_name: Option<String>,
    workspace_icon: Option<String>,
    bot_id: Option<String>,
    duplicated_template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    code: Option<String>,
    message: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn parse_oauth_error(status: u16, text: &str, grant: &GrantType) -> OAuthExchangeError {
    if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(text) {
        let code_opt = err.code.or(err.error);
        let description_opt = err.message.or(err.error_description);
        if let Some(code_raw) = code_opt.as_ref() {
            let code = code_raw.trim().to_lowercase();
            match code.as_str() {
                "invalid_grant" => {
                    return match grant {
                        GrantType::AuthorizationCode { .. } => {
                            OAuthExchangeError::AuthorizationCodeExpired
                        }
                        GrantType::RefreshToken { .. } => OAuthExchangeError::RefreshTokenInvalid,
                    };
                }
                "invalid_client" | "unauthorized_client" => {
                    return OAuthExchangeError::InvalidClientCredentials;
                }
                "access_denied" => {
                    return OAuthExchangeError::AccessDenied;
                }
                _ => {}
            }
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(code) = code_opt {
            if !code.trim().is_empty() {
                parts.push(code);
            }
        }
        if let Some(desc) = description_opt {
            if !desc.trim().is_empty() {
                parts.push(desc);
            }
        }
        if !parts.is_empty() {
            return OAuthExchangeError::RequestFailed(format!(
                "HTTP {}: {}",
                status,
                parts.join(" - ")
            ));
        }
    }
    let fallback = text.trim();
    if fallback.is_empty() {
        OAuthExchangeError::RequestFailed(format!("HTTP {}: (empty response)", status))
    } else {
        OAuthExchangeError::RequestFailed(format!("HTTP {}: {}", status, fallback))
    }
}

fn map_storage_panic(payload: Box<dyn Any + Send>) -> OAuthExchangeError {
    let message = if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "未知存储错误".to_string()
    };
    if message.to_lowercase().contains("no column named") || message.contains("缺少 OAuth 所需列")
    {
        OAuthExchangeError::StorageMissingColumns
    } else {
        OAuthExchangeError::StorageFailure(message)
    }
}

fn compute_oauth_expires_at(expires_in: Option<i64>) -> Option<i64> {
    let seconds = expires_in.unwrap_or(DEFAULT_EXPIRES_FALLBACK_SECS);
    if seconds <= 0 {
        return None;
    }
    let now = Utc::now().timestamp_millis();
    Some(now.saturating_add(seconds.saturating_mul(1000)))
}

fn generate_state() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = thread_rng();
    let bytes: Vec<u8> = (0..32)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx]
        })
        .collect();
    String::from_utf8(bytes).expect("alphabet is valid utf8")
}

fn build_authorization_url(config: &OAuthSessionConfig, state: &str) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("client_id", &config.client_id);
    serializer.append_pair("redirect_uri", &config.redirect_uri);
    serializer.append_pair("response_type", &config.response_type);
    serializer.append_pair("owner", &config.owner);
    serializer.append_pair("state", state);
    format!("{}?{}", AUTHORIZATION_ENDPOINT, serializer.finish())
}

fn compute_expiration(ttl: Duration) -> i64 {
    let now = Utc::now();
    let chrono_ttl =
        chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(600));
    (now + chrono_ttl).timestamp_millis()
}

impl SessionRecord {
    fn new() -> Self {
        Self {
            created_at: Instant::now(),
        }
    }
}

impl OAuthSessionConfig {
    pub fn from_settings(settings: &OAuthSettings) -> Self {
        let mut config = Self::new(
            settings.client_id.clone(),
            settings.client_secret.clone(),
            settings.redirect_uri.clone(),
        );
        if let Some(url) = settings.token_url.as_ref() {
            if !url.trim().is_empty() {
                config.token_url = url.clone();
            }
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    use crate::notion::storage::InMemoryTokenStore;
    use crate::notion::types::TokenKind;

    #[test]
    fn start_session_builds_authorization_url() {
        let manager = OAuthSessionManager::with_default_ttl();
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        let session = manager.start_session(&config);
        assert!(session.authorization_url.contains("client-demo"));
        assert!(session.authorization_url.contains(&session.state));
        assert!(session.authorization_url.contains("response_type=code"));
        assert!(session.authorization_url.contains("owner=user"));
        assert!(session.expires_at > Utc::now().timestamp_millis());
    }

    #[test]
    fn exchange_code_succeeds_with_valid_state() {
        let manager = OAuthSessionManager::with_default_ttl();
        let mut config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        config.token_url = "mock://exchange-success".into();
        let session = manager.start_session(&config);
        let url = format!(
            "https://example.com/callback?code=test-code&state={}#/",
            session.state
        );
        let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let row = manager
            .exchange_code(&config, store.clone(), "OAuth Token", &url)
            .expect("exchange succeeds");
        assert_eq!(row.name, "OAuth Token");
        assert_eq!(row.kind, TokenKind::Oauth);
        assert_eq!(row.workspace_name.as_deref(), Some("My Workspace"));
        assert!(row.expires_at.is_some());
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, row.id);
        assert_eq!(list[0].workspace_name.as_deref(), Some("My Workspace"));
    }

    #[test]
    fn exchange_code_rejects_wrong_state() {
        let manager = OAuthSessionManager::with_default_ttl();
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        manager.start_session(&config);
        let url = "https://example.com/callback?code=test-code&state=other-state#/".to_string();
        let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let err = manager
            .exchange_code(&config, store, "OAuth Token", &url)
            .expect_err("should error");
        assert_eq!(err, OAuthExchangeError::StateMismatch);
    }

    #[test]
    fn exchange_code_rejects_expired_state() {
        let manager = OAuthSessionManager::new(Duration::from_millis(10));
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        let session = manager.start_session(&config);
        thread::sleep(Duration::from_millis(20));
        let url = format!(
            "https://example.com/callback?code=test-code&state={}#/",
            session.state
        );
        let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let err = manager
            .exchange_code(&config, store, "OAuth Token", &url)
            .expect_err("should expire");
        assert_eq!(err, OAuthExchangeError::StateExpired);
    }

    #[test]
    fn exchange_code_requires_code_param() {
        let manager = OAuthSessionManager::with_default_ttl();
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        let session = manager.start_session(&config);
        let url = format!("https://example.com/callback?state={}#/", session.state);
        let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let err = manager
            .exchange_code(&config, store, "OAuth Token", &url)
            .expect_err("missing code");
        assert_eq!(err, OAuthExchangeError::MissingCode);
    }

    #[test]
    fn exchange_code_requires_state_param() {
        let manager = OAuthSessionManager::with_default_ttl();
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        manager.start_session(&config);
        let url = "https://example.com/callback?code=test-code#/".to_string();
        let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let err = manager
            .exchange_code(&config, store, "OAuth Token", &url)
            .expect_err("missing state");
        assert_eq!(err, OAuthExchangeError::MissingState);
    }

    #[test]
    fn refresh_token_success() {
        let manager = OAuthSessionManager::with_default_ttl();
        let mut config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        config.token_url = "mock://refresh-success".into();

        let result = manager
            .refresh_token(&config, "mock-refresh")
            .expect("refresh succeeds");
        assert_eq!(result.access_token, "refreshed_token");
        assert_eq!(result.refresh_token.as_deref(), Some("new_refresh_token"));
        assert!(result.expires_at.is_some());
        assert_eq!(result.workspace_name.as_deref(), Some("Refresh Workspace"));
    }

    #[test]
    fn refresh_token_requires_refresh_value() {
        let manager = OAuthSessionManager::with_default_ttl();
        let config = OAuthSessionConfig::new(
            "client-demo".into(),
            "secret-demo".into(),
            "https://example.com/callback".into(),
        );
        let err = manager
            .refresh_token(&config, " ")
            .expect_err("missing refresh token");
        assert_eq!(err, OAuthExchangeError::MissingRefreshToken);
    }

    #[test]
    fn parse_error_maps_invalid_grant_for_code_exchange() {
        let grant = GrantType::AuthorizationCode {
            code: "demo-code".into(),
        };
        let err = parse_oauth_error(
            400,
            r#"{"error":"invalid_grant","error_description":"authorization code expired"}"#,
            &grant,
        );
        assert_eq!(err, OAuthExchangeError::AuthorizationCodeExpired);
    }

    #[test]
    fn parse_error_maps_invalid_grant_for_refresh() {
        let grant = GrantType::RefreshToken {
            token: "old-refresh".into(),
        };
        let err = parse_oauth_error(
            400,
            r#"{"error":"invalid_grant","error_description":"token revoked"}"#,
            &grant,
        );
        assert_eq!(err, OAuthExchangeError::RefreshTokenInvalid);
    }

    #[test]
    fn parse_error_maps_invalid_client() {
        let grant = GrantType::AuthorizationCode {
            code: "demo-code".into(),
        };
        let err = parse_oauth_error(
            401,
            r#"{"error":"invalid_client","error_description":"client credentials mismatch"}"#,
            &grant,
        );
        assert_eq!(err, OAuthExchangeError::InvalidClientCredentials);
    }

    #[test]
    fn parse_error_maps_access_denied() {
        let grant = GrantType::AuthorizationCode {
            code: "demo-code".into(),
        };
        let err = parse_oauth_error(
            403,
            r#"{"error":"access_denied","error_description":"user denied"}"#,
            &grant,
        );
        assert_eq!(err, OAuthExchangeError::AccessDenied);
    }

    #[test]
    fn parse_error_falls_back_for_unknown() {
        let grant = GrantType::AuthorizationCode {
            code: "demo-code".into(),
        };
        let err = parse_oauth_error(500, "internal error", &grant);
        assert_eq!(
            err,
            OAuthExchangeError::RequestFailed("HTTP 500: internal error".into())
        );
    }
}
