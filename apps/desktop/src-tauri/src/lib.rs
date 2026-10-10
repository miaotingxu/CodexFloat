use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Mutex},
    thread,
    time::Duration,
};

mod models;
mod paths;

pub use models::{AccountStatus, DailyPoint, RateWindow, Snapshot};

use crate::paths::{auth_file_path, sessions_path};

#[derive(Debug, Deserialize)]
struct TokenInfo {
    total_token_usage: Option<TotalUsage>,
}
#[derive(Debug, Deserialize)]
struct TotalUsage {
    total_tokens: Option<i64>,
}
#[derive(Debug, Deserialize)]
struct LinePayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<TokenInfo>,
}
#[derive(Debug, Deserialize)]
struct EventLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<LinePayload>,
}

#[derive(Debug, Clone)]
struct AccountCache {
    cache_key: String,
    status: AccountStatus,
    fetched_at: DateTime<Local>,
}

struct RateCache {
    windows: Vec<RateWindow>,
    fetched_at: DateTime<Local>,
}

#[derive(Default)]
struct RateRetry {
    last_attempt: Option<DateTime<Local>>,
    consecutive_failures: u32,
}

struct RateState {
    cache: Option<RateCache>,
    retry: RateRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshMode {
    Cached,
    Force,
}

#[derive(Debug, Clone)]
struct AccountUsageCache {
    daily: Vec<DailyPoint>,
    fetched_at: DateTime<Local>,
}

struct LocalTokenUsage {
    today: i64,
    last7: i64,
    last30: i64,
    daily: Vec<DailyPoint>,
    coverage: String,
}

struct TokenSnapshot {
    today: i64,
    last7: i64,
    last30: i64,
    daily: Vec<DailyPoint>,
    token_coverage: String,
    usage_source: String,
    today_available: bool,
}

pub struct Store {
    pub db: Mutex<Connection>,
    pub expanded: Mutex<bool>,
    pub click_through: Mutex<bool>,
    user_hidden: Mutex<bool>,
    rate_state: Mutex<RateState>,
    rate_refresh: Mutex<()>,
    account_cache: Mutex<Option<AccountCache>>,
    account_refresh: Mutex<()>,
    account_usage_cache: Mutex<Option<AccountUsageCache>>,
    account_usage_refresh: Mutex<()>,
}
impl Store {
    pub fn new(path: &Path) -> rusqlite::Result<Self> {
        let c = Connection::open(path)?;
        init(&c)?;
        let expanded = c
            .query_row("SELECT expanded FROM window_state WHERE id=1", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|value| value != 0)
            .unwrap_or(false);
        let rate_cache = load_rate_cache(&c);
        Ok(Self {
            db: Mutex::new(c),
            expanded: Mutex::new(expanded),
            click_through: Mutex::new(false),
            user_hidden: Mutex::new(false),
            rate_state: Mutex::new(RateState {
                cache: rate_cache,
                retry: RateRetry::default(),
            }),
            rate_refresh: Mutex::new(()),
            account_cache: Mutex::new(None),
            account_refresh: Mutex::new(()),
            account_usage_cache: Mutex::new(None),
            account_usage_refresh: Mutex::new(()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibilityAction {
    None,
    Hide,
    Show,
}

fn fullscreen_visibility_transition(
    is_fullscreen: bool,
    user_hidden: bool,
    hidden_by_fullscreen: bool,
) -> (bool, VisibilityAction) {
    if is_fullscreen && !user_hidden && !hidden_by_fullscreen {
        (true, VisibilityAction::Hide)
    } else if !is_fullscreen && hidden_by_fullscreen {
        if user_hidden {
            (false, VisibilityAction::None)
        } else {
            (false, VisibilityAction::Show)
        }
    } else {
        (hidden_by_fullscreen, VisibilityAction::None)
    }
}

fn init(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS cursors(file_id TEXT PRIMARY KEY,file_hash TEXT NOT NULL,offset INTEGER NOT NULL,last_total INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,file_id TEXT NOT NULL,event_offset INTEGER NOT NULL,total_tokens INTEGER NOT NULL,delta_tokens INTEGER NOT NULL,day TEXT NOT NULL); CREATE TABLE IF NOT EXISTS daily(day TEXT PRIMARY KEY,tokens INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS window_state(id INTEGER PRIMARY KEY CHECK(id=1),x INTEGER NOT NULL,y INTEGER NOT NULL,expanded INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS quota_cache(id INTEGER PRIMARY KEY CHECK(id=1),windows_json TEXT NOT NULL,fetched_at TEXT NOT NULL);")
}

fn load_rate_cache(c: &Connection) -> Option<RateCache> {
    let (windows_json, fetched_at): (String, String) = c
        .query_row(
            "SELECT windows_json,fetched_at FROM quota_cache WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let windows = serde_json::from_str(&windows_json).ok()?;
    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at)
        .ok()?
        .with_timezone(&Local);
    Some(RateCache {
        windows,
        fetched_at,
    })
}

fn save_rate_cache(c: &Connection, cache: &RateCache) -> rusqlite::Result<()> {
    let windows_json = serde_json::to_string(&cache.windows)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    c.execute(
        "INSERT INTO quota_cache(id,windows_json,fetched_at) VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET windows_json=excluded.windows_json,fetched_at=excluded.fetched_at",
        params![windows_json, cache.fetched_at.to_rfc3339()],
    )?;
    Ok(())
}

fn cleanup_old_daily(c: &Connection, keep_days: i64) -> rusqlite::Result<usize> {
    let cutoff = (Local::now().date_naive()
        - chrono::Days::new(keep_days.saturating_sub(1) as u64))
    .to_string();
    c.execute("DELETE FROM events WHERE day < ?", [&cutoff])?;
    c.execute("DELETE FROM daily WHERE day < ?", [cutoff])
}
fn clear_token_data(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("DELETE FROM events; DELETE FROM daily;")
}
fn digest(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}
fn file_id(path: &Path) -> String {
    digest(&path.to_string_lossy())
}
fn remove_file_events(c: &Connection, id: &str) -> rusqlite::Result<()> {
    let contributions = {
        let mut statement =
            c.prepare("SELECT day, SUM(delta_tokens) FROM events WHERE file_id=? GROUP BY day")?;
        let rows = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (day, tokens) in contributions {
        c.execute(
            "UPDATE daily SET tokens=MAX(0,tokens-?) WHERE day=?",
            params![tokens, day],
        )?;
        c.execute("DELETE FROM daily WHERE day=? AND tokens=0", [&day])?;
    }
    c.execute("DELETE FROM events WHERE file_id=?", [id])?;
    Ok(())
}
fn scan_file(c: &Connection, path: &Path) -> rusqlite::Result<usize> {
    let id = file_id(path);
    let current_len = match fs::metadata(path) {
        Ok(metadata) => metadata.len() as i64,
        Err(_) => return Ok(0),
    };
    let (_old_hash, old_offset, last_total): (Option<String>, i64, i64) = c
        .query_row(
            "SELECT file_hash,offset,last_total FROM cursors WHERE file_id=?",
            [&id],
            |r| Ok((Some(r.get(0)?), r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((None, 0, 0));
    let truncated = old_offset > current_len;
    if truncated {
        remove_file_events(c, &id)?;
    }
    let start = if truncated { 0 } else { old_offset };
    let f =
        fs::File::open(path).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut reader = BufReader::new(f);
    reader
        .seek_relative(start)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut offset = start;
    let mut previous = if start > 0 { last_total } else { 0 };
    let mut count = 0;
    loop {
        let at = offset;
        let mut line = Vec::new();
        let bytes = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        if bytes == 0 {
            break;
        }
        let complete_line = line.last() == Some(&b'\n');
        if complete_line {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        let parsed = serde_json::from_slice::<EventLine>(&line);
        if parsed.is_err() && !complete_line {
            break;
        }
        offset += bytes as i64;
        let Ok(ev) = parsed else {
            continue;
        };
        if ev.kind.as_deref() != Some("event_msg") {
            continue;
        }
        let event_day = ev
            .timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Local).date_naive().to_string())
            .unwrap_or_else(|| Local::now().date_naive().to_string());
        let Some(p) = ev.payload else { continue };
        if p.kind.as_deref() != Some("token_count") {
            continue;
        }
        let Some(info) = p.info else { continue };
        let Some(total) = info.total_token_usage.and_then(|x| x.total_tokens) else {
            continue;
        };
        let delta = if total == previous {
            0
        } else if total < previous {
            total.max(0)
        } else {
            (total - previous).max(0)
        };
        previous = total;
        let day = event_day;
        let eid = digest(&format!("{id}:{at}:{total}"));
        let inserted=c.execute("INSERT OR IGNORE INTO events(event_id,file_id,event_offset,total_tokens,delta_tokens,day) VALUES(?,?,?,?,?,?)",params![eid,id,at,total,delta,day])?;
        if inserted > 0 && delta > 0 {
            c.execute("INSERT INTO daily(day,tokens) VALUES(?,?) ON CONFLICT(day) DO UPDATE SET tokens=tokens+excluded.tokens",params![day,delta])?;
            count += 1;
        }
    }
    c.execute("INSERT INTO cursors(file_id,file_hash,offset,last_total) VALUES(?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET file_hash=excluded.file_hash,offset=excluded.offset,last_total=excluded.last_total",params![id,id,offset,previous])?;
    Ok(count)
}
pub fn scan_sessions(c: &Connection, root: &Path) -> rusqlite::Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    c.execute_batch("BEGIN IMMEDIATE")?;
    let mut n = 0;
    fn walk(c: &Connection, p: &Path, n: &mut usize) -> rusqlite::Result<()> {
        for e in
            fs::read_dir(p).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
        {
            let p = e
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                .path();
            if p.is_dir() {
                walk(c, &p, n)?
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                *n += scan_file(c, &p)?;
            }
        }
        Ok(())
    }
    let result = walk(c, root, &mut n).and_then(|()| {
        cleanup_old_daily(c, 30)?;
        Ok(n)
    });
    match result {
        Ok(count) => {
            c.execute_batch("COMMIT")?;
            Ok(count)
        }
        Err(error) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

const SUBSCRIPTION_CHECK_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const SUBSCRIPTIONS_URL: &str = "https://chatgpt.com/backend-api/subscriptions";
const ACCOUNT_CACHE_MAX_AGE_SECS: i64 = 30 * 60;
const ACCOUNT_USAGE_CACHE_MAX_AGE_SECS: i64 = 120;

fn token_coverage(root_exists: bool, scan_succeeded: bool) -> &'static str {
    if root_exists && scan_succeeded {
        "partial"
    } else {
        "unavailable"
    }
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
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

fn subscription_state(date: Option<&str>) -> String {
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

fn auth_plan_type(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| json_string(auth.get("chatgpt_plan_type")))
        .and_then(|plan| normalize_plan_type(&plan))
}

fn auth_subscription_active_until(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| json_string(auth.get("chatgpt_subscription_active_until")))
        .and_then(|date| normalize_subscription_date(&date))
}

fn auth_identity() -> Result<(String, Option<String>, Option<String>, Option<String>), String> {
    let raw = fs::read_to_string(auth_file_path())
        .map_err(|e| format!("读取 Codex 登录文件失败: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 Codex 登录文件失败: {e}"))?;
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
        .or_else(|| auth.and_then(|v| json_string(v.get("account_id"))))
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
        .map_err(|e| format!("订阅接口请求失败: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("读取订阅接口响应失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("订阅接口返回 HTTP {status}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("订阅接口 JSON 无效: {e}"))
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

fn read_account_status_with<F>(
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

fn query_account_status(
    cache: &Mutex<Option<AccountCache>>,
    refresh: &Mutex<()>,
    mode: RefreshMode,
) -> AccountStatus {
    let (access, account_id, token_plan, token_active_until) = match auth_identity() {
        Ok(identity) => identity,
        Err(error) => return account_status_fallback(cache, None, false, error),
    };
    let cache_identity = account_id.as_deref().unwrap_or(&access);
    let cache_key = digest(cache_identity);
    let fetch_account_id = account_id.clone();
    read_account_status_with(cache, refresh, &cache_key, mode, move || {
        fetch_account_status(
            &access,
            fetch_account_id.as_deref(),
            token_plan,
            token_active_until,
        )
    })
}

fn sums(c: &Connection, days: i64) -> rusqlite::Result<i64> {
    let since =
        (Local::now().date_naive() - chrono::Days::new(days.saturating_sub(1) as u64)).to_string();
    c.query_row(
        "SELECT COALESCE(SUM(tokens),0) FROM daily WHERE day>=?",
        [since],
        |r| r.get(0),
    )
}
fn series(c: &Connection) -> rusqlite::Result<Vec<DailyPoint>> {
    let today = Local::now().date_naive();
    (0..7)
        .rev()
        .map(|i| {
            let d = (today - chrono::Days::new(i)).to_string();
            c.query_row(
                "SELECT COALESCE(tokens,0) FROM daily WHERE day=?",
                [&d],
                |r| {
                    Ok(DailyPoint {
                        day: d.clone(),
                        tokens: r.get(0)?,
                    })
                },
            )
        })
        .collect()
}
fn recv_response(
    messages: &mpsc::Receiver<serde_json::Value>,
    id: i64,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let message = messages.recv_timeout(remaining).ok()?;
        if message.get("id").and_then(|value| value.as_i64()) == Some(id) {
            return Some(message);
        }
    }
}

fn first_existing_path(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_file())
}

fn parse_rate_limits_response(
    response: &serde_json::Value,
) -> Result<Vec<RateWindow>, &'static str> {
    if response.get("error").is_some() {
        return Err("protocol_error");
    }
    let rate_limits = response
        .get("result")
        .and_then(|value| value.get("rateLimits"))
        .and_then(serde_json::Value::as_object)
        .ok_or("invalid_response")?;
    let mut windows = rate_limits
        .iter()
        .filter_map(|(key, value)| {
            let value = value.as_object()?;
            let used = value.get("usedPercent")?.as_f64()?;
            let resets_at = value.get("resetsAt").and_then(|value| value.as_i64());
            let minutes = value
                .get("windowDurationMins")
                .and_then(|value| value.as_i64());
            let label = match minutes {
                Some(300) => "5 小时".to_string(),
                Some(10_080) => "7 天".to_string(),
                Some(value) if value > 0 && value % 1_440 == 0 => {
                    format!("{} 天", value / 1_440)
                }
                Some(value) if value > 0 && value % 60 == 0 => {
                    format!("{} 小时", value / 60)
                }
                Some(value) if value > 0 => format!("{value} 分钟"),
                _ => key.to_string(),
            };
            Some(RateWindow {
                label,
                used_percent: Some(used),
                remaining_percent: Some((100.0 - used).clamp(0.0, 100.0)),
                resets_at,
                window_minutes: minutes,
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by_key(|window| window.window_minutes.unwrap_or(i64::MAX));
    if windows.is_empty() {
        return Err("empty_snapshot");
    }
    Ok(windows)
}

fn resolve_codex_executable() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(value) = std::env::var_os("CODEX_EXE") {
        candidates.push(PathBuf::from(value));
    }
    candidates.push(PathBuf::from(
        r"D:\nvm4w\nodejs\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
    ));
    candidates.push(PathBuf::from(r"D:\nvm4w\nodejs\codex.cmd"));
    if let Some(root) = std::env::var_os("APPDATA").map(PathBuf::from) {
        candidates.push(
            root.join("npm/node_modules/@openai/codex/node_modules/@openai/codex-win32-x64")
                .join("vendor/x86_64-pc-windows-msvc/bin/codex.exe"),
        );
        candidates.push(root.join("npm/codex.cmd"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join("codex.exe"));
            candidates.push(directory.join("codex.cmd"));
        }
    }
    first_existing_path(candidates).unwrap_or_else(|| PathBuf::from("codex.cmd"))
}

fn read_rate_limits_uncached() -> (Vec<RateWindow>, String) {
    let executable = resolve_codex_executable();

    let mut command = if executable.extension().and_then(|value| value.to_str()) == Some("cmd") {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c"]).arg(executable);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    } else {
        Command::new(executable)
    };
    let mut child = match command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (vec![], "unavailable".into()),
    };
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    };
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    };
    let (messages_tx, messages_rx) = mpsc::channel::<serde_json::Value>();
    thread::spawn(move || {
        let reader = BufReader::new(output);
        for line in reader.lines().flatten() {
            if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                if messages_tx.send(message).is_err() {
                    break;
                }
            }
        }
    });

    if writeln!(
        input,
        "{}",
        r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-float","title":"Codex Float","version":"0.1.0"}}}"#
    )
    .and_then(|_| input.flush())
    .is_err()
    {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }
    let initialized = recv_response(&messages_rx, 1, Duration::from_secs(4))
        .is_some_and(|message| message.get("error").is_none());
    if !initialized {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }

    if writeln!(input, r#"{{"method":"initialized"}}"#).is_err()
        || writeln!(input, r#"{{"id":2,"method":"account/rateLimits/read"}}"#).is_err()
        || input.flush().is_err()
    {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }

    let response = recv_response(&messages_rx, 2, Duration::from_secs(8));
    let _ = child.kill();
    let Some(response) = response else {
        return (vec![], "unavailable".into());
    };
    match parse_rate_limits_response(&response) {
        Ok(windows) => (windows, "live".into()),
        Err(_) => (vec![], "unavailable".into()),
    }
}
fn read_account_usage_uncached() -> Option<Vec<DailyPoint>> {
    let executable = resolve_codex_executable();
    let mut command = if executable.extension().and_then(|v| v.to_str()) == Some("cmd") {
        let mut c = Command::new("cmd.exe");
        c.args(["/d", "/c"]).arg(executable);
        c
    } else {
        Command::new(executable)
    };
    let mut child = command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let output = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<serde_json::Value>();
    thread::spawn(move || {
        for line in BufReader::new(output).lines().flatten() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    writeln!(input, r#"{{"id":1,"method":"initialize","params":{{"clientInfo":{{"name":"codex-float","title":"Codex Float","version":"0.1.0"}}}}}}"#).ok()?;
    input.flush().ok()?;
    if recv_response(&rx, 1, Duration::from_secs(5)).is_none() {
        let _ = child.kill();
        return None;
    }
    writeln!(input, r#"{{"method":"initialized"}}"#).ok()?;
    writeln!(input, r#"{{"id":2,"method":"account/usage/read"}}"#).ok()?;
    input.flush().ok()?;
    let response = recv_response(&rx, 2, Duration::from_secs(15))?;
    let buckets = response
        .get("result")?
        .get("dailyUsageBuckets")?
        .as_array()?;
    let mut daily = Vec::new();
    for bucket in buckets {
        let day = bucket
            .get("startDate")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let tokens = bucket.get("tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        if !day.is_empty() {
            daily.push(DailyPoint {
                day: day.to_string(),
                tokens,
            });
        }
    }
    let _ = child.kill();
    (!daily.is_empty()).then_some(daily)
}

fn cached_account_usage(
    cache: &Mutex<Option<AccountUsageCache>>,
    mode: RefreshMode,
    request_started: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<Vec<DailyPoint>> {
    let slot = cache.lock().ok()?;
    let cached = slot.as_ref()?;
    let use_cached = match mode {
        RefreshMode::Cached => {
            let age = (now - cached.fetched_at).num_seconds();
            (0..ACCOUNT_USAGE_CACHE_MAX_AGE_SECS).contains(&age)
        }
        RefreshMode::Force => cached.fetched_at >= request_started,
    };
    use_cached.then(|| cached.daily.clone())
}

fn read_account_usage_with<F>(
    cache: &Mutex<Option<AccountUsageCache>>,
    refresh: &Mutex<()>,
    mode: RefreshMode,
    fetch: F,
) -> Option<Vec<DailyPoint>>
where
    F: FnOnce() -> Option<Vec<DailyPoint>>,
{
    let request_started = Local::now();
    if let Some(daily) = cached_account_usage(cache, mode, request_started, Local::now()) {
        return Some(daily);
    }
    let _refresh_guard = refresh.lock().ok()?;
    if let Some(daily) = cached_account_usage(cache, mode, request_started, Local::now()) {
        return Some(daily);
    }
    let daily = fetch()?;
    if daily.is_empty() {
        return None;
    }
    if let Ok(mut slot) = cache.lock() {
        *slot = Some(AccountUsageCache {
            daily: daily.clone(),
            fetched_at: Local::now(),
        });
    }
    Some(daily)
}

fn read_account_usage(
    cache: &Mutex<Option<AccountUsageCache>>,
    refresh: &Mutex<()>,
    mode: RefreshMode,
) -> Option<Vec<DailyPoint>> {
    read_account_usage_with(cache, refresh, mode, read_account_usage_uncached)
}

fn usage_totals(daily: &[DailyPoint], today: chrono::NaiveDate) -> (i64, i64, i64, bool) {
    let today_key = today.to_string();
    let today_available = daily.iter().any(|point| point.day == today_key);
    let today_value = daily
        .iter()
        .filter(|p| p.day == today_key)
        .map(|p| p.tokens)
        .sum();
    let since7 = (today - chrono::Days::new(6)).to_string();
    let since30 = (today - chrono::Days::new(29)).to_string();
    let last7 = daily
        .iter()
        .filter(|p| p.day >= since7 && p.day <= today_key)
        .map(|p| p.tokens)
        .sum();
    let last30 = daily
        .iter()
        .filter(|p| p.day >= since30 && p.day <= today_key)
        .map(|p| p.tokens)
        .sum();
    (today_value, last7, last30, today_available)
}

fn account_history_with_local_today(
    account_daily: Vec<DailyPoint>,
    local_today: i64,
    today: chrono::NaiveDate,
) -> Vec<DailyPoint> {
    let today_key = today.to_string();
    let mut merged: Vec<DailyPoint> = account_daily
        .into_iter()
        .filter(|point| point.day.as_str() < today_key.as_str())
        .collect();
    merged.push(DailyPoint {
        day: today_key,
        tokens: local_today,
    });
    merged.sort_by(|left, right| left.day.cmp(&right.day));
    merged
}

fn select_token_snapshot(
    account_daily: Option<Vec<DailyPoint>>,
    local: LocalTokenUsage,
    today: chrono::NaiveDate,
) -> TokenSnapshot {
    if let Some(daily) = account_daily.filter(|daily| !daily.is_empty()) {
        let (today_tokens, last7, last30, today_available) = usage_totals(&daily, today);
        if local.coverage != "unavailable" {
            let daily = account_history_with_local_today(daily, local.today, today);
            let (today, last7, last30, _) = usage_totals(&daily, today);
            return TokenSnapshot {
                today,
                last7,
                last30,
                daily,
                token_coverage: "account_history_local_today".into(),
                usage_source: "account_history_local_today".into(),
                today_available: true,
            };
        }
        return TokenSnapshot {
            today: today_available.then_some(today_tokens).unwrap_or(0),
            last7,
            last30,
            daily,
            token_coverage: "account".into(),
            usage_source: if today_available {
                "account".into()
            } else {
                "account_history_today_unavailable".into()
            },
            today_available,
        };
    }
    let today_available = local.coverage != "unavailable";
    TokenSnapshot {
        today: local.today,
        last7: local.last7,
        last30: local.last30,
        daily: local.daily,
        token_coverage: local.coverage,
        usage_source: "local".into(),
        today_available,
    }
}

const RATE_REFRESH_INTERVAL_SECS: i64 = 120;
const RATE_LIVE_MAX_AGE_SECS: i64 = 180;
const RATE_CACHE_MAX_AGE_SECS: i64 = 15 * 60;

fn rate_retry_delay_secs(failures: u32) -> i64 {
    match failures {
        0 => 0,
        1 => 5,
        2 => 15,
        _ => 30,
    }
}

fn rate_cache_status(cache: Option<&RateCache>, now: DateTime<Local>) -> Option<&'static str> {
    let cache = cache?;
    if cache.windows.is_empty() {
        return None;
    }
    let age = (now - cache.fetched_at).num_seconds();
    if age < 0 || age > RATE_CACHE_MAX_AGE_SECS {
        return None;
    }
    if age < RATE_LIVE_MAX_AGE_SECS {
        Some("live")
    } else {
        Some("stale")
    }
}

fn discard_expired_rate_cache(
    state: &Mutex<RateState>,
    db: &Mutex<Connection>,
    now: DateTime<Local>,
) {
    let expired = if let Ok(mut state) = state.lock() {
        let expired = state
            .cache
            .as_ref()
            .is_some_and(|cache| rate_cache_status(Some(cache), now).is_none());
        if expired {
            state.cache = None;
        }
        expired
    } else {
        false
    };
    if expired {
        if let Ok(db) = db.lock() {
            let _ = db.execute("DELETE FROM quota_cache WHERE id=1", []);
        }
    }
}

fn cached_rate_limits(
    cache: Option<&RateCache>,
    now: DateTime<Local>,
) -> Option<(Vec<RateWindow>, String)> {
    let cached = cache?;
    rate_cache_status(Some(cached), now)?;
    Some((cached.windows.clone(), cached.fetched_at.to_rfc3339()))
}

fn cached_rate_response(
    state: &RateState,
    now: DateTime<Local>,
) -> Option<(Vec<RateWindow>, String, String, String)> {
    let (windows, fetched_at) = cached_rate_limits(state.cache.as_ref(), now)?;
    let status = rate_cache_status(state.cache.as_ref(), now)?;
    Some((windows, status.into(), status.into(), fetched_at))
}

fn rate_unavailable() -> (Vec<RateWindow>, String, String, String) {
    (
        vec![],
        "unavailable".into(),
        "unavailable".into(),
        String::new(),
    )
}

fn retry_is_due(retry: &RateRetry, now: DateTime<Local>) -> bool {
    let delay = rate_retry_delay_secs(retry.consecutive_failures);
    if delay == 0 {
        return true;
    }
    retry
        .last_attempt
        .map(|last| (now - last).num_seconds() >= delay)
        .unwrap_or(true)
}

fn read_rate_limits_with<F>(
    state: &Mutex<RateState>,
    refresh: &Mutex<()>,
    db: &Mutex<Connection>,
    mode: RefreshMode,
    fetch: F,
) -> (Vec<RateWindow>, String, String, String)
where
    F: FnOnce() -> (Vec<RateWindow>, String),
{
    let request_started = Local::now();
    discard_expired_rate_cache(state, db, request_started);
    if matches!(mode, RefreshMode::Cached) {
        if let Ok(state) = state.lock() {
            if let Some(cache) = state.cache.as_ref() {
                let age = (request_started - cache.fetched_at).num_seconds();
                if (0..RATE_REFRESH_INTERVAL_SECS).contains(&age) {
                    if let Some(snapshot) = cached_rate_response(&state, request_started) {
                        return snapshot;
                    }
                }
            }
            if !retry_is_due(&state.retry, request_started) {
                return cached_rate_response(&state, request_started)
                    .unwrap_or_else(rate_unavailable);
            }
        }
    }

    let Ok(_refresh_guard) = refresh.lock() else {
        return rate_unavailable();
    };
    let attempt_at = Local::now();
    if let Ok(mut state) = state.lock() {
        match mode {
            RefreshMode::Cached => {
                if let Some(cache) = state.cache.as_ref() {
                    let age = (attempt_at - cache.fetched_at).num_seconds();
                    if (0..RATE_REFRESH_INTERVAL_SECS).contains(&age) {
                        if let Some(snapshot) = cached_rate_response(&state, attempt_at) {
                            return snapshot;
                        }
                    }
                }
                if !retry_is_due(&state.retry, attempt_at) {
                    return cached_rate_response(&state, attempt_at)
                        .unwrap_or_else(rate_unavailable);
                }
            }
            RefreshMode::Force => {
                if state
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.fetched_at >= request_started)
                {
                    if let Some(snapshot) = cached_rate_response(&state, attempt_at) {
                        return snapshot;
                    }
                }
            }
        }
        state.retry.last_attempt = Some(attempt_at);
    } else {
        return rate_unavailable();
    }

    let (windows, status) = fetch();
    let fetched_at = Local::now();
    if status == "live" && !windows.is_empty() {
        let cache = RateCache {
            windows: windows.clone(),
            fetched_at,
        };
        if let Ok(mut state) = state.lock() {
            state.cache = Some(cache);
            state.retry = RateRetry {
                last_attempt: Some(fetched_at),
                consecutive_failures: 0,
            };
        }
        if let Ok(db) = db.lock() {
            let _ = save_rate_cache(
                &db,
                &RateCache {
                    windows: windows.clone(),
                    fetched_at,
                },
            );
        }
        return (
            windows,
            "live".into(),
            "live".into(),
            fetched_at.to_rfc3339(),
        );
    }

    let now = Local::now();
    if let Ok(mut state) = state.lock() {
        state.retry.consecutive_failures = state.retry.consecutive_failures.saturating_add(1);
        state.retry.last_attempt = Some(now);
        return cached_rate_response(&state, now).unwrap_or_else(rate_unavailable);
    }
    rate_unavailable()
}

fn read_rate_limits(
    state: &Mutex<RateState>,
    refresh: &Mutex<()>,
    db: &Mutex<Connection>,
    mode: RefreshMode,
) -> (Vec<RateWindow>, String, String, String) {
    read_rate_limits_with(state, refresh, db, mode, read_rate_limits_uncached)
}

fn dashboard_snapshot(state: &Store, mode: RefreshMode) -> Snapshot {
    let sessions = sessions_path();
    let sessions_exist = sessions.is_dir();
    let local_usage = {
        let db = state.db.lock().unwrap();
        let scan_succeeded = sessions_exist && scan_sessions(&db, &sessions).is_ok();
        let coverage = token_coverage(sessions_exist, scan_succeeded);
        if coverage == "unavailable" {
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: coverage.into(),
            }
        } else {
            LocalTokenUsage {
                today: sums(&db, 1).unwrap_or(0),
                last7: sums(&db, 7).unwrap_or(0),
                last30: sums(&db, 30).unwrap_or(0),
                daily: series(&db).unwrap_or_default(),
                coverage: coverage.into(),
            }
        }
    };
    let tokens = select_token_snapshot(
        read_account_usage(
            &state.account_usage_cache,
            &state.account_usage_refresh,
            mode,
        ),
        local_usage,
        Local::now().date_naive(),
    );
    let expanded = *state.expanded.lock().unwrap();
    let (windows, status, freshness, rate_updated_at) =
        read_rate_limits(&state.rate_state, &state.rate_refresh, &state.db, mode);
    Snapshot {
        expanded,
        today: tokens.today,
        last7: tokens.last7,
        last30: tokens.last30,
        daily: tokens.daily,
        windows,
        account: query_account_status(&state.account_cache, &state.account_refresh, mode),
        rate_status: status,
        rate_freshness: freshness,
        token_coverage: tokens.token_coverage,
        usage_source: tokens.usage_source,
        today_available: tokens.today_available,
        updated_at: if rate_updated_at.is_empty() {
            Local::now().to_rfc3339()
        } else {
            rate_updated_at
        },
    }
}

#[tauri::command]
fn get_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    dashboard_snapshot(&state, RefreshMode::Cached)
}

#[tauri::command]
fn refresh_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    dashboard_snapshot(&state, RefreshMode::Force)
}
#[tauri::command]
fn clear_token_history(state: tauri::State<Store>) -> Result<(), String> {
    let db = state.db.lock().map_err(|_| "database lock")?;
    clear_token_data(&db).map_err(|e| e.to_string())
}
#[tauri::command]
fn save_window_position(state: tauri::State<Store>, x: i32, y: i32) -> Result<(), String> {
    let db = state.db.lock().map_err(|_| "database lock")?;
    let expanded = *state.expanded.lock().map_err(|_| "lock")?;
    db.execute("INSERT INTO window_state(id,x,y,expanded) VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET x=excluded.x,y=excluded.y,expanded=excluded.expanded", rusqlite::params![x, y, expanded as i64])
        .map_err(|e| e.to_string())?;
    Ok(())
}
#[tauri::command]
fn set_window_expanded(
    app: tauri::AppHandle,
    state: tauri::State<Store>,
    expanded: bool,
) -> Result<(), String> {
    *state.expanded.lock().map_err(|_| "lock")? = expanded;
    {
        let db = state.db.lock().map_err(|_| "database lock")?;
        db.execute("INSERT INTO window_state(id,x,y,expanded) VALUES(1,0,0,?) ON CONFLICT(id) DO UPDATE SET expanded=excluded.expanded", [expanded as i64])
            .map_err(|e| e.to_string())?;
    }
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("float") {
        w.set_size(tauri::LogicalSize::new(
            if expanded { 320.0 } else { 250.0 },
            if expanded { 280.0 } else { 64.0 },
        ))
        .map_err(|e| e.to_string())?;
        if let (Some(monitor), Ok(position), Ok(size)) = (
            w.current_monitor().map_err(|e| e.to_string())?,
            w.outer_position(),
            w.outer_size(),
        ) {
            let area = monitor.work_area();
            let min_x = area.position.x;
            let min_y = area.position.y;
            let max_x = (area.position.x + area.size.width as i32 - size.width as i32).max(min_x);
            let max_y = (area.position.y + area.size.height as i32 - size.height as i32).max(min_y);
            let x = position.x.clamp(min_x, max_x);
            let y = position.y.clamp(min_y, max_y);
            if x != position.x || y != position.y {
                w.set_position(tauri::PhysicalPosition::new(x, y))
                    .map_err(|e| e.to_string())?;
            }
            let db = state.db.lock().map_err(|_| "database lock")?;
            db.execute(
                "INSERT INTO window_state(id,x,y,expanded) VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET x=excluded.x,y=excluded.y,expanded=excluded.expanded",
                rusqlite::params![x, y, expanded as i64],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
#[cfg(windows)]
fn start_fullscreen_monitor(app: tauri::AppHandle) {
    use std::{thread, time::Duration};
    use tauri::Manager;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    thread::spawn(move || {
        let own_pid = unsafe { GetCurrentProcessId() };
        let mut hidden = false;
        loop {
            let foreground = unsafe { GetForegroundWindow() };
            let mut pid = 0;
            let foreground_pid = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
            let mut fullscreen = false;
            if foreground_pid != 0
                && pid != own_pid
                && unsafe { IsWindowVisible(foreground).as_bool() }
            {
                let monitor = unsafe { MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST) };
                if !monitor.is_invalid() {
                    let mut info = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        rcMonitor: RECT::default(),
                        rcWork: RECT::default(),
                        dwFlags: 0,
                    };
                    let mut window_rect = RECT::default();
                    let got_monitor = unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() };
                    let got_window = unsafe { GetWindowRect(foreground, &mut window_rect).is_ok() };
                    fullscreen = got_monitor && got_window && window_rect == info.rcMonitor;
                }
            }
            if let Some(window) = app.get_webview_window("float") {
                let user_hidden = app
                    .state::<Store>()
                    .user_hidden
                    .lock()
                    .map(|value| *value)
                    .unwrap_or(true);
                let (next_hidden, action) =
                    fullscreen_visibility_transition(fullscreen, user_hidden, hidden);
                hidden = next_hidden;
                match action {
                    VisibilityAction::Hide => {
                        let _ = window.hide();
                    }
                    VisibilityAction::Show => {
                        let _ = window.show();
                    }
                    VisibilityAction::None => {}
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}
#[cfg(not(windows))]
fn start_fullscreen_monitor(_app: tauri::AppHandle) {}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let db = data.join("CodexFloat").join("codex-float.sqlite");
    let _ = fs::create_dir_all(db.parent().unwrap());
    let store = Store::new(&db).expect("database");
    tauri::Builder::default()
        .manage(store)
        .setup(|app| {
            use tauri::{Emitter, Manager};
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_shortcuts(["ctrl+shift+space"])?
                        .with_handler(|app, shortcut, event| {
                            if event.state == ShortcutState::Pressed
                                && shortcut
                                    .matches(Modifiers::CONTROL | Modifiers::SHIFT, Code::Space)
                            {
                                let state = app.state::<Store>();
                                let next = state.click_through.lock().ok().map(|mut enabled| {
                                    *enabled = !*enabled;
                                    *enabled
                                });
                                if let Some(enabled) = next {
                                    if let Some(window) = app.get_webview_window("float") {
                                        let _ = window.set_ignore_cursor_events(enabled);
                                    }
                                    let _ = app.emit("click-through-changed", enabled);
                                }
                            }
                        })
                        .build(),
                )?;
            }
            start_fullscreen_monitor(app.handle().clone());
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;
                let show_main =
                    MenuItem::with_id(app, "show-main", "打开主窗口", true, None::<&str>)?;
                let show_float =
                    MenuItem::with_id(app, "show-float", "显示悬浮窗", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "退出 Codex Float", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_main, &show_float, &quit])?;
                TrayIconBuilder::new()
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show-main" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "show-float" => {
                            let state = app.state::<Store>();
                            if let Ok(mut hidden) = state.user_hidden.lock() {
                                *hidden = false;
                            }
                            if let Ok(mut enabled) = state.click_through.lock() {
                                *enabled = false;
                            }
                            if let Some(window) = app.get_webview_window("float") {
                                let _ = window.set_ignore_cursor_events(false);
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            let _ = app.emit("click-through-changed", false);
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(app)?;
            }
            let state = app.state::<Store>();
            let saved = state.db.lock().ok().and_then(|db| {
                db.query_row(
                    "SELECT x,y,expanded FROM window_state WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i32>(0)?,
                            row.get::<_, i32>(1)?,
                            row.get::<_, i64>(2)? != 0,
                        ))
                    },
                )
                .ok()
            });
            if let Some(window) = app.get_webview_window("float") {
                let expanded = saved.as_ref().map(|value| value.2).unwrap_or(false);
                if expanded {
                    window.set_size(tauri::LogicalSize::new(320.0, 300.0))?;
                }
                let valid_saved_position = saved.as_ref().is_some_and(|(x, y, _)| {
                    app.available_monitors().ok().is_some_and(|monitors| {
                        monitors.iter().any(|monitor| {
                            let area = monitor.work_area();
                            *x >= area.position.x
                                && *y >= area.position.y
                                && *x < area.position.x + area.size.width as i32
                                && *y < area.position.y + area.size.height as i32
                        })
                    })
                });
                if valid_saved_position {
                    let (x, y, _) = saved.unwrap();
                    window.set_position(tauri::PhysicalPosition::new(x, y))?;
                } else if let Some(monitor) = window.current_monitor()? {
                    let work_area = monitor.work_area();
                    let size = window.outer_size()?;
                    let margin = (16.0 * monitor.scale_factor()).round() as i32;
                    let x = work_area.position.x + work_area.size.width as i32
                        - size.width as i32
                        - margin;
                    let y = work_area.position.y + work_area.size.height as i32
                        - size.height as i32
                        - margin;
                    window.set_position(tauri::PhysicalPosition::new(x, y))?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "float" {
                    use tauri::Manager;
                    let state = window.app_handle().state::<Store>();
                    if let Ok(mut hidden) = state.user_hidden.lock() {
                        *hidden = true;
                    };
                }
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard_snapshot,
            refresh_dashboard_snapshot,
            set_window_expanded,
            save_window_position,
            clear_token_history
        ])
        .run(tauri::generate_context!())
        .expect("error running app");
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn first_existing_path_prefers_the_first_real_candidate() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.cmd");
        let first = dir.path().join("first.cmd");
        let second = dir.path().join("second.exe");
        std::fs::write(&first, "").unwrap();
        std::fs::write(&second, "").unwrap();
        assert_eq!(
            first_existing_path(vec![missing, first.clone(), second]),
            Some(first)
        );
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }
    fn line(total: i64, inc: i64) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }
    fn line_at(total: i64, inc: i64, timestamp: &str) -> String {
        serde_json::json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }

    fn live_window() -> RateWindow {
        RateWindow {
            label: "5 小时".into(),
            used_percent: Some(20.0),
            remaining_percent: Some(80.0),
            resets_at: None,
            window_minutes: Some(300),
        }
    }

    fn point(day: &str, tokens: i64) -> DailyPoint {
        DailyPoint {
            day: day.into(),
            tokens,
        }
    }
    #[test]
    fn aggregation_uses_cumulative_total_not_last_snapshot_field() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        let event_day = Local::now().date_naive().to_string();
        fs::write(
            &p,
            format!(
                "{}\n{}\n",
                line_at(100, 100, &format!("{event_day}T10:00:00Z")),
                line_at(150, 999, &format!("{event_day}T10:01:00Z"))
            ),
        )
        .unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 30).unwrap(), 150);
    }
    #[test]
    fn duplicate_snapshot_is_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n{}\n", line(10, 10), line(10, 0))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 10)
    }
    #[test]
    fn counter_reset_uses_new_total() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::write(&p, format!("{}\n{}\n", line(100, 100), line(3, 3))).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 103)
    }
    #[test]
    fn second_scan_and_append_are_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(4, 4))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(format!("{}\n", line(9, 5)).as_bytes())
            .unwrap();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 9)
    }
    #[test]
    fn event_timestamp_controls_the_daily_bucket() {
        let d = tempdir().unwrap();
        let p = d.path().join("private-session.jsonl");
        let timestamp = "2026-08-10T23:30:00Z";
        fs::write(&p, format!("{}\n", line_at(12, 12, timestamp))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        let expected_day = DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Local)
            .date_naive()
            .to_string();
        let (stored_day, tokens): (String, i64) = c
            .query_row("SELECT day,tokens FROM daily", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((stored_day, tokens), (expected_day, 12));
        let stored_file_id: String = c
            .query_row("SELECT file_id FROM cursors", [], |row| row.get(0))
            .unwrap();
        assert!(!stored_file_id.contains("private-session"));
    }

    #[test]
    fn token_coverage_reports_unavailable_sources() {
        assert_eq!(token_coverage(false, true), "unavailable");
        assert_eq!(token_coverage(true, false), "unavailable");
        assert_eq!(token_coverage(true, true), "partial");
    }

    #[test]
    fn cleanup_removes_only_data_older_than_thirty_days() {
        let c = db();
        c.execute("INSERT INTO daily(day,tokens) VALUES('2000-01-01',10)", [])
            .unwrap();
        c.execute(
            "INSERT INTO daily(day,tokens) VALUES(?,20)",
            [Local::now().date_naive().to_string()],
        )
        .unwrap();
        cleanup_old_daily(&c, 30).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn recv_response_filters_by_id() {
        let (sender, receiver) = mpsc::channel();
        sender.send(serde_json::json!({"id": 99})).unwrap();
        sender
            .send(serde_json::json!({"id": 2, "result": {}}))
            .unwrap();
        let response = recv_response(&receiver, 2, Duration::from_millis(50)).unwrap();
        assert_eq!(response.get("id").and_then(|value| value.as_i64()), Some(2));
    }

    #[test]
    fn recv_response_times_out_without_matching_id() {
        let (_sender, receiver) = mpsc::channel();
        assert!(recv_response(&receiver, 2, Duration::from_millis(10)).is_none());
    }

    #[test]
    fn cache_is_available_until_fifteen_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS - 1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_some());
    }

    #[test]
    fn cache_expires_after_fifteen_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS + 1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_none());
    }

    #[test]
    fn rate_cache_status_changes_after_three_and_fifteen_minutes() {
        let now = Local::now();
        let cache_with_age = |seconds| RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(seconds),
        };
        assert_eq!(
            rate_cache_status(Some(&cache_with_age(0)), now),
            Some("live")
        );
        assert_eq!(
            rate_cache_status(Some(&cache_with_age(180)), now),
            Some("stale")
        );
        assert_eq!(rate_cache_status(Some(&cache_with_age(901)), now), None);
    }

    #[test]
    fn rate_limits_parser_rejects_protocol_errors_and_empty_snapshots() {
        assert!(parse_rate_limits_response(
            &serde_json::json!({"id":2,"error":{"code":"unsupported"}})
        )
        .is_err());
        assert!(parse_rate_limits_response(
            &serde_json::json!({"id":2,"result":{"rateLimits":{}}})
        )
        .is_err());
    }

    #[test]
    fn rate_limits_parser_keeps_all_windows_and_sorts_by_duration() {
        let response = serde_json::json!({
            "id": 2,
            "result": {"rateLimits": {
                "primary": {"usedPercent": 50.0, "windowDurationMins": 300},
                "secondary": {"usedPercent": 10.0, "windowDurationMins": 10080},
                "tertiary": {"usedPercent": 80.0, "windowDurationMins": 60}
            }}
        });
        let windows = parse_rate_limits_response(&response).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.window_minutes)
                .collect::<Vec<_>>(),
            vec![Some(60), Some(300), Some(10080)]
        );
    }

