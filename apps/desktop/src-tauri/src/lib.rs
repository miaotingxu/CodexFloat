use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RateWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub expanded: bool,
    pub today: i64,
    pub last7: i64,
    pub last30: i64,
    pub daily: Vec<DailyPoint>,
    pub windows: Vec<RateWindow>,
    pub account: AccountStatus,
    pub rate_status: String,
    pub rate_freshness: String,
    pub token_coverage: String,
    pub usage_source: String,
    pub today_available: bool,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyPoint {
    pub day: String,
    pub tokens: i64,
}
#[derive(Debug, Deserialize)]
struct TokenInfo {
    total_token_usage: Option<TotalUsage>,
    last_token_usage: Option<TotalUsage>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub bound: bool,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub subscription_active_until: Option<String>,
    pub subscription_state: String,
    pub subscription_error: Option<String>,
    pub checked_at: Option<String>,
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

pub struct Store {
    pub db: Mutex<Connection>,
    pub expanded: Mutex<bool>,
    pub click_through: Mutex<bool>,
    rate_cache: Mutex<Option<RateCache>>,
    account_cache: Mutex<Option<AccountCache>>,
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
        Ok(Self {
            db: Mutex::new(c),
            expanded: Mutex::new(expanded),
            click_through: Mutex::new(false),
            rate_cache: Mutex::new(None),
            account_cache: Mutex::new(None),
        })
    }
}
fn init(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS cursors(file_id TEXT PRIMARY KEY,file_hash TEXT NOT NULL,offset INTEGER NOT NULL,last_total INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,file_id TEXT NOT NULL,event_offset INTEGER NOT NULL,total_tokens INTEGER NOT NULL,delta_tokens INTEGER NOT NULL,day TEXT NOT NULL); CREATE TABLE IF NOT EXISTS daily(day TEXT PRIMARY KEY,tokens INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS window_state(id INTEGER PRIMARY KEY CHECK(id=1),x INTEGER NOT NULL,y INTEGER NOT NULL,expanded INTEGER NOT NULL DEFAULT 0);")
}
fn cleanup_old_daily(c: &Connection, keep_days: i64) -> rusqlite::Result<usize> {
    let cutoff = (Local::now().date_naive()
        - chrono::Days::new(keep_days.saturating_sub(1) as u64))
    .to_string();
    c.execute("DELETE FROM daily WHERE day < ?", [cutoff])
}
fn clear_token_data(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("DELETE FROM events; DELETE FROM daily; DELETE FROM cursors;")
}
fn digest(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}
fn file_id(path: &Path) -> String {
    digest(&path.to_string_lossy())
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
    let start = if old_offset <= current_len {
        old_offset
    } else {
        0
    };
    let f =
        fs::File::open(path).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut reader = BufReader::new(f);
    reader
        .seek_relative(start)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut offset = start;
    let mut previous = if start > 0 { last_total } else { 0 };
    let mut count = 0;
    for line in reader.lines() {
        let line = line.map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let bytes = line.as_bytes().len() as i64 + 1;
        let at = offset;
        offset += bytes;
        let Ok(ev) = serde_json::from_str::<EventLine>(&line) else {
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
        let increment = info
            .last_token_usage
            .and_then(|x| x.total_tokens)
            .unwrap_or(0);
        let delta = if total == previous {
            0
        } else if increment > 0 {
            increment
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
    cleanup_old_daily(c, 30)?;
    let mut n = 0;
    if !root.exists() {
        return Ok(0);
    }
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
    walk(c, root, &mut n)?;
    Ok(n)
}

const SUBSCRIPTION_CHECK_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const SUBSCRIPTIONS_URL: &str = "https://chatgpt.com/backend-api/subscriptions";
const ACCOUNT_CACHE_MAX_AGE_SECS: i64 = 30 * 60;

fn auth_file_path() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".codex")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
        .join("auth.json")
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

fn auth_identity() -> Result<(String, String, Option<String>, Option<String>), String> {
    let raw = fs::read_to_string(auth_file_path())
        .map_err(|e| format!("读取 Codex 登录文件失败: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 Codex 登录文件失败: {e}"))?;
    let tokens = value.get("tokens").ok_or("Codex 登录文件没有 tokens")?;
    let access =
        json_string(tokens.get("access_token")).ok_or("Codex 登录文件没有 access_token")?;
    let id_token = json_string(tokens.get("id_token")).unwrap_or_default();
    let payload = decode_jwt_payload(&id_token)
        .or_else(|| decode_jwt_payload(&access))
        .unwrap_or(serde_json::Value::Null);
    let auth = payload.get("https://api.openai.com/auth");
    let email = json_string(payload.get("email"))
        .or_else(|| auth.and_then(|v| json_string(v.get("email"))))
        .unwrap_or_else(|| "Codex 账号".into());
    let account_id = json_string(tokens.get("account_id"))
        .or_else(|| auth.and_then(|v| json_string(v.get("account_id"))))
        .or_else(|| {
            decode_jwt_payload(&access).and_then(|v| {
                v.get("https://api.openai.com/auth")
                    .and_then(|a| json_string(a.get("account_id")))
            })
        });
    let plan = auth.and_then(|v| json_string(v.get("chatgpt_plan_type")));
    let token_date = auth.and_then(|v| json_string(v.get("chatgpt_subscription_active_until")));
    Ok((
        access,
        email,
        account_id.or_else(|| Some(String::new())),
        plan.or(token_date),
    ))
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

fn query_account_status(cache: &Mutex<Option<AccountCache>>) -> AccountStatus {
    let identity = auth_identity();
    let Ok((access, email, account_id, token_plan_or_date)) = identity else {
        return AccountStatus {
            subscription_state: "unknown".into(),
            subscription_error: Some("尚未绑定可用的 Codex OAuth 账号".into()),
            ..Default::default()
        };
    };
    let cache_key = email.clone() + account_id.as_deref().unwrap_or("");
    if let Ok(slot) = cache.lock() {
        if let Some(cached) = slot.as_ref() {
            if (Local::now() - cached.fetched_at).num_seconds() < ACCOUNT_CACHE_MAX_AGE_SECS
                && cached.cache_key == cache_key
            {
                return cached.status.clone();
            }
        }
    }
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return AccountStatus {
                bound: true,
                email: Some(email),
                subscription_state: "unknown".into(),
                subscription_error: Some(error.to_string()),
                ..Default::default()
            }
        }
    };
    let mut status = AccountStatus {
        bound: true,
        email: Some(email.clone()),
        plan_type: None,
        subscription_active_until: None,
        subscription_state: "unknown".into(),
        subscription_error: None,
        checked_at: Some(Local::now().to_rfc3339()),
    };
    let jwt_payload = decode_jwt_payload(&access);
    status.plan_type = jwt_payload
        .as_ref()
        .and_then(|v| v.get("https://api.openai.com/auth"))
        .and_then(|v| json_string(v.get("chatgpt_plan_type")));
    if status.plan_type.is_none() {
        status.plan_type = token_plan_or_date.clone();
    }
    status.subscription_active_until = jwt_payload
        .as_ref()
        .and_then(|v| v.get("https://api.openai.com/auth"))
        .and_then(|v| json_string(v.get("chatgpt_subscription_active_until")))
        .filter(|v| v.contains('T'));
    let remote = subscription_request(
        &client,
        SUBSCRIPTION_CHECK_URL,
        &access,
        account_id.as_deref(),
        Some((
            "timezone_offset_min",
            &((Local::now().offset().local_minus_utc() / 60).to_string()),
        )),
    );
    if let Ok(value) = remote {
        if status.plan_type.is_none() {
            status.plan_type = find_key(&value, &["subscription_plan", "plan_type", "planType"]);
        }
        if status.subscription_active_until.is_none() {
            status.subscription_active_until = find_key(
                &value,
                &["expires_at", "active_until", "subscription_active_until"],
            );
        }
    }
    if status.subscription_active_until.is_none() {
        if let Some(id) = account_id.as_deref().filter(|id| !id.is_empty()) {
            if let Ok(value) = subscription_request(
                &client,
                SUBSCRIPTIONS_URL,
                &access,
                Some(id),
                Some(("account_id", id)),
            ) {
                if status.plan_type.is_none() {
                    status.plan_type = find_key(&value, &["subscription_plan", "plan_type"]);
                }
                status.subscription_active_until = find_key(
                    &value,
                    &["active_until", "expires_at", "subscription_active_until"],
                );
            }
        }
    }
    status.subscription_state = subscription_state(status.subscription_active_until.as_deref());
    if status.subscription_active_until.is_none() {
        status.subscription_error = Some("官方接口未返回订阅到期时间".into());
    }
    if let Ok(mut slot) = cache.lock() {
        *slot = Some(AccountCache {
            cache_key,
            status: status.clone(),
            fetched_at: Local::now(),
        });
    }
    status
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
    let result = response
        .get("result")
        .and_then(|value| value.get("rateLimits"))
        .unwrap_or(&serde_json::Value::Null);
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(value) = result.get(key).and_then(|value| value.as_object()) else {
            continue;
        };
        let used = value.get("usedPercent").and_then(|value| value.as_f64());
        let resets_at = value.get("resetsAt").and_then(|value| value.as_i64());
        let minutes = value
            .get("windowDurationMins")
            .and_then(|value| value.as_i64());
        let label = match minutes {
            Some(300) => "5 小时".to_string(),
            Some(10_080) => "7 天".to_string(),
            Some(value) if value % 1_440 == 0 => format!("{} 天", value / 1_440),
            Some(value) if value % 60 == 0 => format!("{} 小时", value / 60),
            Some(value) => format!("{value} 分钟"),
            None => key.to_string(),
        };
        windows.push(RateWindow {
            label,
            used_percent: used,
            remaining_percent: used.map(|value| (100.0 - value).clamp(0.0, 100.0)),
            resets_at,
            window_minutes: minutes,
        });
    }
    (windows, "live".into())
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

fn usage_totals(daily: &[DailyPoint], local_today: i64) -> (i64, i64, i64) {
    let today = Local::now().date_naive();
    let today_key = today.to_string();
    let today_value = daily
        .iter()
        .find(|p| p.day == today_key)
        .map(|p| p.tokens)
        .unwrap_or(local_today);
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
    (today_value, last7, last30)
}

const RATE_CACHE_MAX_AGE_SECS: i64 = 600;

fn cached_rate_limits(
    cache: Option<&RateCache>,
    now: DateTime<Local>,
) -> Option<(Vec<RateWindow>, String)> {
    let cached = cache?;
    let age = (now - cached.fetched_at).num_seconds();
    if age < 0 || age > RATE_CACHE_MAX_AGE_SECS {
        return None;
    }
    Some((cached.windows.clone(), cached.fetched_at.to_rfc3339()))
}

fn read_rate_limits(cache: &Mutex<Option<RateCache>>) -> (Vec<RateWindow>, String, String, String) {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_rate_limits_uncached());
    });
    if let Ok((windows, status)) = receiver.recv_timeout(Duration::from_secs(15)) {
        if status == "live" {
            let fetched_at = Local::now();
            if let Ok(mut slot) = cache.lock() {
                *slot = Some(RateCache {
                    windows: windows.clone(),
                    fetched_at,
                });
            }
            return (
                windows,
                "live".into(),
                "live".into(),
                fetched_at.to_rfc3339(),
            );
        }
    }
    if let Ok(slot) = cache.lock() {
        if let Some((windows, fetched_at)) = cached_rate_limits(slot.as_ref(), Local::now()) {
            return (windows, "stale".into(), "stale".into(), fetched_at);
        }
    }
    (
        vec![],
        "unavailable".into(),
        "unavailable".into(),
        String::new(),
    )
}
#[tauri::command]
fn get_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default()
        .join(".codex/sessions");
    let (token_coverage, local_today, local_last7, local_last30, local_daily) = {
        let db = state.db.lock().unwrap();
        let coverage = match scan_sessions(&db, &home) {
            Ok(_) => "partial",
            Err(_) => "unavailable",
        };
        (
            coverage,
            sums(&db, 1).unwrap_or(0),
            sums(&db, 7).unwrap_or(0),
            sums(&db, 30).unwrap_or(0),
            series(&db).unwrap_or_default(),
        )
    };
    let account_daily = read_account_usage_uncached();
    let (today, last7, last30, daily, usage_source, token_coverage, today_available) =
        match account_daily {
            Some(daily) => {
                let (today, last7, last30) = usage_totals(&daily, local_today);
                let today_key = Local::now().date_naive().to_string();
                let has_account_today = daily.iter().any(|point| point.day == today_key);
                let source = if has_account_today {
                    "account"
                } else {
                    "account_history_pending_today"
                };
                (
                    today,
                    last7,
                    last30,
                    daily,
                    source.to_string(),
                    "account".to_string(),
                    has_account_today,
                )
            }
            None => (
                local_today,
                local_last7,
                local_last30,
                local_daily,
                "local".to_string(),
                token_coverage.to_string(),
                true,
            ),
        };
    let expanded = *state.expanded.lock().unwrap();
    let (windows, status, freshness, rate_updated_at) = read_rate_limits(&state.rate_cache);
    Snapshot {
        expanded,
        today,
        last7,
        last30,
        daily,
        windows,
        account: query_account_status(&state.account_cache),
        rate_status: status,
        rate_freshness: freshness,
        token_coverage: token_coverage.into(),
        usage_source,
        today_available,
        updated_at: if rate_updated_at.is_empty() {
            Local::now().to_rfc3339()
        } else {
            rate_updated_at
        },
    }
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
                if fullscreen && !hidden {
                    let _ = window.hide();
                    hidden = true;
                } else if !fullscreen && hidden {
                    let _ = window.show();
                    hidden = false;
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
                            if let Some(window) = app.get_webview_window("float") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
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
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard_snapshot,
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
    fn cache_is_stale_until_ten_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_some());
    }

    #[test]
    fn cache_expires_after_ten_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS + 1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_none());
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
        for table in ["daily", "events", "cursors"] {
            let count: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "table {table} not cleared");
        }
    }
}
