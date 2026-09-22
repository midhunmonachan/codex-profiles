use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde::Serialize;
use serde_with::{NoneAsEmptyString, serde_as};
use std::path::Path;
use std::time::Duration;

use crate::{
    AUTH_ERR_FILE_NOT_FOUND, AUTH_ERR_INCOMPLETE_ACCOUNT, AUTH_ERR_INCOMPLETE_EMAIL,
    AUTH_ERR_INCOMPLETE_PLAN, AUTH_ERR_INVALID_JSON, AUTH_ERR_INVALID_JSON_OBJECT,
    AUTH_ERR_INVALID_JSON_RELOGIN, AUTH_ERR_INVALID_REFRESH_RESPONSE,
    AUTH_ERR_INVALID_TOKENS_OBJECT, AUTH_ERR_MISSING_TOKENS, AUTH_ERR_PROFILE_MISSING_ACCESS_TOKEN,
    AUTH_ERR_PROFILE_MISSING_ACCOUNT, AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN,
    AUTH_ERR_PROFILE_NO_REFRESH_TOKEN, AUTH_ERR_READ, AUTH_ERR_REFRESH_FAILED_OTHER,
    AUTH_ERR_REFRESH_MISSING_ACCESS_TOKEN, AUTH_ERR_REFRESH_STATE_CHANGED,
    AUTH_ERR_UNSUPPORTED_STORE_MODE, AUTH_ERR_WRITE_AUTH, write_atomic_private,
};

const API_KEY_PREFIX: &str = "api-key-";
const API_KEY_LABEL: &str = "Key";
const API_KEY_SEPARATOR: &str = "~";
const API_KEY_PREFIX_LEN: usize = 12;
const API_KEY_SUFFIX_LEN: usize = 16;
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_CLIENT_ID";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthStoreMode {
    File,
    Keyring,
    Auto,
    Ephemeral,
}

impl AuthStoreMode {
    fn as_str(self) -> &'static str {
        match self {
            AuthStoreMode::File => "file",
            AuthStoreMode::Keyring => "keyring",
            AuthStoreMode::Auto => "auto",
            AuthStoreMode::Ephemeral => "ephemeral",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<Tokens>,
    #[serde(default)]
    pub last_refresh: Option<String>,
}

#[serde_as]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    pub account_id: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    pub id_token: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    pub access_token: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    pub refresh_token: Option<String>,
}

#[serde_as]
#[derive(Deserialize)]
struct IdTokenClaims {
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    sub: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    email: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    organization_id: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    project_id: Option<String>,
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<AuthClaims>,
}

#[serde_as]
#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    chatgpt_plan_type: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    chatgpt_user_id: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    user_id: Option<String>,
    #[serde(default)]
    #[serde_as(as = "NoneAsEmptyString")]
    chatgpt_account_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileIdentityKey {
    pub principal_id: String,
    pub workspace_or_org_id: String,
    pub plan_type: String,
}

pub fn read_tokens(path: &Path) -> Result<Tokens, String> {
    let auth = read_auth_file(path)?;
    if let Some(tokens) = auth.tokens {
        return Ok(tokens);
    }
    if let Some(api_key) = auth.openai_api_key.as_deref() {
        return Ok(tokens_from_api_key(api_key));
    }
    Err(crate::msg1(AUTH_ERR_MISSING_TOKENS, path.display()))
}

pub fn read_auth_file(path: &Path) -> Result<AuthFile, String> {
    ensure_file_auth_store(path)?;

    let data = std::fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AUTH_ERR_FILE_NOT_FOUND.to_string()
        } else {
            crate::msg2(AUTH_ERR_READ, path.display(), err)
        }
    })?;
    let auth: AuthFile = serde_json::from_str(&data)
        .map_err(|err| crate::msg2(AUTH_ERR_INVALID_JSON_RELOGIN, path.display(), err))?;
    // Deserializing AuthFile above already proves that this is valid JSON;
    // parse the same bytes as a value without manufacturing an unreachable
    // second parse error path.
    let raw: serde_json::Value =
        serde_json::from_str(&data).expect("AuthFile deserialization validates JSON");
    resolve_auth_file_mode(&raw, auth)
}

/// Resolve credentials with the same precedence as Codex's
/// `AuthDotJson::resolved_mode`. The public `AuthFile` model intentionally
/// omits credential fields owned by external stores, so callers that accept
/// in-memory JSON must pass the raw value through this resolver too.
pub(crate) fn resolve_auth_file_mode(
    raw: &serde_json::Value,
    auth: AuthFile,
) -> Result<AuthFile, String> {
    let explicit_mode = raw.get("auth_mode").filter(|value| !value.is_null());
    let mode = match explicit_mode {
        Some(value) => value
            .as_str()
            .ok_or_else(|| "Invalid auth_mode: expected a string".to_string())?,
        None if raw
            .get("personal_access_token")
            .is_some_and(|v| !v.is_null()) =>
        {
            "personalAccessToken"
        }
        None if raw.get("bedrock_api_key").is_some_and(|v| !v.is_null()) => "bedrockApiKey",
        None if raw.get("bedrock_access_keys").is_some_and(|v| !v.is_null()) => "bedrockAccessKeys",
        None if auth.openai_api_key.is_some() => "apikey",
        None => "chatgpt",
    };
    match mode {
        "apikey" if auth.openai_api_key.is_some() => Ok(AuthFile { tokens: None, ..auth }),
        "chatgpt" => Ok(AuthFile { openai_api_key: None, ..auth }),
        "apikey" => Err("API-key authentication has no OPENAI_API_KEY".to_string()),
        _ => Err("Unsupported authentication mode. codex-profiles supports file-backed ChatGPT OAuth and OpenAI API keys; externally managed credentials cannot be switched here.".to_string()),
    }
}

pub fn ensure_file_auth_store(path: &Path) -> Result<(), String> {
    let store_mode = read_auth_store_mode_for_path(path)?;
    if store_mode != AuthStoreMode::File {
        return Err(crate::msg1(
            AUTH_ERR_UNSUPPORTED_STORE_MODE,
            store_mode.as_str(),
        ));
    }

    Ok(())
}

pub fn read_tokens_opt(path: &Path) -> Option<Tokens> {
    if !path.is_file() {
        return None;
    }
    read_tokens(path).ok()
}

pub fn tokens_from_api_key(api_key: &str) -> Tokens {
    Tokens {
        account_id: Some(api_key_profile_id(api_key)),
        id_token: None,
        access_token: None,
        refresh_token: None,
    }
}

pub fn has_auth(path: &Path) -> bool {
    read_tokens_opt(path).is_some_and(|tokens| is_profile_ready(&tokens))
}

pub fn is_profile_ready(tokens: &Tokens) -> bool {
    if is_api_key_profile(tokens) {
        return true;
    }
    if token_account_id(tokens).is_none() {
        return false;
    }
    if tokens.access_token.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    let (email, plan) = extract_email_and_plan(tokens);
    email.is_some() && plan.is_some()
}