    #[test]
    fn rate_retry_delay_uses_five_fifteen_then_thirty_seconds() {
        assert_eq!(rate_retry_delay_secs(0), 0);
        assert_eq!(rate_retry_delay_secs(1), 5);
        assert_eq!(rate_retry_delay_secs(2), 15);
        assert_eq!(rate_retry_delay_secs(3), 30);
        assert_eq!(rate_retry_delay_secs(4), 30);
    }

    #[test]
    fn normalized_rate_cache_survives_database_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        let cache = RateCache {
            windows: vec![RateWindow {
                label: "5 小时".into(),
                used_percent: Some(25.0),
                remaining_percent: Some(75.0),
                resets_at: Some(1_900_000_000),
                window_minutes: Some(300),
            }],
            fetched_at: Local::now(),
        };
        {
            let c = Connection::open(&path).unwrap();
            init(&c).unwrap();
            save_rate_cache(&c, &cache).unwrap();
        }
        let c = Connection::open(&path).unwrap();
        init(&c).unwrap();
        let loaded = load_rate_cache(&c).unwrap();
        assert_eq!(loaded.windows.len(), 1);
        assert_eq!(loaded.windows[0].remaining_percent, Some(75.0));
        assert_eq!(loaded.fetched_at.timestamp(), cache.fetched_at.timestamp());
    }

    #[test]
    fn expired_rate_cache_is_removed_from_memory_and_database() {
        let now = Local::now();
        let cache = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS + 1),
        };
        let c = db();
        save_rate_cache(&c, &cache).unwrap();
        let state = Mutex::new(RateState {
            cache: Some(cache),
            retry: RateRetry::default(),
        });
        let db = Mutex::new(c);

        discard_expired_rate_cache(&state, &db, now);

        assert!(state.lock().unwrap().cache.is_none());
        let remaining: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM quota_cache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn concurrent_forced_rate_refreshes_share_one_provider_call() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let state = Arc::new(Mutex::new(RateState {
            cache: None,
            retry: RateRetry::default(),
        }));
        let refresh = Arc::new(Mutex::new(()));
        let db = Arc::new(Mutex::new(db()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let state = Arc::clone(&state);
            let refresh = Arc::clone(&refresh);
            let db = Arc::clone(&db);
            let calls = Arc::clone(&calls);
            workers.push(std::thread::spawn(move || {
                read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    (vec![live_window()], "live".into())
                })
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap().1, "live");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_rate_refresh_ignores_a_fresh_cache() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Mutex::new(RateState {
            cache: Some(RateCache {
                windows: vec![live_window()],
                fetched_at: Local::now(),
            }),
            retry: RateRetry::default(),
        });
        let refresh = Mutex::new(());
        let db = Mutex::new(db());
        let calls = AtomicUsize::new(0);

        let (_, status, _, _) =
            read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                (vec![live_window()], "live".into())
            });

        assert_eq!(status, "live");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_rate_refresh_bypasses_retry_delay() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Mutex::new(RateState {
            cache: None,
            retry: RateRetry {
                last_attempt: Some(Local::now()),
                consecutive_failures: 3,
            },
        });
        let refresh = Mutex::new(());
        let db = Mutex::new(db());
        let calls = AtomicUsize::new(0);

        let (_, status, _, _) =
            read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                (vec![live_window()], "live".into())
            });

        assert_eq!(status, "live");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_account_status_preserves_the_last_success_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let previous = AccountStatus {
            bound: true,
            plan_type: Some("Plus".into()),
            subscription_active_until: Some("2099-01-01T00:00:00Z".into()),
            subscription_state: "active".into(),
            subscription_error: None,
            checked_at: Some("2098-01-01T00:00:00Z".into()),
        };
        let cache = Mutex::new(Some(AccountCache {
            cache_key: "account".into(),
            status: previous,
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let status =
            read_account_status_with(&cache, &refresh, "account", RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("network unavailable".into())
            });

        assert!(status.bound);
        assert_eq!(status.plan_type.as_deref(), Some("Plus"));
        assert_eq!(
            status.subscription_error.as_deref(),
            Some("network unavailable")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_account_status_reuses_a_fresh_value() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountCache {
            cache_key: "account".into(),
            status: AccountStatus {
                bound: true,
                plan_type: Some("Plus".into()),
                subscription_active_until: None,
                subscription_state: "unknown".into(),
                subscription_error: None,
                checked_at: None,
            },
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let status =
            read_account_status_with(&cache, &refresh, "account", RefreshMode::Cached, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(AccountStatus::default())
            });

        assert_eq!(status.plan_type.as_deref(), Some("Plus"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn forced_account_usage_ignores_a_fresh_cache() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountUsageCache {
            daily: vec![point("2026-10-09", 10)],
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let daily = read_account_usage_with(&cache, &refresh, RefreshMode::Force, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(vec![point("2026-10-09", 20)])
        })
        .unwrap();

        assert_eq!(daily[0].tokens, 20);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_account_usage_reuses_a_fresh_value() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountUsageCache {
            daily: vec![point("2026-10-09", 10)],
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let daily = read_account_usage_with(&cache, &refresh, RefreshMode::Cached, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(vec![point("2026-10-09", 20)])
        })
        .unwrap();

        assert_eq!(daily[0].tokens, 10);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_forced_account_usage_refreshes_share_one_provider_call() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let cache = Arc::new(Mutex::new(None));
        let refresh = Arc::new(Mutex::new(()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let refresh = Arc::clone(&refresh);
            let calls = Arc::clone(&calls);
            workers.push(std::thread::spawn(move || {
                read_account_usage_with(&cache, &refresh, RefreshMode::Force, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    Some(vec![point("2026-10-09", 20)])
                })
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap()[0].tokens, 20);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn account_history_with_local_today_prefers_local_today_and_recalculates_totals() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30), point("2026-10-09", 70)]),
            local,
            today,
        );

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_029);
        assert_eq!(selected.last30, 1_029);
        assert_eq!(selected.daily.last().unwrap().tokens, 999);
        assert_eq!(selected.usage_source, "account_history_local_today");
        assert_eq!(selected.token_coverage, "account_history_local_today");
        assert!(selected.today_available);
    }

    #[test]
    fn account_history_without_today_uses_local_today() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(Some(vec![point("2026-10-08", 30)]), local, today);

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_029);
        assert_eq!(selected.last30, 1_029);
        assert_eq!(selected.usage_source, "account_history_local_today");
        assert_eq!(selected.token_coverage, "account_history_local_today");
        assert!(selected.today_available);
    }

    #[test]
    fn unavailable_local_usage_uses_account_today_when_available() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30), point("2026-10-09", 70)]),
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.today, 70);
        assert_eq!(selected.last7, 100);
        assert_eq!(selected.usage_source, "account");
        assert_eq!(selected.token_coverage, "account");
        assert!(selected.today_available);
    }

    #[test]
    fn account_history_without_local_today_marks_today_unavailable() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30)]),
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.today, 0);
        assert_eq!(selected.last7, 30);
        assert_eq!(selected.usage_source, "account_history_today_unavailable");
        assert_eq!(selected.token_coverage, "account");
        assert!(!selected.today_available);
    }

    #[test]
    fn unavailable_account_usage_falls_back_to_one_local_group() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(None, local, today);

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_999);
        assert_eq!(selected.last30, 2_999);
        assert_eq!(selected.usage_source, "local");
        assert_eq!(selected.token_coverage, "partial");
        assert!(selected.today_available);
    }

    #[test]
    fn unavailable_local_usage_marks_today_unavailable() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            None,
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.usage_source, "local");
        assert_eq!(selected.token_coverage, "unavailable");
        assert!(!selected.today_available);
    }

    #[test]
    fn future_cache_timestamp_is_not_accepted() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now + chrono::Duration::seconds(1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_none());
    }

    #[test]
    fn subscription_state_uses_subscription_date_not_token_expiry() {
        assert_eq!(subscription_state(Some("2099-01-01T00:00:00Z")), "active");
        assert_eq!(subscription_state(Some("2000-01-01T00:00:00Z")), "expired");
        assert_eq!(subscription_state(Some("not-a-date")), "unknown");
        assert_eq!(subscription_state(None), "unknown");
    }

    #[test]
    fn account_status_serialization_never_exposes_email() {
        let value = serde_json::to_value(AccountStatus::default()).unwrap();
        assert!(value.get("email").is_none());
    }

    #[test]
    fn account_claims_keep_plan_and_subscription_date_separate() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "2026-10-01T00:00:00Z",
                "chatgpt_subscription_active_until": "2026-10-01T00:00:00Z"
            }
        });
        assert_eq!(auth_plan_type(&payload), None);
        assert_eq!(
            auth_subscription_active_until(&payload),
            Some("2026-10-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn fullscreen_visibility_preserves_user_visibility_choice() {
        assert_eq!(
            fullscreen_visibility_transition(true, false, false),
            (true, VisibilityAction::Hide)
        );
        assert_eq!(
            fullscreen_visibility_transition(false, true, true),
            (false, VisibilityAction::None)
        );
        assert_eq!(
            fullscreen_visibility_transition(false, false, true),
            (false, VisibilityAction::Show)
        );
        assert_eq!(
            fullscreen_visibility_transition(true, true, false),
            (false, VisibilityAction::None)
        );
    }

    #[test]
    fn jwt_payload_decoder_rejects_malformed_tokens() {
        assert!(decode_jwt_payload("not-a-jwt").is_none());
        assert!(decode_jwt_payload("a.b.c").is_none());
    }

    #[test]
    fn clear_token_data_removes_events_daily_and_cursors() {
        let c = db();
        c.execute("INSERT INTO daily(day,tokens) VALUES('2026-01-01',10)", [])
            .unwrap();
        c.execute(
            "INSERT INTO cursors(file_id,file_hash,offset,last_total) VALUES('id','hash',1,1)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO events(event_id,file_id,event_offset,total_tokens,delta_tokens,day) VALUES('e','id',0,1,1,'2026-01-01')", []).unwrap();
        clear_token_data(&c).unwrap();
        for table in ["daily", "events"] {
            let count: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "table {table} not cleared");
        }
        let cursors: i64 = c
            .query_row("SELECT COUNT(*) FROM cursors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cursors, 1, "scan baselines must survive a statistics clear");
    }

    #[test]
    fn clearing_statistics_only_counts_tokens_added_after_the_clear() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_sessions(&c, dir.path()).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 100);

        clear_token_data(&c).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", line(150, 50)).as_bytes())
            .unwrap();
        scan_sessions(&c, dir.path()).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 50);
    }

    #[test]
    fn session_scan_does_not_retain_data_older_than_thirty_days() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.jsonl");
        let old_day = (Local::now().date_naive() - chrono::Days::new(40)).to_string();
        fs::write(
            &path,
            format!("{}\n", line_at(12, 12, &format!("{old_day}T10:00:00Z"))),
        )
        .unwrap();
        let c = db();

        scan_sessions(&c, dir.path()).unwrap();

        let daily_count: i64 = c
            .query_row("SELECT COUNT(*) FROM daily", [], |row| row.get(0))
            .unwrap();
        let event_count: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(daily_count, 0);
        assert_eq!(event_count, 0);
    }

    #[test]
    fn session_scan_retries_an_incomplete_trailing_json_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let complete = line(20, 20);
        let split = 12;
        fs::write(&path, format!("{}\n{}", line(10, 10), &complete[..split])).unwrap();
        let c = db();
        scan_file(&c, &path).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 10);

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", &complete[split..]).as_bytes())
            .unwrap();
        scan_file(&c, &path).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 20);
    }

    #[test]
    fn session_scan_rebuilds_a_truncated_file_without_double_counting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_file(&c, &path).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 100);

        fs::write(&path, format!("{}\n", line(7, 7))).unwrap();
        scan_file(&c, &path).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 7);
    }
}
