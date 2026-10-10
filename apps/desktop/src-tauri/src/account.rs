use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Local, Utc};
use std::{fs, sync::Mutex, time::Duration};

use crate::{
    models::AccountStatus,
    paths::auth_file_path,
    state::{AccountCache, RefreshMode, Store},
    storage,
};

const SUBSCRIPTION_CHECK_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const SUBSCRIPTIONS_URL: &str = "https://chatgpt.com/backend-api/subscriptions";
const ACCOUNT_CACHE_MAX_AGE_SECS: i64 = 30 * 60;

pub(crate) fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let part = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(part).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn json_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn find_key(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(found) = json_string(object.get(*key)) {
                return Some(found);
            }
        }
        for child in object.values() {
            if let Some(found) = find_key(child, keys) {
                return Some(found);
            }
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            if let Some(found) = find_key(child, keys) {
                return Some(found);
            }
        }
    }
    None
}

pub(crate) fn subscription_state(date: Option<&str>) -> String {
    let Some(raw) = date else {
        return "unknown".into();
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc));
    let Some(parsed) = parsed else {
        return "unknown".into();
    };
    let diff = parsed.signed_duration_since(Utc::now());
    if diff.num_seconds() <= 0 {
        "expired".into()
    } else if diff.num_hours() < 24 {
        "within_24h".into()
    } else if diff.num_days() < 7 {
        "within_7d".into()
    } else if diff.num_days() < 30 {
        "within_30d".into()
    } else {
        "active".into()
    }
}

fn normalize_plan_type(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty()
        || DateTime::parse_from_rfc3339(value).is_ok()
        || chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
    {
        None
    } else {
        Some(value.to_string())
    }
}

fn normalize_subscription_date(raw: &str) -> Option<String> {
    let value = raw.trim();
    if DateTime::parse_from_rfc3339(value).is_ok() {
        Some(value.to_string())
    } else {
        chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .map(|date| format!("{date}T00:00:00Z"))
    }
}

pub(crate) fn auth_plan_type(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| json_string(auth.get("chatgpt_plan_type")))
        .and_then(|plan| normalize_plan_type(&plan))
}

pub(crate) fn auth_subscription_active_until(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| json_string(auth.get("chatgpt_subscription_active_until")))
        .and_then(|date| normalize_subscription_date(&date))
}