pub fn extract_email_and_plan(tokens: &Tokens) -> (Option<String>, Option<String>) {
    if is_api_key_profile(tokens) {
        let display = api_key_display_label(tokens).unwrap_or_else(|| API_KEY_LABEL.to_string());
        return (Some(display), Some(API_KEY_LABEL.to_string()));
    }
    let claims = tokens.id_token.as_deref().and_then(decode_id_token_claims);
    let email = claims.as_ref().and_then(|c| c.email.clone());
    let plan = claims
        .and_then(|c| c.auth)
        .and_then(|auth| auth.chatgpt_plan_type)
        .map(|plan| format_plan(&plan));
    (email, plan)
}

pub fn extract_profile_identity(tokens: &Tokens) -> Option<ProfileIdentityKey> {
    if is_api_key_profile(tokens) {
        let principal_id = token_account_id(tokens)?.to_string();
        return Some(ProfileIdentityKey {
            workspace_or_org_id: principal_id.clone(),
            principal_id,
            plan_type: "key".to_string(),
        });
    }

    let claims = tokens.id_token.as_deref().and_then(decode_id_token_claims);
    let principal_id = claims
        .as_ref()
        .and_then(|claims| {
            claims.auth.as_ref().and_then(|auth| {
                auth.chatgpt_user_id
                    .clone()
                    .or_else(|| auth.user_id.clone())
            })
        })
        .or_else(|| claims.as_ref().and_then(|claims| claims.sub.clone()))
        .or_else(|| token_account_id(tokens).map(str::to_string))
        .and_then(|value| normalize_identity_value(&value))?;

    let workspace_or_org_id = claims
        .as_ref()
        .and_then(|claims| {
            claims
                .auth
                .as_ref()
                .and_then(|auth| auth.chatgpt_account_id.clone())
        })
        .or_else(|| {
            claims
                .as_ref()
                .and_then(|claims| claims.organization_id.clone())
        })
        .or_else(|| claims.as_ref().and_then(|claims| claims.project_id.clone()))
        .or_else(|| token_account_id(tokens).map(str::to_string))
        .and_then(|value| normalize_identity_value(&value))
        .unwrap_or_else(|| "unknown".to_string());

    let plan_type = claims
        .as_ref()
        .and_then(|claims| {
            claims
                .auth
                .as_ref()
                .and_then(|auth| auth.chatgpt_plan_type.clone())
        })
        .or_else(|| extract_email_and_plan(tokens).1)
        .map(|value| normalize_plan_type(&value))
        .unwrap_or_else(|| "unknown".to_string());

    Some(ProfileIdentityKey {
        principal_id,
        workspace_or_org_id,
        plan_type,
    })
}

fn normalize_identity_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_plan_type(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

pub fn require_identity(tokens: &Tokens) -> Result<(String, String, String), String> {
    let Some(account_id) = token_account_id(tokens) else {
        return Err(AUTH_ERR_INCOMPLETE_ACCOUNT.to_string());
    };
    let (email, plan) = extract_email_and_plan(tokens);
    let email = email.ok_or_else(|| AUTH_ERR_INCOMPLETE_EMAIL.to_string())?;
    let plan = plan.ok_or_else(|| AUTH_ERR_INCOMPLETE_PLAN.to_string())?;
    Ok((account_id.to_string(), email, plan))
}

pub fn profile_error(
    tokens: &Tokens,
    email: Option<&str>,
    plan: Option<&str>,
) -> Option<&'static str> {
    if is_api_key_profile(tokens) {
        return None;
    }
    if email.is_none() || plan.is_none() {
        return Some(AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN);
    }
    if token_account_id(tokens).is_none() {
        return Some(AUTH_ERR_PROFILE_MISSING_ACCOUNT);
    }
    if tokens.access_token.is_none() {
        return Some(AUTH_ERR_PROFILE_MISSING_ACCESS_TOKEN);
    }
    None
}

pub fn token_account_id(tokens: &Tokens) -> Option<&str> {
    tokens
        .account_id
        .as_deref()
        .filter(|value| !value.is_empty())
}

pub fn is_api_key_profile(tokens: &Tokens) -> bool {
    tokens
        .account_id
        .as_deref()
        .map(|value| value.starts_with(API_KEY_PREFIX))
        .unwrap_or(false)
        && tokens.id_token.is_none()
        && tokens.access_token.is_none()
        && tokens.refresh_token.is_none()
}

pub fn format_plan(plan: &str) -> String {
    let mut out = String::new();
    for word in plan.split(['_', '-']) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&title_case(word));
    }
    if out.is_empty() {
        "Unknown".to_string()
    } else {
        out
    }
}

pub fn is_free_plan(plan: Option<&str>) -> bool {
    plan.map(|value| value.eq_ignore_ascii_case("free"))
        .unwrap_or(false)
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    out.extend(chars.flat_map(|ch| ch.to_lowercase()));
    out
}

fn decode_id_token_claims(token: &str) -> Option<IdTokenClaims> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn api_key_profile_id(api_key: &str) -> String {
    let prefix = api_key_prefix(api_key);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in api_key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{API_KEY_PREFIX}{prefix}{API_KEY_SEPARATOR}{hash:016x}")
}

fn api_key_display_label(tokens: &Tokens) -> Option<String> {
    let account_id = tokens.account_id.as_deref()?;
    let rest = account_id.strip_prefix(API_KEY_PREFIX)?;
    let (prefix, hash) = rest.split_once(API_KEY_SEPARATOR)?;
    if prefix.is_empty() {
        return None;
    }
    let suffix: String = hash.chars().rev().take(API_KEY_SUFFIX_LEN).collect();
    let suffix: String = suffix.chars().rev().collect();
    if suffix.is_empty() {
        return None;
    }
    Some(format!("{API_KEY_SEPARATOR}{suffix}"))
}

fn api_key_prefix(api_key: &str) -> String {
    let mut out = String::new();
    for ch in api_key.chars().take(API_KEY_PREFIX_LEN) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

#[derive(Serialize)]
struct RefreshRequest {
    client_id: String,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

pub fn refresh_profile_tokens(path: &Path, tokens: &mut Tokens) -> Result<(), String> {
    let initial_contents = std::fs::read_to_string(path)
        .map_err(|err| crate::msg2(AUTH_ERR_READ, path.display(), err))?;
    let disk_tokens = read_tokens(path)?;
    if !same_refresh_state(&disk_tokens, tokens) {
        if same_profile_refresh_target(&disk_tokens, tokens) {
            *tokens = disk_tokens;
            return Ok(());
        }
        return Err(AUTH_ERR_REFRESH_STATE_CHANGED.to_string());
    }

    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AUTH_ERR_PROFILE_NO_REFRESH_TOKEN.to_string())?;
    let refreshed = refresh_access_token(refresh_token)?;
    validate_refresh_response(&refreshed)?;

    // The request can take long enough for Codex login, load, or another
    // refresh to replace the file. Re-read before applying the response so a
    // stale response cannot overwrite a newer account or token rotation.
    let latest_contents = std::fs::read_to_string(path)
        .map_err(|err| crate::msg2(AUTH_ERR_READ, path.display(), err))?;
    let latest_disk_tokens = read_tokens(path)?;
    if latest_contents != initial_contents {
        if same_auth_document_context(&initial_contents, &latest_contents)
            && same_profile_refresh_target(&latest_disk_tokens, tokens)
        {
            *tokens = latest_disk_tokens;
            return Ok(());
        }
        return Err(AUTH_ERR_REFRESH_STATE_CHANGED.to_string());
    }

    let mut next_tokens = latest_disk_tokens.clone();
    apply_refresh(&mut next_tokens, &refreshed)?;
    update_auth_tokens(path, &latest_contents, &refreshed)?;
    *tokens = next_tokens;
    Ok(())
}

fn same_refresh_state(left: &Tokens, right: &Tokens) -> bool {
    left.account_id == right.account_id
        && left.id_token == right.id_token
        && left.access_token == right.access_token
        && left.refresh_token == right.refresh_token
}

fn same_auth_document_context(left: &str, right: &str) -> bool {
    fn without_tokens_and_refresh(value: &str) -> Option<serde_json::Value> {
        let mut value = serde_json::from_str::<serde_json::Value>(value).ok()?;
        let object = value.as_object_mut()?;
        object.remove("tokens");
        object.remove("last_refresh");
        Some(value)
    }

    without_tokens_and_refresh(left) == without_tokens_and_refresh(right)
}

fn same_profile_refresh_target(left: &Tokens, right: &Tokens) -> bool {
    if left.account_id != right.account_id {
        return false;
    }

    match (
        extract_profile_identity(left),
        extract_profile_identity(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn read_auth_store_mode_for_path(path: &Path) -> Result<AuthStoreMode, String> {
    if path.file_name().and_then(|name| name.to_str()) != Some("auth.json") {
        return Ok(AuthStoreMode::File);
    }
    // A path whose filename is `auth.json` normally has a parent (`Path`
    // represents a bare filename with an empty parent). Keep the empty-parent
    // fallback defensive and total without a branch that cannot be reached by
    // a valid path containing this filename.
    let config_path = path.parent().unwrap_or(Path::new("")).join("config.toml");
    let config_keys = [
        "cli_auth_credentials_store",
        "cli_auth_credentials_store_mode",
    ];
    let configured_store = crate::common::read_config_string(&config_path, &config_keys)?;
    if let Some(value) = configured_store {
        return parse_auth_store_mode(&value);
    }
    Ok(AuthStoreMode::File)
}

fn parse_auth_store_mode(value: &str) -> Result<AuthStoreMode, String> {
    match value {
        "file" => Ok(AuthStoreMode::File),
        "keyring" => Ok(AuthStoreMode::Keyring),
        "auto" => Ok(AuthStoreMode::Auto),
        "ephemeral" => Ok(AuthStoreMode::Ephemeral),
        other => Err(crate::msg1(AUTH_ERR_UNSUPPORTED_STORE_MODE, other)),
    }
}

fn refresh_access_token(refresh_token: &str) -> Result<RefreshResponse, String> {
    let request = RefreshRequest {
        client_id: std::env::var(CLIENT_ID_OVERRIDE_ENV_VAR)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| CLIENT_ID.to_string()),
        grant_type: "refresh_token",
        refresh_token: refresh_token.to_string(),
    };
    let endpoint = refresh_token_url();
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();
    let response = agent
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .send_json(&request)
        .map_err(|other| crate::msg1(AUTH_ERR_REFRESH_FAILED_OTHER, other))?;

    if !response.status().is_success() {
        return Err(
            crate::UnexpectedHttpError::from_ureq_response(response, Some(&endpoint))
                .plain_message(),
        );
    }

    response
        .into_body()
        .read_json::<RefreshResponse>()
        .map_err(|err| crate::msg1(AUTH_ERR_INVALID_REFRESH_RESPONSE, err))
}

fn apply_refresh(tokens: &mut Tokens, refreshed: &RefreshResponse) -> Result<(), String> {
    let Some(access_token) = refreshed
        .access_token
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(AUTH_ERR_REFRESH_MISSING_ACCESS_TOKEN.to_string());
    };
    tokens.access_token = Some(access_token.clone());
    let refreshed_id_token = refreshed
        .id_token
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned();
    tokens.id_token = refreshed_id_token
        .clone()
        .or_else(|| tokens.id_token.clone());
    tokens.refresh_token = refreshed
        .refresh_token
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .or_else(|| tokens.refresh_token.clone());
    Ok(())
}

fn validate_refresh_response(refreshed: &RefreshResponse) -> Result<(), String> {
    if refreshed
        .access_token
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(crate::msg1(
            AUTH_ERR_INVALID_REFRESH_RESPONSE,
            "access_token is empty",
        ));
    }
    if refreshed
        .id_token
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(crate::msg1(
            AUTH_ERR_INVALID_REFRESH_RESPONSE,
            "id_token is empty",
        ));
    }
    if refreshed
        .refresh_token
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(crate::msg1(
            AUTH_ERR_INVALID_REFRESH_RESPONSE,
            "refresh_token is empty",
        ));
    }
    Ok(())
}

fn update_auth_tokens(
    path: &Path,
    expected_contents: &str,
    refreshed: &RefreshResponse,
) -> Result<(), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| crate::msg2(AUTH_ERR_READ, path.display(), err))?;
    let mut value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|err| crate::msg2(AUTH_ERR_INVALID_JSON, path.display(), err))?;

    let Some(root) = value.as_object_mut() else {
        return Err(crate::msg1(AUTH_ERR_INVALID_JSON_OBJECT, path.display()));
    };
    let tokens = root
        .entry("tokens")
        .or_insert_with(|| serde_json::json!({}));
    let Some(tokens_map) = tokens.as_object_mut() else {
        return Err(crate::msg1(AUTH_ERR_INVALID_TOKENS_OBJECT, path.display()));
    };

    // Keep the final read-modify-write conditional as well as the pre-request
    // check. Compare the complete document so an auth-mode or provider change
    // with identical token fields cannot be overwritten by a stale refresh.
    if contents != expected_contents {
        return Err(AUTH_ERR_REFRESH_STATE_CHANGED.to_string());
    }

    let _ = refreshed.id_token.as_ref().inspect(|id_token| {
        tokens_map.insert(
            "id_token".to_string(),
            serde_json::Value::String((*id_token).clone()),
        );
    });
    let _ = refreshed.access_token.as_ref().inspect(|access_token| {
        tokens_map.insert(
            "access_token".to_string(),
            serde_json::Value::String((*access_token).clone()),
        );
    });
    let _ = refreshed.refresh_token.as_ref().inspect(|refresh_token| {
        tokens_map.insert(
            "refresh_token".to_string(),
            serde_json::Value::String((*refresh_token).clone()),
        );
    });
    root.insert(
        "last_refresh".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    let json =
        serde_json::to_string_pretty(&value).expect("serde_json::Value is always serializable");
    write_atomic_private(path, format!("{json}\n").as_bytes())
        .map_err(|err| crate::msg2(AUTH_ERR_WRITE_AUTH, path.display(), err))
}