fn auth_identity() -> Result<(String, Option<String>, Option<String>, Option<String>), String> {
    let raw = fs::read_to_string(auth_file_path())
        .map_err(|error| format!("读取 Codex 登录文件失败: {error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("解析 Codex 登录文件失败: {error}"))?;
    let tokens = value.get("tokens").ok_or("Codex 登录文件没有 tokens")?;
    let access =
        json_string(tokens.get("access_token")).ok_or("Codex 登录文件没有 access_token")?;
    let id_token = json_string(tokens.get("id_token")).unwrap_or_default();
    let access_payload = decode_jwt_payload(&access);
    let payload = decode_jwt_payload(&id_token)
        .or_else(|| access_payload.clone())
        .unwrap_or(serde_json::Value::Null);
    let auth = payload.get("https://api.openai.com/auth");
    let account_id = json_string(tokens.get("account_id"))
        .or_else(|| auth.and_then(|value| json_string(value.get("account_id"))))
        .or_else(|| {
            access_payload
                .as_ref()
                .and_then(|value| value.get("https://api.openai.com/auth"))
                .and_then(|value| json_string(value.get("account_id")))
        });
    let plan = auth_plan_type(&payload);
    let active_until = auth_subscription_active_until(&payload);
    Ok((access, account_id, plan, active_until))
}

fn subscription_request(
    client: &reqwest::blocking::Client,
    url: &str,
    access: &str,
    account_id: Option<&str>,
    query: Option<(&str, &str)>,
) -> Result<serde_json::Value, String> {
    let mut request = client
        .get(url)
        .bearer_auth(access)
        .header("Accept", "application/json")
        .header("Referer", "https://chatgpt.com/")
        .header("User-Agent", "Mozilla/5.0 Codex Float")
        .header(
            "x-openai-target-path",
            url.trim_start_matches("https://chatgpt.com"),
        )
        .header(
            "x-openai-target-route",
            url.trim_start_matches("https://chatgpt.com"),
        );
    if let Some(id) = account_id.filter(|id| !id.is_empty()) {
        request = request.header("ChatGPT-Account-Id", id);
    }
    if let Some((key, value)) = query {
        request = request.query(&[(key, value)]);
    }
    let response = request
        .send()
        .map_err(|error| format!("订阅接口请求失败: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("读取订阅接口响应失败: {error}"))?;
    if !status.is_success() {
        return Err(format!("订阅接口返回 HTTP {status}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("订阅接口 JSON 无效: {error}"))
}

fn cached_account_status(
    cache: &Mutex<Option<AccountCache>>,
    cache_key: &str,
    mode: RefreshMode,
    request_started: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<AccountStatus> {
    let slot = cache.lock().ok()?;
    let cached = slot.as_ref()?;
    if cached.cache_key != cache_key {
        return None;
    }
    let use_cached = match mode {
        RefreshMode::Cached => {
            let age = (now - cached.fetched_at).num_seconds();
            (0..ACCOUNT_CACHE_MAX_AGE_SECS).contains(&age)
        }
        RefreshMode::Force => cached.fetched_at >= request_started,
    };
    use_cached.then(|| cached.status.clone())
}

fn account_status_fallback(
    cache: &Mutex<Option<AccountCache>>,
    cache_key: Option<&str>,
    bound: bool,
    error: String,
) -> AccountStatus {
    if let Ok(slot) = cache.lock() {
        if let Some(cached) = slot
            .as_ref()
            .filter(|cached| cache_key.map(|key| cached.cache_key == key).unwrap_or(true))
        {
            let mut status = cached.status.clone();
            status.subscription_error = Some(error);
            return status;
        }
    }
    AccountStatus {
        bound,
        subscription_state: "unknown".into(),
        subscription_error: Some(error),
        checked_at: Some(Local::now().to_rfc3339()),
        ..Default::default()
    }
}

pub(crate) fn read_account_status_with<F>(
    cache: &Mutex<Option<AccountCache>>,
    refresh: &Mutex<()>,
    cache_key: &str,
    mode: RefreshMode,
    fetch: F,
) -> AccountStatus
where
    F: FnOnce() -> Result<AccountStatus, String>,
{
    let request_started = Local::now();
    if let Some(status) =
        cached_account_status(cache, cache_key, mode, request_started, Local::now())
    {
        return status;
    }
    let Ok(_refresh_guard) = refresh.lock() else {
        return account_status_fallback(cache, Some(cache_key), true, "账户状态刷新不可用".into());
    };
    if let Some(status) =
        cached_account_status(cache, cache_key, mode, request_started, Local::now())
    {
        return status;
    }
    match fetch() {
        Ok(status) => {
            if let Ok(mut slot) = cache.lock() {
                *slot = Some(AccountCache {
                    cache_key: cache_key.into(),
                    status: status.clone(),
                    fetched_at: Local::now(),
                });
            }
            status
        }
        Err(error) => account_status_fallback(cache, Some(cache_key), true, error),
    }
}

fn fetch_account_status(
    access: &str,
    account_id: Option<&str>,
    token_plan: Option<String>,
    token_active_until: Option<String>,
) -> Result<AccountStatus, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| format!("账户状态客户端初始化失败: {error}"))?;
    let mut status = AccountStatus {
        bound: true,
        plan_type: token_plan,
        subscription_active_until: token_active_until,
        subscription_state: "unknown".into(),
        subscription_error: None,
        checked_at: Some(Local::now().to_rfc3339()),
    };
    let timezone_offset_min = (Local::now().offset().local_minus_utc() / 60).to_string();
    if let Ok(value) = subscription_request(
        &client,
        SUBSCRIPTION_CHECK_URL,
        access,
        account_id,
        Some(("timezone_offset_min", &timezone_offset_min)),
    ) {
        if status.plan_type.is_none() {
            status.plan_type = find_key(&value, &["subscription_plan", "plan_type", "planType"])
                .and_then(|plan| normalize_plan_type(&plan));
        }
        if status.subscription_active_until.is_none() {
            status.subscription_active_until = find_key(
                &value,
                &["expires_at", "active_until", "subscription_active_until"],
            )
            .and_then(|date| normalize_subscription_date(&date));
        }
    }
    if status.subscription_active_until.is_none() {
        if let Some(id) = account_id.filter(|id| !id.is_empty()) {
            if let Ok(value) = subscription_request(
                &client,
                SUBSCRIPTIONS_URL,
                access,
                Some(id),
                Some(("account_id", id)),
            ) {
                if status.plan_type.is_none() {
                    status.plan_type = find_key(&value, &["subscription_plan", "plan_type"])
                        .and_then(|plan| normalize_plan_type(&plan));
                }
                status.subscription_active_until = find_key(
                    &value,
                    &["active_until", "expires_at", "subscription_active_until"],
                )
                .and_then(|date| normalize_subscription_date(&date));
            }
        }
    }
    status.subscription_state = subscription_state(status.subscription_active_until.as_deref());
    if status.subscription_active_until.is_none() {
        status.subscription_error = Some("官方接口未返回订阅到期时间".into());
    }
    Ok(status)
}

pub(crate) fn query_account_status(state: &Store, mode: RefreshMode) -> AccountStatus {
    let (access, account_id, token_plan, token_active_until) = match auth_identity() {
        Ok(identity) => identity,
        Err(error) => return account_status_fallback(&state.account_cache, None, false, error),
    };
    let cache_identity = account_id.as_deref().unwrap_or(&access);
    let cache_key = storage::digest(cache_identity);
    let fetch_account_id = account_id.clone();
    read_account_status_with(
        &state.account_cache,
        &state.account_refresh,
        &cache_key,
        mode,
        move || {
            fetch_account_status(
                &access,
                fetch_account_id.as_deref(),
                token_plan,
                token_active_until,
            )
        },
    )
}