fn refresh_token_url() -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| REFRESH_TOKEN_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{FAIL_WRITE_OPEN, FailpointGuard};
    use crate::test_utils::{
        ENV_MUTEX, build_id_token, http_ok_response, set_env_guard, spawn_server,
    };
    use std::fs;

    fn build_id_token_payload(payload: &str) -> String {
        let header = r#"{"alg":"none","typ":"JWT"}"#;
        let header = URL_SAFE_NO_PAD.encode(header);
        let payload = URL_SAFE_NO_PAD.encode(payload);
        format!("{header}.{payload}.")
    }

    #[test]
    fn read_auth_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.json");
        let err = read_auth_file(&missing).unwrap_err();
        assert!(err.contains("Auth file not found"));

        let unreadable = dir.path().join("directory");
        fs::create_dir(&unreadable).unwrap();
        let err = read_auth_file(&unreadable).unwrap_err();
        assert!(err.contains("Could not read"));

        let bad = dir.path().join("bad.json");
        fs::write(&bad, "{oops").expect("write");
        let err = read_auth_file(&bad).unwrap_err();
        assert!(err.contains("Invalid JSON"));

        fs::write(&bad, r#"{"auth_mode":42}"#).unwrap();
        let err = read_auth_file(&bad).unwrap_err();
        assert!(err.contains("Invalid auth_mode: expected a string"));
    }

    #[test]
    fn read_tokens_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let id_token = build_id_token("me@example.com", "pro");
        let value = serde_json::json!({
            "tokens": {"account_id": "acct", "id_token": id_token, "access_token": "acc"}
        });
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let tokens = read_tokens(&path).unwrap();
        assert_eq!(token_account_id(&tokens), Some("acct"));

        let api_path = dir.path().join("auth_api.json");
        let value = serde_json::json!({"OPENAI_API_KEY": "sk-test"});
        fs::write(&api_path, serde_json::to_string(&value).unwrap()).unwrap();
        let tokens = read_tokens(&api_path).unwrap();
        assert!(is_api_key_profile(&tokens));

        let empty_path = dir.path().join("empty.json");
        fs::write(&empty_path, "{}").unwrap();
        let err = read_tokens(&empty_path).unwrap_err();
        assert!(err.contains("Missing tokens"));
    }

    #[test]
    fn auth_mode_matches_codex_precedence_and_rejects_external_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut value = serde_json::json!({
            "OPENAI_API_KEY": "sk-test",
            "tokens": { "account_id": "oauth-account", "access_token": "oauth-access" }
        });
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(is_api_key_profile(&read_tokens(&path).unwrap()));
        value["auth_mode"] = serde_json::json!("chatgpt");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            read_tokens(&path).unwrap().account_id.as_deref(),
            Some("oauth-account")
        );
        value["auth_mode"] = serde_json::json!("apikey");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(is_api_key_profile(&read_tokens(&path).unwrap()));
        for mode in [
            "chatgptAuthTokens",
            "agentIdentity",
            "personalAccessToken",
            "headers",
            "bedrockApiKey",
            "bedrockAccessKeys",
            "unknown",
        ] {
            value["auth_mode"] = serde_json::json!(mode);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                read_tokens(&path)
                    .unwrap_err()
                    .contains("Unsupported authentication mode")
            );
        }
        value.as_object_mut().unwrap().remove("auth_mode");
        for field in [
            "personal_access_token",
            "bedrock_api_key",
            "bedrock_access_keys",
        ] {
            value[field] = serde_json::json!("private-credential");
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = read_tokens(&path).unwrap_err();
            assert!(error.contains("Unsupported authentication mode"));
            assert!(!error.contains("private-credential"));
            value.as_object_mut().unwrap().remove(field);
        }
    }

    #[test]
    fn explicit_api_key_mode_requires_an_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({
                "auth_mode": "apikey",
                "tokens": { "account_id": "oauth-account", "access_token": "oauth-access" }
            })
            .to_string(),
        )
        .unwrap();

        let error = read_auth_file(&path).unwrap_err();
        assert!(error.contains("API-key authentication has no OPENAI_API_KEY"));
    }

    #[test]
    fn read_tokens_refuses_non_file_store_modes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth_path = dir.path().join("auth.json");
        let auth = serde_json::json!({
            "tokens": {"account_id": "acct", "access_token": "acc"}
        });
        fs::write(&auth_path, serde_json::to_string(&auth).unwrap()).unwrap();

        for mode in ["keyring", "auto", "ephemeral"] {
            fs::write(
                dir.path().join("config.toml"),
                format!("cli_auth_credentials_store = \"{mode}\"\n"),
            )
            .unwrap();
            let err = read_tokens(&auth_path).unwrap_err();
            assert!(err.contains(mode));
            assert!(err.contains("file-backed auth"));
        }
    }

    #[test]
    fn read_tokens_allows_file_store_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth_path = dir.path().join("auth.json");
        let auth = serde_json::json!({
            "tokens": {"account_id": "acct", "access_token": "acc"}
        });
        fs::write(&auth_path, serde_json::to_string(&auth).unwrap()).unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();

        let tokens = read_tokens(&auth_path).unwrap();
        assert_eq!(tokens.account_id.as_deref(), Some("acct"));
        assert_eq!(tokens.access_token.as_deref(), Some("acc"));
    }

    #[test]
    fn read_tokens_opt_handles_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("none.json");
        assert!(read_tokens_opt(&path).is_none());

        let invalid = dir.path().join("invalid.json");
        fs::write(&invalid, "not json").unwrap();
        assert!(read_tokens_opt(&invalid).is_none());
    }

    #[test]
    fn has_auth_distinguishes_ready_and_incomplete_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({
                "tokens": {
                    "account_id": "acct",
                    "id_token": build_id_token("ready@example.com", "pro"),
                    "access_token": "access"
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(has_auth(&path));

        fs::write(
            &path,
            serde_json::json!({
                "tokens": { "account_id": "acct", "access_token": "access" }
            })
            .to_string(),
        )
        .unwrap();
        assert!(!has_auth(&path));
    }

    #[test]
    fn api_key_helpers() {
        let tokens = tokens_from_api_key("sk-test-1234");
        assert!(is_api_key_profile(&tokens));
        let display = api_key_display_label(&tokens).unwrap();
        assert!(display.starts_with(API_KEY_SEPARATOR));
        assert_eq!(api_key_prefix("abc$123"), "abc-123".to_string());
    }

    #[test]
    fn auth_helper_empty_inputs_are_safe() {
        assert_eq!(AuthStoreMode::File.as_str(), "file");
        assert_eq!(normalize_plan_type(""), "unknown");
        assert_eq!(title_case(""), "");

        let empty_prefix = Tokens {
            account_id: Some("api-key-~suffix".to_string()),
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        assert!(api_key_display_label(&empty_prefix).is_none());

        let empty_suffix = Tokens {
            account_id: Some("api-key-prefix~".to_string()),
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        assert!(api_key_display_label(&empty_suffix).is_none());

        let malformed_api_key = Tokens {
            account_id: Some("api-key-invalid".to_string()),
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        let (email, plan) = extract_email_and_plan(&malformed_api_key);
        assert_eq!(email.as_deref(), Some(API_KEY_LABEL));
        assert_eq!(plan.as_deref(), Some(API_KEY_LABEL));
    }

    #[test]
    fn format_plan_and_free() {
        assert_eq!(format_plan("chatgpt_plus"), "Chatgpt Plus");
        assert_eq!(format_plan(""), "Unknown");
        assert!(is_free_plan(Some("free")));
        assert!(!is_free_plan(Some("pro")));
    }

    #[test]
    fn extract_email_and_plan_paths() {
        let id_token = build_id_token("me@example.com", "pro");
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        let (email, plan) = extract_email_and_plan(&tokens);
        assert_eq!(email.as_deref(), Some("me@example.com"));
        assert_eq!(plan.as_deref(), Some("Pro"));

        let api_tokens = tokens_from_api_key("sk-test");
        let (email, plan) = extract_email_and_plan(&api_tokens);
        assert_eq!(plan.as_deref(), Some(API_KEY_LABEL));
        assert!(email.is_some());
    }

    #[test]
    fn extract_profile_identity_prefers_user_and_workspace_claims() {
        let id_token = build_id_token_payload(
            "{\"email\":\"me@example.com\",\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"team\",\"chatgpt_user_id\":\"user-123\",\"chatgpt_account_id\":\"ws-123\"}}",
        );
        let tokens = Tokens {
            account_id: Some("acct-fallback".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.principal_id, "user-123");
        assert_eq!(identity.workspace_or_org_id, "ws-123");
        assert_eq!(identity.plan_type, "team");
    }

    #[test]
    fn extract_profile_identity_falls_back_to_sub_and_org() {
        let id_token = build_id_token_payload(
            "{\"sub\":\"sub-1\",\"organization_id\":\"org-1\",\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"Pro\"}}",
        );
        let tokens = Tokens {
            account_id: None,
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.principal_id, "sub-1");
        assert_eq!(identity.workspace_or_org_id, "org-1");
        assert_eq!(identity.plan_type, "pro");

        let id_token = build_id_token_payload(
            r#"{"sub":"sub-2","project_id":"project-2","https://api.openai.com/auth":{"chatgpt_plan_type":"Pro"}}"#,
        );
        let tokens = Tokens {
            account_id: None,
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.workspace_or_org_id, "project-2");

        let id_token = build_id_token_payload(r#"{"sub":"sub-3","email":"three@example.com"}"#);
        let tokens = Tokens {
            account_id: None,
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.workspace_or_org_id, "unknown");
    }

    #[test]
    fn extract_profile_identity_uses_account_fallback_when_claims_missing() {
        let tokens = Tokens {
            account_id: Some("acct-only".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.principal_id, "acct-only");
        assert_eq!(identity.workspace_or_org_id, "acct-only");
        assert_eq!(identity.plan_type, "pro");

        let id_token = build_id_token_payload(r#"{"email":"me@example.com"}"#);
        let tokens = Tokens {
            account_id: Some("acct-only".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        };
        let identity = extract_profile_identity(&tokens).unwrap();
        assert_eq!(identity.plan_type, "unknown");
    }

    #[test]
    fn extract_profile_identity_supports_api_key_profiles() {
        let identity = extract_profile_identity(&tokens_from_api_key("sk-test")).unwrap();
        assert_eq!(identity.plan_type, "key");
        assert_eq!(identity.principal_id, identity.workspace_or_org_id);
    }

    #[test]
    fn require_identity_errors() {
        let tokens = Tokens {
            account_id: None,
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        let err = require_identity(&tokens).unwrap_err();
        assert!(err.contains("missing account"));
    }

    #[test]
    fn profile_error_variants() {
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        assert_eq!(
            profile_error(&tokens, Some("e"), Some("p")),
            Some(crate::AUTH_ERR_PROFILE_MISSING_ACCESS_TOKEN)
        );

        let api_tokens = tokens_from_api_key("sk-test");
        assert!(profile_error(&api_tokens, None, None).is_none());

        let tokens = Tokens {
            account_id: None,
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert_eq!(
            profile_error(&tokens, Some("me@example.com"), Some("Pro")),
            Some(crate::AUTH_ERR_PROFILE_MISSING_ACCOUNT)
        );

        let id_token = build_id_token_payload(
            "{\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"pro\"}}",
        );
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert_eq!(
            profile_error(&tokens, None, Some("Pro")),
            Some(crate::AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN)
        );

        let complete = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert!(profile_error(&complete, Some("me@example.com"), Some("Pro")).is_none());
    }

    #[test]
    fn is_profile_ready_variants() {
        let api_tokens = tokens_from_api_key("sk-test");
        assert!(is_profile_ready(&api_tokens));

        let tokens = Tokens {
            account_id: None,
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert!(!is_profile_ready(&tokens));

        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: None,
            refresh_token: None,
        };
        assert!(!is_profile_ready(&tokens));

        let id_token = build_id_token_payload("{\"email\":\"me@example.com\"}");
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert!(!is_profile_ready(&tokens));
    }

    #[test]
    fn require_identity_missing_fields() {
        let id_token = build_id_token_payload("{\"email\":\"me@example.com\"}");
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        let err = require_identity(&tokens).unwrap_err();
        assert!(err.contains("missing plan"));

        let id_token = build_id_token_payload(
            "{\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"pro\"}}",
        );
        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(id_token),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        let err = require_identity(&tokens).unwrap_err();
        assert!(err.contains("missing email"));

        let tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("acc".to_string()),
            refresh_token: None,
        };
        assert!(require_identity(&tokens).is_ok());
    }

    #[test]
    fn refresh_profile_tokens_missing_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let value = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "acc"
            }
        });
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let mut tokens = read_tokens(&path).unwrap();
        let err = refresh_profile_tokens(&path, &mut tokens).unwrap_err();
        assert!(err.contains("refresh token"));
    }

    #[test]
    fn refresh_profile_tokens_refuses_disk_mismatch() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let _env = set_env_guard(
            REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            Some("http://127.0.0.1:9"),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old-access",
                "refresh_token": "rt"
            }
        });
        fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();
        let mut tokens = read_tokens(&path).unwrap();

        let drifted = serde_json::json!({
            "tokens": {
                "account_id": "other-account",
                "access_token": "disk-access",
                "refresh_token": "disk-refresh"
            }
        });
        fs::write(&path, serde_json::to_string(&drifted).unwrap()).unwrap();

        let err = refresh_profile_tokens(&path, &mut tokens).unwrap_err();
        assert!(err.contains("changed on disk"));
        assert_eq!(tokens.account_id.as_deref(), Some("acct"));
        assert_eq!(tokens.access_token.as_deref(), Some("old-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));

        let stored = fs::read_to_string(&path).unwrap();
        assert!(stored.contains("other-account"));
        assert!(stored.contains("disk-access"));
        assert!(stored.contains("disk-refresh"));
    }

    #[test]
    fn refresh_profile_tokens_reuses_rotated_disk_tokens_for_same_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "id_token": build_id_token_payload(
                    "{\"sub\":\"user-1\",\"email\":\"same@example.com\",\"organization_id\":\"org-1\",\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"pro\",\"chatgpt_account_id\":\"acct\"}}"
                )
            }
        });
        fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();
        let mut tokens = read_tokens(&path).unwrap();

        let rotated = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "id_token": build_id_token_payload(
                    "{\"sub\":\"user-1\",\"email\":\"same@example.com\",\"organization_id\":\"org-1\",\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"pro\",\"chatgpt_account_id\":\"acct\"}}"
                )
            }
        });
        fs::write(&path, serde_json::to_string(&rotated).unwrap()).unwrap();

        let rotated_contents = fs::read_to_string(&path).unwrap();
        let rotated_tokens = read_tokens(&path).unwrap();
        assert!(same_auth_document_context(
            &serde_json::to_string(&initial).unwrap(),
            &rotated_contents
        ));
        assert!(same_profile_refresh_target(&rotated_tokens, &tokens));

        refresh_profile_tokens(&path, &mut tokens).unwrap();
        assert_eq!(tokens.account_id.as_deref(), Some("acct"));
        assert_eq!(tokens.access_token.as_deref(), Some("new-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn refresh_profile_tokens_rejects_rotated_disk_tokens_when_identity_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "   ",
                "access_token": "old-access",
                "refresh_token": "old-refresh"
            }
        });
        fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();
        let mut tokens = read_tokens(&path).unwrap();

        let rotated = serde_json::json!({
            "tokens": {
                "account_id": "   ",
                "access_token": "new-access",
                "refresh_token": "new-refresh"
            }
        });
        fs::write(&path, serde_json::to_string(&rotated).unwrap()).unwrap();

        let err = refresh_profile_tokens(&path, &mut tokens).unwrap_err();
        assert!(err.contains("changed on disk"));
        assert_eq!(tokens.account_id.as_deref(), Some("   "));
        assert_eq!(tokens.access_token.as_deref(), Some("old-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
    }

    #[test]
    fn set_env_clears_value() {
        let _guard = ENV_MUTEX.lock().unwrap();
        {
            let _env = set_env_guard("CODEX_PROFILES_TEST_ENV", Some("value"));
        }
        {
            let _env = set_env_guard("CODEX_PROFILES_TEST_ENV", None);
        }
    }

    #[test]
    fn decode_id_token_claims_handles_invalid() {
        assert!(decode_id_token_claims("not-a-jwt").is_none());
        let bad = "a.b.c";
        assert!(decode_id_token_claims(bad).is_none());
        let good = build_id_token("me@example.com", "pro");
        assert!(decode_id_token_claims(&good).is_some());
    }

    #[test]
    fn apply_refresh_requires_access_token() {
        let mut tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        let refreshed = RefreshResponse {
            id_token: None,
            access_token: None,
            refresh_token: None,
        };
        let err = apply_refresh(&mut tokens, &refreshed).unwrap_err();
        assert!(err.contains("missing an access token"));
    }

    #[test]
    fn apply_refresh_preserves_account_id_when_id_token_claim_changes() {
        let mut tokens = Tokens {
            account_id: Some("acct-old".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("old-access".to_string()),
            refresh_token: Some("old-refresh".to_string()),
        };
        let refreshed_id_token = build_id_token_payload(
            "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"ws-new\",\"chatgpt_plan_type\":\"pro\"}}",
        );
        let refreshed = RefreshResponse {
            id_token: Some(refreshed_id_token),
            access_token: Some("new-access".to_string()),
            refresh_token: Some("new-refresh".to_string()),
        };
        apply_refresh(&mut tokens, &refreshed).unwrap();
        assert_eq!(tokens.account_id.as_deref(), Some("acct-old"));
        assert_eq!(tokens.access_token.as_deref(), Some("new-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn apply_refresh_preserves_optional_fields_when_response_omits_them() {
        let mut tokens = Tokens {
            account_id: Some("acct-old".to_string()),
            id_token: Some(build_id_token("me@example.com", "pro")),
            access_token: Some("old-access".to_string()),
            refresh_token: Some("old-refresh".to_string()),
        };
        apply_refresh(
            &mut tokens,
            &RefreshResponse {
                id_token: None,
                access_token: Some("new-access".to_string()),
                refresh_token: None,
            },
        )
        .unwrap();
        assert_eq!(tokens.access_token.as_deref(), Some("new-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        assert_eq!(tokens.account_id.as_deref(), Some("acct-old"));
    }

    #[test]
    fn refresh_response_rejects_empty_tokens() {
        let empty_access = RefreshResponse {
            id_token: None,
            access_token: Some("  ".to_string()),
            refresh_token: None,
        };
        assert!(
            validate_refresh_response(&empty_access)
                .unwrap_err()
                .contains("access_token is empty")
        );

        let empty_refresh = RefreshResponse {
            id_token: None,
            access_token: Some("new-access".to_string()),
            refresh_token: Some(String::new()),
        };
        assert!(
            validate_refresh_response(&empty_refresh)
                .unwrap_err()
                .contains("refresh_token is empty")
        );

        let mut tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: None,
            access_token: Some("old-access".to_string()),
            refresh_token: Some("old-refresh".to_string()),
        };
        assert!(
            apply_refresh(&mut tokens, &empty_access)
                .unwrap_err()
                .contains("missing an access token")
        );
    }

    #[test]
    fn update_auth_tokens_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.json");
        let err = update_auth_tokens(
            &missing,
            "",
            &RefreshResponse {
                id_token: None,
                access_token: None,
                refresh_token: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("Could not read"));

        let bad = dir.path().join("bad.json");
        fs::write(&bad, "{oops").unwrap();
        let err = update_auth_tokens(
            &bad,
            "",
            &RefreshResponse {
                id_token: None,
                access_token: None,
                refresh_token: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("Invalid JSON"));

        let not_obj = dir.path().join("not_obj.json");
        fs::write(&not_obj, "[]").unwrap();
        let err = update_auth_tokens(
            &not_obj,
            "",
            &RefreshResponse {
                id_token: None,
                access_token: None,
                refresh_token: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("expected object"));

        let tokens_not_obj = dir.path().join("tokens_not_obj.json");
        fs::write(&tokens_not_obj, "{\"tokens\": []}").unwrap();
        let err = update_auth_tokens(
            &tokens_not_obj,
            "",
            &RefreshResponse {
                id_token: None,
                access_token: None,
                refresh_token: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("Invalid tokens"));

        let missing_tokens = dir.path().join("missing_tokens.json");
        fs::write(&missing_tokens, "{}").unwrap();
        let original = fs::read_to_string(&missing_tokens).unwrap();
        update_auth_tokens(
            &missing_tokens,
            &original,
            &RefreshResponse {
                id_token: None,
                access_token: Some("new-access".to_string()),
                refresh_token: None,
            },
        )
        .unwrap();
        assert_eq!(
            read_tokens(&missing_tokens)
                .unwrap()
                .access_token
                .as_deref(),
            Some("new-access")
        );

        assert!(parse_auth_store_mode("unsupported").is_err());
    }

    #[test]
    fn update_auth_tokens_reports_atomic_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let value = serde_json::json!({
            "tokens": {"account_id": "acct", "access_token": "old"}
        });
        let original = serde_json::to_string(&value).unwrap();
        fs::write(&path, &original).unwrap();
        let _failpoint = FailpointGuard::new(FAIL_WRITE_OPEN, 1);
        let error = update_auth_tokens(
            &path,
            &original,
            &RefreshResponse {
                id_token: None,
                access_token: Some("new-access".to_string()),
                refresh_token: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("Could not write"), "{error}");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn update_auth_tokens_preserves_file_when_compare_and_swap_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let value = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old-access",
                "refresh_token": "old-refresh"
            }
        });
        let original = serde_json::to_string(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let error = update_auth_tokens(
            &path,
            "stale snapshot",
            &RefreshResponse {
                id_token: None,
                access_token: Some("new-access".to_string()),
                refresh_token: Some("new-refresh".to_string()),
            },
        )
        .unwrap_err();
        assert_eq!(error, AUTH_ERR_REFRESH_STATE_CHANGED);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn update_auth_tokens_preserves_account_id_when_id_token_claim_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let value = serde_json::json!({
            "tokens": {
                "account_id": "acct-old",
                "access_token": "old-access",
            }
        });
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let refreshed_id_token = build_id_token_payload(
            "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"ws-fresh\",\"chatgpt_plan_type\":\"pro\"}}",
        );
        let original = serde_json::to_string(&value).unwrap();
        update_auth_tokens(
            &path,
            &original,
            &RefreshResponse {
                id_token: Some(refreshed_id_token),
                access_token: Some("new-access".to_string()),
                refresh_token: Some("new-refresh".to_string()),
            },
        )
        .unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains("\"account_id\": \"acct-old\""));
        assert!(!updated.contains("\"account_id\": \"ws-fresh\""));
        assert!(updated.contains("\"access_token\": \"new-access\""));
        let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(updated["last_refresh"].as_str().unwrap()).is_ok()
        );
    }

    #[test]
    fn refresh_request_matches_codex_wire_contract() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let _guard = ENV_MUTEX.lock().unwrap();
        for client_id in [None, Some(""), Some("custom-client")] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                stream
                    .write_all(
                        http_ok_response(r#"{"access_token":"new"}"#, "application/json")
                            .as_bytes(),
                    )
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()
            });
            let _endpoint = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));
            let _client = set_env_guard(CLIENT_ID_OVERRIDE_ENV_VAR, client_id);
            refresh_access_token("synthetic-refresh-token").unwrap();
            assert_eq!(
                server.join().unwrap(),
                serde_json::json!({
                    "client_id": client_id.filter(|value| !value.is_empty()).unwrap_or(CLIENT_ID),
                    "grant_type": "refresh_token",
                    "refresh_token": "synthetic-refresh-token"
                })
            );
        }
    }

    #[test]
    fn refresh_access_token_success_and_status() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let ok_body = "{\"access_token\":\"acc\",\"id_token\":\"id\",\"refresh_token\":\"ref\"}";
        let ok_resp = http_ok_response(ok_body, "application/json");
        let ok_url = spawn_server(ok_resp);
        {
            let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&ok_url));
            let refreshed = refresh_access_token("token").unwrap();
            assert_eq!(refreshed.access_token.as_deref(), Some("acc"));
        }

        let err_resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_string();
        let err_url = spawn_server(err_resp);
        {
            let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&err_url));
            let err = refresh_access_token("token").unwrap_err();
            assert!(err.contains("Unknown error\nunexpected status 401 Unauthorized"));
            assert!(err.contains("\nURL: http://"));
        }

        let expired_body = r#"{"error":{"code":"refresh_token_expired"}}"#;
        let expired_resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            expired_body.len(),
            expired_body
        );
        let expired_url = spawn_server(expired_resp);
        {
            let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&expired_url));
            let err = refresh_access_token("token").unwrap_err();
            assert!(err.contains("unexpected status 401 Unauthorized"));
            assert!(err.contains("refresh_token_expired"));
        }

        let reused_body = r#"{"error":{"code":"refresh_token_reused"}}"#;
        let reused_resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            reused_body.len(),
            reused_body
        );
        let reused_url = spawn_server(reused_resp);
        {
            let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&reused_url));
            let err = refresh_access_token("token").unwrap_err();
            assert!(err.contains("unexpected status 401 Unauthorized"));
            assert!(err.contains("refresh_token_reused"));
        }

        let revoked_body = r#"{"error":{"code":"refresh_token_invalidated"}}"#;
        let revoked_resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            revoked_body.len(),
            revoked_body
        );
        let revoked_url = spawn_server(revoked_resp);
        {
            let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&revoked_url));
            let err = refresh_access_token("token").unwrap_err();
            assert!(err.contains("unexpected status 401 Unauthorized"));
            assert!(err.contains("refresh_token_invalidated"));
        }
    }

    #[test]
    fn refresh_access_token_rejects_a_truncated_success_body() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let response =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n";
        let url = spawn_server(response.to_string());
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&url));
        let error = refresh_access_token("token").unwrap_err();
        assert!(error.contains("Invalid refresh response"), "{error}");
    }

    #[test]
    fn refresh_access_token_reports_transport_errors() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let _env = set_env_guard(
            REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            Some("http://127.0.0.1:9"),
        );
        let error = refresh_access_token("token").unwrap_err();
        assert!(error.contains("Token refresh failed"), "{error}");
    }

    #[test]
    fn refresh_token_url_uses_the_official_default_without_an_override() {
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, None);
        assert_eq!(refresh_token_url(), REFRESH_TOKEN_URL);
    }

    #[test]
    fn refresh_profile_tokens_updates_file() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let ok_body = "{\"access_token\":\"acc\",\"id_token\":\"id\",\"refresh_token\":\"ref\"}";
        let ok_resp = http_ok_response(ok_body, "application/json");
        let ok_url = spawn_server(ok_resp);
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&ok_url));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let value = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old",
                "refresh_token": "rt"
            }
        });
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let mut tokens = read_tokens(&path).unwrap();
        refresh_profile_tokens(&path, &mut tokens).unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains("acc"));
    }

    #[test]
    fn refresh_profile_tokens_reports_initial_and_post_request_read_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let mut tokens = Tokens {
            account_id: Some("acct".to_string()),
            id_token: None,
            access_token: Some("old".to_string()),
            refresh_token: Some("refresh".to_string()),
        };
        let error = refresh_profile_tokens(&missing, &mut tokens).unwrap_err();
        assert!(error.contains("Could not read"));

        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let _guard = ENV_MUTEX.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("refresh release signal");
            stream
                .write_all(
                    http_ok_response(r#"{"access_token":"new"}"#, "application/json").as_bytes(),
                )
                .unwrap();
        });
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old",
                "refresh_token": "refresh"
            }
        });
        fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        let path_for_refresh = path.clone();
        let refresh = std::thread::spawn(move || {
            let mut tokens = read_tokens(&path_for_refresh).unwrap();
            refresh_profile_tokens(&path_for_refresh, &mut tokens)
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("refresh request");
        fs::remove_file(&path).unwrap();
        release_tx.send(()).unwrap();
        let error = refresh.join().unwrap().unwrap_err();
        assert!(error.contains("Could not read"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn refresh_profile_tokens_rejects_empty_optional_token_without_mutation() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let response = http_ok_response(
            r#"{"access_token":"new-access","id_token":""}"#,
            "application/json",
        );
        let endpoint = spawn_server(response);
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "access_token": "old-access",
                "refresh_token": "refresh-token"
            }
        });
        let initial_bytes = serde_json::to_vec(&initial).unwrap();
        fs::write(&path, &initial_bytes).unwrap();
        let mut tokens = read_tokens(&path).unwrap();

        let error = refresh_profile_tokens(&path, &mut tokens).unwrap_err();
        assert!(error.contains("id_token is empty"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), initial_bytes);
        assert_eq!(tokens.access_token.as_deref(), Some("old-access"));
    }

    #[test]
    fn refresh_profile_tokens_rejects_state_changed_after_request() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let _guard = ENV_MUTEX.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("refresh release signal");
            stream
                .write_all(
                    http_ok_response(
                        r#"{"access_token":"new-access","refresh_token":"new-refresh"}"#,
                        "application/json",
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct-a",
                "access_token": "old-access",
                "refresh_token": "refresh-a"
            }
        });
        fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();
        let path_for_refresh = path.clone();
        let refresh = std::thread::spawn(move || {
            let mut tokens = read_tokens(&path_for_refresh).unwrap();
            refresh_profile_tokens(&path_for_refresh, &mut tokens)
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("refresh request");
        let replacement = serde_json::json!({
            "tokens": {
                "account_id": "acct-b",
                "access_token": "current-access",
                "refresh_token": "refresh-b"
            }
        });
        fs::write(&path, serde_json::to_string(&replacement).unwrap()).unwrap();
        release_tx.send(()).unwrap();

        let error = refresh.join().unwrap().unwrap_err();
        assert!(error.contains("changed on disk"));
        assert_eq!(
            read_tokens(&path).unwrap().account_id.as_deref(),
            Some("acct-b")
        );
        server.join().unwrap();
    }

    #[test]
    fn refresh_profile_tokens_accepts_same_profile_rotation_after_request() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let _guard = ENV_MUTEX.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("refresh release signal");
            stream
                .write_all(
                    http_ok_response(r#"{"access_token":"network-access"}"#, "application/json")
                        .as_bytes(),
                )
                .unwrap();
        });
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let id_token = build_id_token_payload(
            r#"{"sub":"user-1","email":"same@example.com","organization_id":"org-1","https://api.openai.com/auth":{"chatgpt_plan_type":"pro","chatgpt_account_id":"acct"}}"#,
        );
        let initial = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "id_token": id_token,
                "access_token": "old-access",
                "refresh_token": "old-refresh"
            }
        });
        fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        let path_for_refresh = path.clone();
        let refresh = std::thread::spawn(move || {
            let mut tokens = read_tokens(&path_for_refresh).unwrap();
            refresh_profile_tokens(&path_for_refresh, &mut tokens).map(|_| tokens)
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("refresh request");
        let replacement = serde_json::json!({
            "tokens": {
                "account_id": "acct",
                "id_token": initial["tokens"]["id_token"],
                "access_token": "disk-access",
                "refresh_token": "disk-refresh"
            }
        });
        fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
        release_tx.send(()).unwrap();

        let tokens = refresh.join().unwrap().unwrap();
        assert_eq!(tokens.access_token.as_deref(), Some("disk-access"));
        assert_eq!(tokens.refresh_token.as_deref(), Some("disk-refresh"));
        assert_eq!(read_tokens(&path).unwrap(), tokens);
        server.join().unwrap();
    }

    #[test]
    fn refresh_profile_tokens_rejects_same_tokens_when_auth_mode_changes() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let _guard = ENV_MUTEX.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("refresh release signal");
            stream
                .write_all(
                    http_ok_response(r#"{"access_token":"new-access"}"#, "application/json")
                        .as_bytes(),
                )
                .unwrap();
        });
        let _env = set_env_guard(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&endpoint));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let initial = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "account_id": "acct-a",
                "access_token": "old-access",
                "refresh_token": "refresh-a"
            }
        });
        fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        let initial_bytes = fs::read(&path).unwrap();
        let path_for_refresh = path.clone();
        let refresh = std::thread::spawn(move || {
            let mut tokens = read_tokens(&path_for_refresh).unwrap();
            refresh_profile_tokens(&path_for_refresh, &mut tokens)
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("refresh request");
        let replacement = serde_json::json!({
            "auth_mode": null,
            "tokens": {
                "account_id": "acct-a",
                "access_token": "old-access",
                "refresh_token": "refresh-a"
            }
        });
        fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
        release_tx.send(()).unwrap();

        let error = refresh.join().unwrap().unwrap_err();
        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(
            fs::read(&path).unwrap(),
            serde_json::to_vec(&replacement).unwrap()
        );
        assert_ne!(fs::read(&path).unwrap(), initial_bytes);
        server.join().unwrap();
    }
}
